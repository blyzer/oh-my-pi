//! Parent-session eval relay for attached environment sessions.

use std::{
	mem,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use omp_core::{FastHashMap, Str};
use omp_env::{EnvClient, EvalParentQueryEvent};
use omp_proto::env::v1::{self as pb, server_frame};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
	runtime::Handle,
	task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use crate::eval::{
	BridgeHostError, BridgeProgressSink, EvalSessionConfig, ParentSessionHost, SessionBridgeHost,
};

/// Capability advertised by an attached session for this relay.
pub const CAPABILITY: &str = omp_env::EVAL_PARENT_RELAY_CAPABILITY;

const MAX_PENDING: usize = 64;

#[derive(Clone)]
pub(crate) struct ConnectionEvalParent {
	relay: Arc<Relay>,
}

struct Relay {
	responses:  Mutex<Option<flume::Sender<pb::ServerFrame>>>,
	next_query: AtomicU64,
	pending:    Mutex<FastHashMap<u64, Pending>>,
}

struct Pending {
	request_id: u64,
	reply:      flume::Sender<RelayEvent>,
}

enum RelayEvent {
	Progress(Value),
	Result(Result<Value, RelayFailure>),
}

#[derive(Clone, Debug)]
enum RelayFailure {
	Unavailable,
	Cancelled,
	Invalid(Str),
	Host(Str),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ParentConfig {
	config:      EvalSessionConfig,
	completion:  bool,
	agent:       bool,
	workpool:    bool,
	concurrency: bool,
	budget:      bool,
}

#[derive(Clone)]
pub(crate) struct OwnedEvalParent {
	relay:      Arc<Relay>,
	request_id: u64,
	owner:      Str,
	config:     ParentConfig,
}

impl ConnectionEvalParent {
	pub(crate) fn new(responses: flume::Sender<pb::ServerFrame>) -> Self {
		Self {
			relay: Arc::new(Relay {
				responses:  Mutex::new(Some(responses)),
				next_query: AtomicU64::new(1),
				pending:    Mutex::new(FastHashMap::default()),
			}),
		}
	}

	pub(crate) async fn fetch_config(
		&self,
		request_id: u64,
		owner: Str,
	) -> Result<OwnedEvalParent, BridgeHostError> {
		let result = self
			.relay
			.request(request_id, owner.clone(), "config", Value::Null, None)
			.await?;
		let config: ParentConfig = serde_json::from_value(result).map_err(|_| {
			BridgeHostError::message("eval parent relay returned invalid configuration")
		})?;
		Ok(OwnedEvalParent { relay: Arc::clone(&self.relay), request_id, owner, config })
	}

	pub(crate) fn answer_progress(&self, request_id: u64, progress: pb::EvalParentProgress) {
		let pending = self
			.relay
			.pending
			.lock()
			.get(&progress.query_id)
			.and_then(|pending| (pending.request_id == request_id).then_some(pending.reply.clone()));
		if let Some(reply) = pending {
			if let Ok(event) = serde_json::from_str(&progress.event_json) {
				let _ = reply.try_send(RelayEvent::Progress(event));
			}
		}
	}

	pub(crate) fn answer_result(&self, request_id: u64, result: pb::EvalParentResult) {
		let pending = self.relay.pending.lock().remove(&result.query_id);
		let Some(pending) = pending.filter(|pending| pending.request_id == request_id) else {
			return;
		};
		let event = match result.body {
			Some(pb::eval_parent_result::Body::ResultJson(json)) => serde_json::from_str(&json)
				.map_or_else(
					|_| {
						RelayEvent::Result(Err(RelayFailure::Invalid(Str::new_static(
							"invalid JSON result",
						))))
					},
					|value| RelayEvent::Result(Ok(value)),
				),
			Some(pb::eval_parent_result::Body::Error(error)) => {
				RelayEvent::Result(Err(match pb::EvalParentErrorCode::try_from(error.code) {
					Ok(pb::EvalParentErrorCode::EvalParentErrorUnavailable) => RelayFailure::Unavailable,
					Ok(pb::EvalParentErrorCode::EvalParentErrorCancelled) => RelayFailure::Cancelled,
					Ok(pb::EvalParentErrorCode::EvalParentErrorInvalid) => {
						RelayFailure::Invalid(Str::from(error.detail))
					},
					Ok(pb::EvalParentErrorCode::EvalParentErrorHost)
					| Ok(pb::EvalParentErrorCode::EvalParentErrorUnspecified)
					| Err(_) => RelayFailure::Host(Str::from(error.detail)),
				}))
			},
			None => RelayEvent::Result(Err(RelayFailure::Unavailable)),
		};
		let _ = pending.reply.try_send(event);
	}

	pub(crate) fn disconnect(&self) {
		drop(self.relay.responses.lock().take());
		drop(mem::take(&mut *self.relay.pending.lock()));
	}
}
impl Relay {
	async fn request(
		&self,
		request_id: u64,
		owner: Str,
		method: &'static str,
		args: Value,
		progress: Option<&dyn BridgeProgressSink>,
	) -> Result<Value, BridgeHostError> {
		let query_id = self.next_query.fetch_add(1, Ordering::Relaxed);
		let (reply, events) = flume::unbounded();
		let responses = {
			let open = self.responses.lock();
			let Some(responses) = open.as_ref() else {
				return Err(BridgeHostError::message("eval parent relay is unavailable"));
			};
			let mut pending = self.pending.lock();
			if pending.len() >= MAX_PENDING {
				return Err(BridgeHostError::message("eval parent relay is unavailable"));
			}
			pending.insert(query_id, Pending { request_id, reply });
			responses.clone()
		};
		let guard = QueryGuard { relay: self, request_id, query_id };
		let args_json = serde_json::to_string(&args)
			.map_err(|_| BridgeHostError::message("eval parent relay arguments are not JSON"))?;
		let sent = responses
			.send_async(frame(
				request_id,
				server_frame::Body::EvalParentOpen(pb::EvalParentOpen {
					query_id,
					owner: owner.to_string(),
					method: method.to_owned(),
					args_json,
				}),
			))
			.await
			.is_ok();
		drop(responses);
		if !sent {
			return Err(BridgeHostError::message("eval parent relay is unavailable"));
		}
		let result = loop {
			match events.recv_async().await {
				Ok(RelayEvent::Progress(event)) => {
					if let Some(progress) = progress {
						progress.progress(event)?;
					}
				},
				Ok(RelayEvent::Result(result)) => break result,
				Err(_) => break Err(RelayFailure::Unavailable),
			}
		};
		drop(guard);
		result.map_err(relay_error)
	}
}

struct QueryGuard<'r> {
	relay:      &'r Relay,
	request_id: u64,
	query_id:   u64,
}

impl Drop for QueryGuard<'_> {
	fn drop(&mut self) {
		if self.relay.pending.lock().remove(&self.query_id).is_none() {
			return;
		}
		let Some(responses) = self.relay.responses.lock().clone() else {
			return;
		};
		let frame = frame(
			self.request_id,
			server_frame::Body::EvalParentCancel(pb::EvalParentCancel { query_id: self.query_id }),
		);
		if let Err(flume::TrySendError::Full(frame)) = responses.try_send(frame)
			&& let Ok(runtime) = Handle::try_current()
		{
			runtime.spawn(async move {
				let _ = responses.send_async(frame).await;
			});
		}
	}
}

#[async_trait::async_trait]
impl ParentSessionHost for OwnedEvalParent {
	fn eval_session_config(&self) -> Result<EvalSessionConfig, BridgeHostError> {
		Ok(self.config.config.clone())
	}

	fn release_eval_owner(&self) {
		let this = self.clone();
		if let Ok(runtime) = Handle::try_current() {
			runtime.spawn(async move {
				let _ = this
					.relay
					.request(this.request_id, this.owner.clone(), "release", Value::Null, None)
					.await;
			});
		}
	}

	fn completion_available(&self) -> bool {
		self.config.completion
	}

	async fn completion(
		&self,
		args: Value,
		progress: &dyn BridgeProgressSink,
	) -> Result<Value, BridgeHostError> {
		self
			.relay
			.request(self.request_id, self.owner.clone(), "completion", args, Some(progress))
			.await
	}

	fn agent_available(&self) -> bool {
		self.config.agent
	}

	async fn agent(
		&self,
		args: Value,
		progress: &dyn BridgeProgressSink,
	) -> Result<Value, BridgeHostError> {
		self
			.relay
			.request(self.request_id, self.owner.clone(), "agent", args, Some(progress))
			.await
	}

	fn workpool_available(&self) -> bool {
		self.config.workpool
	}

	async fn workpool(
		&self,
		args: Value,
		progress: &dyn BridgeProgressSink,
	) -> Result<Value, BridgeHostError> {
		self
			.relay
			.request(self.request_id, self.owner.clone(), "workpool", args, Some(progress))
			.await
	}

	fn concurrency_available(&self) -> bool {
		self.config.concurrency
	}

	async fn concurrency(&self, args: Value) -> Result<Value, BridgeHostError> {
		self
			.relay
			.request(self.request_id, self.owner.clone(), "concurrency", args, None)
			.await
	}

	fn budget_available(&self) -> bool {
		self.config.budget
	}

	async fn budget(&self, args: Value) -> Result<Value, BridgeHostError> {
		self
			.relay
			.request(self.request_id, self.owner.clone(), "budget", args, None)
			.await
	}
}

fn relay_error(error: RelayFailure) -> BridgeHostError {
	match error {
		RelayFailure::Unavailable => BridgeHostError::message("eval parent relay is unavailable"),
		RelayFailure::Cancelled => BridgeHostError::message("eval parent operation was cancelled"),
		RelayFailure::Invalid(detail) | RelayFailure::Host(detail) => {
			BridgeHostError::message(detail)
		},
	}
}

/// Starts the session-side parent operation pump.
pub(crate) fn spawn_eval_parent_pump(
	client: &EnvClient,
	bridge: Arc<SessionBridgeHost>,
	shutdown: &CancellationToken,
	tasks: &mut Vec<JoinHandle<()>>,
) {
	tasks.push(tokio::spawn(pump_eval_parent_queries(client.clone(), bridge, shutdown.clone())));
}

async fn pump_eval_parent_queries(
	client: EnvClient,
	bridge: Arc<SessionBridgeHost>,
	shutdown: CancellationToken,
) {
	let queries = client.eval_parent_queries();
	let mut open = FastHashMap::<(u64, u64), CancellationToken>::default();
	let mut running = JoinSet::new();
	loop {
		tokio::select! {
			() = shutdown.cancelled() => break,
			event = queries.recv_async() => match event {
				Ok(EvalParentQueryEvent::Requested { request_id, query }) => {
					let cancelled = CancellationToken::new();
					open.insert((request_id, query.query_id), cancelled.clone());
					let client = client.clone();
					let bridge = Arc::clone(&bridge);
					running.spawn(async move { answer_query(&client, &bridge, request_id, query, cancelled).await });
				},
				Ok(EvalParentQueryEvent::Cancelled { request_id, query_id }) => {
					if let Some(cancelled) = open.remove(&(request_id, query_id)) { cancelled.cancel(); }
				},
				Ok(EvalParentQueryEvent::Progress(_)) => {},
				Err(_) => break,
			},
			Some(done) = running.join_next(), if !running.is_empty() => { if let Ok(key) = done { open.remove(&key); } },
		}
	}
	for cancelled in open.into_values() {
		cancelled.cancel();
	}
	while running.join_next().await.is_some() {}
}

async fn answer_query(
	client: &EnvClient,
	bridge: &SessionBridgeHost,
	request_id: u64,
	query: pb::EvalParentOpen,
	cancelled: CancellationToken,
) -> (u64, u64) {
	let key = (request_id, query.query_id);
	let result = answer_query_inner(client, bridge, request_id, &query, &cancelled).await;
	if !cancelled.is_cancelled() {
		let wire = match result {
			Ok(value) => pb::EvalParentResult {
				query_id: query.query_id,
				body:     serde_json::to_string(&value)
					.ok()
					.map(pb::eval_parent_result::Body::ResultJson),
			},
			Err(error) => pb::EvalParentResult {
				query_id: query.query_id,
				body:     Some(pb::eval_parent_result::Body::Error(error)),
			},
		};
		let _ = client.answer_eval_parent(request_id, wire).await;
	}
	key
}

async fn answer_query_inner(
	client: &EnvClient,
	bridge: &SessionBridgeHost,
	request_id: u64,
	query: &pb::EvalParentOpen,
	cancelled: &CancellationToken,
) -> Result<Value, pb::EvalParentError> {
	let args = serde_json::from_str(&query.args_json).map_err(|_| {
		error(pb::EvalParentErrorCode::EvalParentErrorInvalid, "invalid JSON arguments")
	})?;
	let parent = bridge.parent_for_relay(&query.owner).map_err(|_| {
		error(pb::EvalParentErrorCode::EvalParentErrorUnavailable, "parent is not bound")
	})?;
	if query.method == "release" {
		parent.release_eval_owner();
		return Ok(Value::Null);
	}
	if query.method == "config" {
		let config = ParentConfig {
			config:      parent.eval_session_config().map_err(|_| {
				error(
					pb::EvalParentErrorCode::EvalParentErrorUnavailable,
					"parent configuration is unavailable",
				)
			})?,
			completion:  parent.completion_available(),
			agent:       parent.agent_available(),
			workpool:    parent.workpool_available(),
			concurrency: parent.concurrency_available(),
			budget:      parent.budget_available(),
		};
		return serde_json::to_value(config).map_err(|_| {
			error(
				pb::EvalParentErrorCode::EvalParentErrorHost,
				"parent configuration is not serializable",
			)
		});
	}
	let (progress_tx, progress_rx) = flume::unbounded();
	let progress = RelayProgressSink { next: AtomicU64::new(0), tx: progress_tx };
	let method = query.method.as_str();
	let call = async {
		match method {
			"completion" => parent.completion(args, &progress).await,
			"agent" => parent.agent(args, &progress).await,
			"workpool" => parent.workpool(args, &progress).await,
			"concurrency" => parent.concurrency(args).await,
			"budget" => parent.budget(args).await,
			_ => Err(BridgeHostError::message("unknown eval parent operation")),
		}
	};
	tokio::pin!(call);
	loop {
		tokio::select! {
			biased;
			() = cancelled.cancelled() => return Err(error(pb::EvalParentErrorCode::EvalParentErrorCancelled, "parent operation cancelled")),
			event = progress_rx.recv_async() => if let Ok(event) = event { let _ = client.send_eval_parent_progress(request_id, pb::EvalParentProgress { query_id: query.query_id, seq: event.0, event_json: serde_json::to_string(&event.1).unwrap_or_default() }).await; },
			result = &mut call => {
				while let Ok((seq, event)) = progress_rx.try_recv() { let _ = client.send_eval_parent_progress(request_id, pb::EvalParentProgress { query_id: query.query_id, seq, event_json: serde_json::to_string(&event).unwrap_or_default() }).await; }
			return result.map_err(|_| error(pb::EvalParentErrorCode::EvalParentErrorHost, "parent operation failed"));
			},
		}
	}
}

struct RelayProgressSink {
	next: AtomicU64,
	tx:   flume::Sender<(u64, Value)>,
}
impl BridgeProgressSink for RelayProgressSink {
	fn progress(&self, event: Value) -> Result<(), BridgeHostError> {
		let seq = self.next.fetch_add(1, Ordering::Relaxed);
		self
			.tx
			.try_send((seq, event))
			.map_err(|_| BridgeHostError::message("eval parent progress receiver was dropped"))
	}
}

fn error(code: pb::EvalParentErrorCode, detail: &'static str) -> pb::EvalParentError {
	pb::EvalParentError { code: code as i32, detail: detail.to_owned() }
}

const fn frame(request_id: u64, body: server_frame::Body) -> pb::ServerFrame {
	pb::ServerFrame { request_id, body: Some(body), props: None }
}
