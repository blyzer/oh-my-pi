//! Relays the memory reflection a daemon-side `reflect` needs to the
//! connection that issued the call, and synthesizes it there, on that
//! session's inference.
//!
//! An attached session's `reflect` runs on the project daemon, which recalls
//! the evidence from the project's memory bank but owns no model: the
//! session's inference lives in another process. An application connection
//! that advertised [`omp_env::REFLECTION_RELAY_CAPABILITY`] to an environment
//! host gets one [`ConnectionReflection`]. Every native invocation and Exec it
//! issues captures an [`OwnedReflection`] bound to that request: a direct
//! `reflect` reaches it through the invocation's task-local
//! (`tools::invocation_reflection`), and a `dyn reflect` in a command's shell
//! through the command's issuer (`devices_host`). The daemon's
//! `ReflectionBridgeHost` asks it before any authority bound in its own
//! process. The query travels as a `ReflectionQuery` on that request's id
//! only, carrying the question, its context and the recalled contents, and
//! only that connection's `ReflectionAnswer` settles it.
//!
//! Not answering never invents an answer. An interrupted or cancelled call
//! withdraws its query (`ReflectionWithdrawn`), and a closed connection
//! resolves every pending reflection as unavailable and refuses later ones at
//! once, so `reflect` answers with the recalled evidence, exactly as it does
//! when no inference is bound at all.
//!
//! The session half is [`pump_reflection_queries`]: an attached composition
//! advertises the capability and synthesizes each relayed query through its
//! own reflection bridge, the one the driver binds to the session's inference
//! (`ProjectEnvironment::reflection_bridge`). A withdrawal, a closed transport
//! or shutdown stops the synthesis without answering.

use std::{
	mem,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use omp_core::{FastHashMap, Str};
use omp_env::{EnvClient, ReflectionQueryEvent};
use omp_proto::env::v1::{self as pb, reflection_answer, server_frame};
use omp_tools::memory::{ReflectionHost, ReflectionHostError, ReflectionRequest};
use parking_lot::Mutex;
use tokio::{
	runtime::Handle,
	task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use crate::approval_relay::RELAY_MAX_PENDING;

/// One connection's reflection relay. Clones share it.
#[derive(Clone)]
pub struct ConnectionReflection {
	relay: Arc<Relay>,
}

struct Relay {
	/// The connection's response channel. It is taken on disconnect, so a
	/// call that outlives its connection never keeps the connection's writer
	/// alive.
	responses:  Mutex<Option<flume::Sender<pb::ServerFrame>>>,
	next_query: AtomicU64,
	pending:    Mutex<FastHashMap<u64, PendingReflection>>,
}

struct PendingReflection {
	/// The request whose stream carried the query; its answer must echo it.
	request_id: u64,
	reply:      flume::Sender<Result<Str, ReflectionHostError>>,
}

/// A connection's reflection relay bound to the request that issued one
/// `reflect` call, directly or through a command.
#[derive(Clone)]
pub struct OwnedReflection {
	relay:      Arc<Relay>,
	request_id: u64,
}

const _: () = assert!(
	size_of::<Option<OwnedReflection>>() <= 16,
	"a call's reflection relay must stay a pointer and a request id"
);

impl ConnectionReflection {
	/// Creates the relay for a connection that answers on `responses`.
	pub fn new(responses: flume::Sender<pb::ServerFrame>) -> Self {
		Self {
			relay: Arc::new(Relay {
				responses:  Mutex::new(Some(responses)),
				next_query: AtomicU64::new(1),
				pending:    Mutex::new(FastHashMap::default()),
			}),
		}
	}

	/// Binds the relay to the request that issues one call: its reflection
	/// travels on `request_id`.
	pub fn owned(&self, request_id: u64) -> OwnedReflection {
		OwnedReflection { relay: Arc::clone(&self.relay), request_id }
	}

	/// Delivers the connection's answer to the reflection it names.
	///
	/// An answer to an unknown, withdrawn or already answered query, or one on
	/// another request than its query's, is ignored without a reply: an error
	/// frame on that request id would end the issuing call's stream.
	pub fn answer(&self, request_id: u64, answer: pb::ReflectionAnswer) {
		let query_id = answer.query_id;
		let pending = {
			let mut pending = self.relay.pending.lock();
			match pending.get(&query_id) {
				Some(open) if open.request_id == request_id => pending.remove(&query_id),
				_ => None,
			}
		};
		let Some(pending) = pending else {
			tracing::debug!(
				request_id,
				query_id,
				"ignored a reflection answer that matches no open query"
			);
			return;
		};
		let _ = pending.reply.try_send(result_from_wire(answer.body));
	}

	/// Ends the relay when its connection closes: every pending reflection is
	/// unavailable, every later one is refused at once, and the connection's
	/// response channel is released.
	pub fn disconnect(&self) {
		drop(self.relay.responses.lock().take());
		// Dropping the reply channels wakes every waiting call.
		drop(mem::take(&mut *self.relay.pending.lock()));
	}
}

#[async_trait::async_trait]
impl ReflectionHost for OwnedReflection {
	/// Asks the issuing connection to synthesize one reflection on its
	/// session's inference.
	///
	/// A closed connection, or one with [`RELAY_MAX_PENDING`] reflections open,
	/// is unavailable at once, and so is one that closes before answering.
	/// Dropping the returned future withdraws the query.
	async fn reflect(&self, request: ReflectionRequest) -> Result<Str, ReflectionHostError> {
		let query_id = self.relay.next_query.fetch_add(1, Ordering::Relaxed);
		let (reply, response) = flume::bounded(1);
		let responses = {
			// Registering under the response lock means a concurrent disconnect
			// either refuses this call here or clears it after registration.
			let open = self.relay.responses.lock();
			let Some(responses) = open.as_ref() else {
				return Err(ReflectionHostError::Unavailable);
			};
			let mut pending = self.relay.pending.lock();
			if pending.len() >= RELAY_MAX_PENDING {
				return Err(ReflectionHostError::Unavailable);
			}
			pending.insert(query_id, PendingReflection { request_id: self.request_id, reply });
			responses.clone()
		};
		let _withdraw = QueryGuard { relay: &self.relay, request_id: self.request_id, query_id };
		let ReflectionRequest { query, context, evidence } = request;
		let query = pb::ReflectionQuery {
			query_id,
			question: query.to_string(),
			context: context.map(|context| context.to_string()),
			evidence: evidence.iter().map(ToString::to_string).collect(),
		};
		let sent = responses
			.send_async(frame(self.request_id, server_frame::Body::ReflectionQuery(query)))
			.await
			.is_ok();
		drop(responses);
		if !sent {
			return Err(ReflectionHostError::Unavailable);
		}
		// A closed connection's disconnect dropped this reply channel.
		response
			.recv_async()
			.await
			.unwrap_or(Err(ReflectionHostError::Unavailable))
	}
}

/// Withdraws a query its caller stopped waiting for: the call was
/// interrupted or cancelled.
///
/// A query already answered, or cleared by a disconnect, is no longer pending
/// and draws no withdrawal.
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
		let withdrawn = frame(
			self.request_id,
			server_frame::Body::ReflectionWithdrawn(pb::ReflectionWithdrawn {
				query_id: self.query_id,
			}),
		);
		// A drop cannot await. A full channel hands the frame to a task when a
		// runtime is current; otherwise the session synthesizes an answer the
		// daemon then ignores.
		if let Err(flume::TrySendError::Full(withdrawn)) = responses.try_send(withdrawn)
			&& let Ok(runtime) = Handle::try_current()
		{
			runtime.spawn(async move {
				let _ = responses.send_async(withdrawn).await;
			});
		}
	}
}

/// What the connection's answer settles a reflection with. An unspecified or
/// unknown failure, and an answer without a body, are inference failures.
fn result_from_wire(body: Option<reflection_answer::Body>) -> Result<Str, ReflectionHostError> {
	match body {
		Some(reflection_answer::Body::Answer(answer)) => Ok(Str::from(answer)),
		Some(reflection_answer::Body::Failure(code)) => match pb::ReflectionFailure::try_from(code) {
			Ok(pb::ReflectionFailure::Unavailable) => Err(ReflectionHostError::Unavailable),
			Ok(pb::ReflectionFailure::Inference | pb::ReflectionFailure::Unspecified) | Err(_) => {
				Err(ReflectionHostError::Inference)
			},
		},
		None => Err(ReflectionHostError::Inference),
	}
}

/// The wire answer for one synthesis outcome.
fn wire_body(result: Result<Str, ReflectionHostError>) -> reflection_answer::Body {
	match result {
		Ok(answer) => reflection_answer::Body::Answer(answer.to_string()),
		Err(ReflectionHostError::Unavailable) => {
			reflection_answer::Body::Failure(pb::ReflectionFailure::Unavailable as i32)
		},
		Err(ReflectionHostError::Inference) => {
			reflection_answer::Body::Failure(pb::ReflectionFailure::Inference as i32)
		},
	}
}

/// Starts the task that synthesizes the reflections the daemon relays to
/// `client` through `host`.
pub fn spawn_reflection_pump(
	client: &EnvClient,
	host: Arc<dyn ReflectionHost>,
	shutdown: &CancellationToken,
	tasks: &mut Vec<JoinHandle<()>>,
) {
	tasks.push(tokio::spawn(pump_reflection_queries(client.clone(), host, shutdown.clone())));
}

/// Synthesizes every reflection the daemon relays to `client` through `host`,
/// until the queue closes with the transport or `shutdown` fires.
///
/// Each query runs as its own task, so one slow synthesis never delays
/// another, and its outcome goes back as a `ReflectionAnswer` on the query's
/// request. A withdrawal stops that synthesis and sends nothing; a closed
/// queue and shutdown do the same for every synthesis still running.
pub async fn pump_reflection_queries(
	client: EnvClient,
	host: Arc<dyn ReflectionHost>,
	shutdown: CancellationToken,
) {
	let queries = client.reflection_queries();
	// Withdrawal handles of the reflections still being synthesized, keyed as
	// the client keys its open queries: `(request_id, query_id)`.
	let mut open = FastHashMap::<(u64, u64), CancellationToken>::default();
	let mut synthesizing = JoinSet::new();
	loop {
		tokio::select! {
			() = shutdown.cancelled() => break,
			event = queries.recv_async() => match event {
				Ok(ReflectionQueryEvent::Requested { request_id, query }) => {
					let key = (request_id, query.query_id);
					let withdrawn = CancellationToken::new();
					open.insert(key, withdrawn.clone());
					let host = Arc::clone(&host);
					let client = client.clone();
					synthesizing.spawn(async move {
						answer_query(&client, host.as_ref(), request_id, query, &withdrawn).await;
						key
					});
				},
				Ok(ReflectionQueryEvent::Withdrawn { request_id, query_id }) => {
					if let Some(withdrawn) = open.remove(&(request_id, query_id)) {
						withdrawn.cancel();
					}
				},
				Err(_) => break,
			},
			Some(done) = synthesizing.join_next(), if !synthesizing.is_empty() => match done {
				Ok(key) => {
					open.remove(&key);
				},
				Err(error) if !error.is_cancelled() => {
					tracing::warn!(%error, "relayed reflection task failed");
				},
				Err(_) => {},
			},
		}
	}
	// No answer can reach the daemon any more: stop every synthesis.
	for withdrawn in open.into_values() {
		withdrawn.cancel();
	}
	while synthesizing.join_next().await.is_some() {}
}

/// Synthesizes one relayed query through `host` and answers it, unless it
/// was withdrawn first. A query without evidence is malformed: it is answered
/// as an inference failure without asking `host`.
async fn answer_query(
	client: &EnvClient,
	host: &dyn ReflectionHost,
	request_id: u64,
	query: pb::ReflectionQuery,
	withdrawn: &CancellationToken,
) {
	let query_id = query.query_id;
	let result = if query.evidence.is_empty() {
		Err(ReflectionHostError::Inference)
	} else {
		let request = ReflectionRequest {
			query:    Str::from(query.question),
			context:  query.context.map(Str::from),
			evidence: query.evidence.into_iter().map(Str::from).collect(),
		};
		tokio::select! {
			biased;
			() = withdrawn.cancelled() => return,
			result = host.reflect(request) => result,
		}
	};
	let answer = pb::ReflectionAnswer { query_id, body: Some(wire_body(result)) };
	if let Err(error) = client.answer_reflection(request_id, answer).await {
		// The daemon withdrew the query meanwhile, or the transport closed.
		tracing::debug!(
			error = &error as &dyn std::error::Error,
			request_id,
			query_id,
			"relayed reflection answer was not sent"
		);
	}
}

const fn frame(request_id: u64, body: server_frame::Body) -> pb::ServerFrame {
	pb::ServerFrame { request_id, body: Some(body), props: None }
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use tokio::time;

	use super::*;

	const REQUEST: u64 = 7;

	/// Bounded wait for frames crossing real channels and threads.
	const WAIT: Duration = Duration::from_secs(10);

	fn request() -> ReflectionRequest {
		ReflectionRequest {
			query:    Str::new_static("deploy target"),
			context:  Some(Str::new_static("Preparing the release")),
			evidence: Arc::from([Str::new_static("The deploy target is fly.io")]),
		}
	}

	fn relay(capacity: usize) -> (ConnectionReflection, flume::Receiver<pb::ServerFrame>) {
		let (responses, frames) = flume::bounded(capacity);
		(ConnectionReflection::new(responses), frames)
	}

	fn spawn_reflect(
		owned: &OwnedReflection,
	) -> tokio::task::JoinHandle<Result<Str, ReflectionHostError>> {
		let owned = owned.clone();
		tokio::spawn(async move { owned.reflect(request()).await })
	}

	async fn next_query(frames: &flume::Receiver<pb::ServerFrame>) -> (u64, pb::ReflectionQuery) {
		let frame = time::timeout(WAIT, frames.recv_async())
			.await
			.expect("relay frame timed out")
			.expect("relay frame");
		let Some(server_frame::Body::ReflectionQuery(query)) = frame.body else {
			panic!("expected a reflection query, got {:?}", frame.body);
		};
		(frame.request_id, query)
	}

	fn answer(query_id: u64, body: reflection_answer::Body) -> pb::ReflectionAnswer {
		pb::ReflectionAnswer { query_id, body: Some(body) }
	}

	#[tokio::test]
	async fn an_answer_settles_the_reflection_on_the_owner_request() {
		let (reflection, frames) = relay(8);
		let call = spawn_reflect(&reflection.owned(REQUEST));
		let (request_id, query) = next_query(&frames).await;
		assert_eq!(request_id, REQUEST);
		assert_eq!(query, pb::ReflectionQuery {
			query_id: query.query_id,
			question: "deploy target".to_owned(),
			context:  Some("Preparing the release".to_owned()),
			evidence: vec!["The deploy target is fly.io".to_owned()],
		});

		reflection.answer(
			REQUEST,
			answer(query.query_id, reflection_answer::Body::Answer("Deploys go to fly.io.".into())),
		);
		assert_eq!(call.await.expect("reflect task"), Ok(Str::new_static("Deploys go to fly.io.")));
		assert!(frames.is_empty(), "an answered query was withdrawn");
	}

	#[tokio::test]
	async fn failures_map_to_their_typed_refusals() {
		let cases = [
			(
				Some(reflection_answer::Body::Failure(pb::ReflectionFailure::Unavailable as i32)),
				ReflectionHostError::Unavailable,
			),
			(
				Some(reflection_answer::Body::Failure(pb::ReflectionFailure::Inference as i32)),
				ReflectionHostError::Inference,
			),
			(
				Some(reflection_answer::Body::Failure(pb::ReflectionFailure::Unspecified as i32)),
				ReflectionHostError::Inference,
			),
			(Some(reflection_answer::Body::Failure(99)), ReflectionHostError::Inference),
			(None, ReflectionHostError::Inference),
		];
		for (body, expected) in cases {
			let (reflection, frames) = relay(8);
			let call = spawn_reflect(&reflection.owned(REQUEST));
			let (_, query) = next_query(&frames).await;
			reflection.answer(REQUEST, pb::ReflectionAnswer { query_id: query.query_id, body });
			assert_eq!(call.await.expect("reflect task"), Err(expected));
		}
	}

	#[tokio::test]
	async fn answers_that_match_no_open_query_are_ignored() {
		let (reflection, frames) = relay(8);
		let call = spawn_reflect(&reflection.owned(REQUEST));
		let (_, query) = next_query(&frames).await;
		let text = || reflection_answer::Body::Answer("forged".into());
		// Another request's answer, and an unknown query's, decide nothing.
		reflection.answer(REQUEST + 1, answer(query.query_id, text()));
		reflection.answer(REQUEST, answer(query.query_id + 100, text()));
		reflection.answer(
			REQUEST,
			answer(query.query_id, reflection_answer::Body::Answer("Deploys go to fly.io.".into())),
		);
		assert_eq!(call.await.expect("reflect task"), Ok(Str::new_static("Deploys go to fly.io.")));
		// A late duplicate is ignored too.
		reflection.answer(REQUEST, answer(query.query_id, text()));
		assert!(frames.is_empty(), "ignored answers draw no reply");
	}

	#[tokio::test]
	async fn dropping_the_call_withdraws_its_query() {
		let (reflection, frames) = relay(8);
		let call = spawn_reflect(&reflection.owned(REQUEST));
		let (_, query) = next_query(&frames).await;
		call.abort();
		let frame = time::timeout(WAIT, frames.recv_async())
			.await
			.expect("withdrawal timed out")
			.expect("withdrawal");
		assert_eq!(frame.request_id, REQUEST);
		assert_eq!(
			frame.body,
			Some(server_frame::Body::ReflectionWithdrawn(pb::ReflectionWithdrawn {
				query_id: query.query_id,
			}))
		);
		// The answer that crosses the withdrawal settles nothing.
		reflection
			.answer(REQUEST, answer(query.query_id, reflection_answer::Body::Answer("x".into())));
		assert!(frames.is_empty());
	}

	#[tokio::test]
	async fn a_disconnect_makes_open_and_later_reflections_unavailable() {
		let (reflection, frames) = relay(8);
		let owned = reflection.owned(REQUEST);
		let call = spawn_reflect(&owned);
		let _ = next_query(&frames).await;
		reflection.disconnect();
		assert_eq!(call.await.expect("reflect task"), Err(ReflectionHostError::Unavailable));
		assert_eq!(owned.reflect(request()).await, Err(ReflectionHostError::Unavailable));
		drop(reflection);
		drop(owned);
		assert!(
			time::timeout(WAIT, frames.recv_async())
				.await
				.expect("channel close timed out")
				.is_err(),
			"the relay kept the connection's writer alive"
		);
	}

	#[tokio::test]
	async fn reflections_past_the_cap_are_unavailable() {
		let (reflection, frames) = relay(RELAY_MAX_PENDING + 1);
		let owned = reflection.owned(REQUEST);
		let calls = (0..RELAY_MAX_PENDING)
			.map(|_| spawn_reflect(&owned))
			.collect::<Vec<_>>();
		for _ in 0..RELAY_MAX_PENDING {
			let _ = next_query(&frames).await;
		}
		assert_eq!(owned.reflect(request()).await, Err(ReflectionHostError::Unavailable));
		reflection.disconnect();
		for call in calls {
			assert_eq!(call.await.expect("reflect task"), Err(ReflectionHostError::Unavailable));
		}
	}

	/// Answers every synthesis with `answer`, recording each request; a
	/// `None` answer never completes, so the test controls withdrawals.
	struct RecordingHost {
		answer:   Option<Result<Str, ReflectionHostError>>,
		requests: Mutex<Vec<ReflectionRequest>>,
	}

	#[async_trait::async_trait]
	impl ReflectionHost for RecordingHost {
		async fn reflect(&self, request: ReflectionRequest) -> Result<Str, ReflectionHostError> {
			self.requests.lock().push(request);
			match self.answer.clone() {
				Some(answer) => answer,
				None => std::future::pending().await,
			}
		}
	}

	/// The session half on a client whose frames the test plays: queries go in
	/// as daemon frames, answers come out as client frames.
	struct SessionPump {
		host:      Arc<RecordingHost>,
		responses: flume::Sender<pb::ServerFrame>,
		answers:   flume::Receiver<pb::ClientFrame>,
		pump:      JoinHandle<()>,
	}

	impl SessionPump {
		fn start(answer: Option<Result<Str, ReflectionHostError>>) -> Self {
			let (outgoing, answers) = flume::unbounded();
			let (responses, incoming) = flume::unbounded();
			let client = EnvClient::from_channels(outgoing, incoming);
			let host = Arc::new(RecordingHost { answer, requests: Mutex::new(Vec::new()) });
			let mut tasks = Vec::new();
			spawn_reflection_pump(
				&client,
				Arc::clone(&host) as Arc<dyn ReflectionHost>,
				&CancellationToken::new(),
				&mut tasks,
			);
			let pump = tasks.pop().expect("pump task");
			Self { host, responses, answers, pump }
		}

		fn query(&self, request_id: u64, query_id: u64, evidence: &[&str]) {
			self
				.responses
				.send(frame(
					request_id,
					server_frame::Body::ReflectionQuery(pb::ReflectionQuery {
						query_id,
						question: "deploy target".to_owned(),
						context: None,
						evidence: evidence.iter().map(|item| (*item).to_owned()).collect(),
					}),
				))
				.expect("send reflection query");
		}

		fn withdraw(&self, request_id: u64, query_id: u64) {
			self
				.responses
				.send(frame(
					request_id,
					server_frame::Body::ReflectionWithdrawn(pb::ReflectionWithdrawn { query_id }),
				))
				.expect("send reflection withdrawal");
		}

		async fn answer(&self) -> (u64, Option<pb::InvocationScope>, pb::ReflectionAnswer) {
			let frame = time::timeout(WAIT, self.answers.recv_async())
				.await
				.expect("reflection answer timed out")
				.expect("reflection answer");
			let Some(pb::client_frame::Body::ReflectionAnswer(answer)) = frame.body else {
				panic!("expected a reflection answer, got {:?}", frame.body);
			};
			(frame.request_id, frame.scope, answer)
		}

		async fn asked(&self, count: usize) {
			time::timeout(WAIT, async {
				while self.host.requests.lock().len() < count {
					time::sleep(Duration::from_millis(5)).await;
				}
			})
			.await
			.expect("the session host was not asked");
		}
	}

	#[tokio::test]
	async fn the_session_synthesizes_through_its_host_on_the_query_request() {
		let session = SessionPump::start(Some(Ok(Str::new_static("Deploys go to fly.io."))));
		session.query(5, 1, &["The deploy target is fly.io"]);
		let (request_id, scope, answer) = session.answer().await;
		assert_eq!((request_id, scope), (5, None));
		assert_eq!(answer, pb::ReflectionAnswer {
			query_id: 1,
			body:     Some(reflection_answer::Body::Answer("Deploys go to fly.io.".into())),
		});
		assert_eq!(*session.host.requests.lock(), [ReflectionRequest {
			query:    Str::new_static("deploy target"),
			context:  None,
			evidence: Arc::from([Str::new_static("The deploy target is fly.io")]),
		}]);
	}

	#[tokio::test]
	async fn an_unbound_session_answers_unavailable_and_a_failed_one_inference() {
		for (refusal, wire) in [
			(ReflectionHostError::Unavailable, pb::ReflectionFailure::Unavailable),
			(ReflectionHostError::Inference, pb::ReflectionFailure::Inference),
		] {
			let session = SessionPump::start(Some(Err(refusal)));
			session.query(5, 1, &["The deploy target is fly.io"]);
			let (_, _, answer) = session.answer().await;
			assert_eq!(answer.body, Some(reflection_answer::Body::Failure(wire as i32)));
		}
	}

	#[tokio::test]
	async fn a_query_without_evidence_is_refused_without_inference() {
		let session = SessionPump::start(Some(Ok(Str::new_static("invented"))));
		session.query(5, 1, &[]);
		let (_, _, answer) = session.answer().await;
		assert_eq!(
			answer.body,
			Some(reflection_answer::Body::Failure(pb::ReflectionFailure::Inference as i32))
		);
		assert!(session.host.requests.lock().is_empty(), "inference ran without evidence");
	}

	#[tokio::test]
	async fn a_withdrawn_query_is_never_answered() {
		let session = SessionPump::start(None);
		session.query(5, 1, &["The deploy target is fly.io"]);
		session.asked(1).await;
		session.withdraw(5, 1);
		let SessionPump { responses, answers, pump, .. } = session;
		drop(responses);
		time::timeout(WAIT, pump)
			.await
			.expect("pump stopped when the transport closed")
			.expect("pump task");
		assert!(answers.is_empty(), "a withdrawn query was answered");
	}

	#[tokio::test]
	async fn a_closed_transport_stops_every_synthesis_without_answering() {
		let session = SessionPump::start(None);
		session.query(5, 1, &["The deploy target is fly.io"]);
		session.query(6, 2, &["Releases tag main"]);
		session.asked(2).await;
		let SessionPump { responses, answers, pump, .. } = session;
		drop(responses);
		time::timeout(WAIT, pump)
			.await
			.expect("pump stopped when the transport closed")
			.expect("pump task");
		assert!(answers.is_empty(), "a closed transport drew an answer");
	}
}
