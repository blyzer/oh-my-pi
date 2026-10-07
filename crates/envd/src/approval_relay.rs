//! Relays the approval prompts a daemon-side command needs to the connection
//! that issued the command, and answers them in the session that issued it.
//!
//! An attached session runs its commands on the project daemon, whose host has
//! no in-process approval route: the session's kernel lives in another
//! process. An application connection that advertised
//! [`omp_env::APPROVAL_RELAY_CAPABILITY`] to an environment host gets one
//! [`ConnectionApprovals`]. Every Exec or native invocation it issues captures
//! an [`OwnedApprovals`] bound to that request, and a prompt the command needs
//! travels as an `ApprovalQuery` on that request's id only; it is never
//! broadcast. The connection answers with an `ApprovalAnswer`. The daemon
//! builds the ticket from its own requirements and the client's decision, so a
//! client can approve or deny a prompt but never change what it approves.
//!
//! Every way of not answering fails closed with the in-process route's
//! semantics. The daemon's backstop deadline is the prompt's own timeout plus
//! [`RELAY_GRACE`], so the session's timeout decision normally arrives first. A
//! dropped request withdraws its query. A closed connection resolves every
//! pending prompt as unreachable and refuses later prompts at once, including
//! those of commands that outlive it (detached or auto-backgrounded).
//!
//! The session half is [`pump_approval_queries`]: an attached composition
//! advertises the capability and files each relayed query on the approval
//! route the driver binds ([`ApprovalRelayBinding`]), the same kernel route
//! its in-process prompts use, so the prompt is journaled and answered like any
//! other. A withdrawal or a closed transport cancels the filed prompt instead
//! of answering it.

use std::{
	mem,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use omp_agent::{
	ApprovalDecision, ApprovalRoute, ApprovalScope, ApprovalSource, ApprovalSpec, ApprovalTicket,
	TicketState,
};
use omp_core::{FastHashMap, Str, sf};
use omp_env::{ApprovalQueryEvent, EnvClient};
use omp_proto::env::v1::{self as pb, server_frame};
use parking_lot::{Mutex, RwLock};
use tokio::{
	runtime::Handle,
	task::{JoinHandle, JoinSet},
	time,
};
use tokio_util::sync::CancellationToken;

/// How long the daemon waits past a prompt's own timeout before deciding it
/// itself.
///
/// The session times the prompt out first and answers with its timeout
/// decision, so the session's journal and the daemon agree at the boundary.
/// The grace only matters when the client hangs.
pub const RELAY_GRACE: Duration = Duration::from_secs(10);

/// Most prompts one connection may have outstanding; more fail closed at once.
pub const RELAY_MAX_PENDING: usize = 64;

/// Reason recorded when the issuing connection cannot answer any more.
const DISCONNECTED: &str = "the connection that issued the command closed";

/// One connection's approval relay. Clones share it.
#[derive(Clone)]
pub struct ConnectionApprovals {
	relay: Arc<Relay>,
}

struct Relay {
	/// The connection's response channel. It is taken on disconnect, so a
	/// command that outlives its connection never keeps the connection's
	/// writer alive.
	responses:  Mutex<Option<flume::Sender<pb::ServerFrame>>>,
	next_query: AtomicU64,
	pending:    Mutex<FastHashMap<u64, PendingApproval>>,
}

struct PendingApproval {
	/// The request whose stream carried the query; its answer must echo it.
	request_id: u64,
	reply:      flume::Sender<ApprovalDecision>,
}

/// A connection's relay bound to the request that issued one command.
#[derive(Clone)]
pub struct OwnedApprovals {
	relay:      Arc<Relay>,
	request_id: u64,
}

const _: () = assert!(
	size_of::<Option<OwnedApprovals>>() <= 16,
	"a command's approval relay must stay a pointer and a request id"
);

impl ConnectionApprovals {
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

	/// Binds the relay to the request that issues one command: that command's
	/// prompts travel on `request_id`.
	pub fn owned(&self, request_id: u64) -> OwnedApprovals {
		OwnedApprovals { relay: Arc::clone(&self.relay), request_id }
	}

	/// Delivers the connection's answer to the prompt it names.
	///
	/// An answer to an unknown, withdrawn, already answered or timed-out query,
	/// or one on another request than its query's, is ignored without a reply:
	/// an error frame on that request id would end the issuing command's
	/// stream.
	pub fn answer(&self, request_id: u64, answer: pb::ApprovalAnswer) {
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
				"ignored an approval answer that matches no open query"
			);
			return;
		};
		let _ = pending.reply.try_send(decision_from_wire(answer.decision));
	}

	/// Ends the relay when its connection closes: every pending prompt is
	/// decided as unreachable, every later prompt fails closed at once, and
	/// the connection's response channel is released.
	pub fn disconnect(&self) {
		drop(self.relay.responses.lock().take());
		// Dropping the reply channels wakes every waiting prompt.
		let pending = mem::take(&mut *self.relay.pending.lock());
		drop(pending);
	}
}

impl OwnedApprovals {
	/// Whether the issuing connection can still answer a prompt.
	pub fn is_live(&self) -> bool {
		self.relay.responses.lock().is_some()
	}

	/// Asks the issuing connection to decide one prompt and returns the
	/// decided ticket.
	///
	/// The ticket carries `reasons` as given, whatever the client answers.
	/// Without an answer the prompt is decided by its own timeout rules after
	/// its smallest nonzero timeout plus [`RELAY_GRACE`] (never, when every
	/// timeout is zero), and by its unreachable rules when the connection
	/// closes or already has [`RELAY_MAX_PENDING`] prompts open. Dropping the
	/// returned future withdraws the query.
	pub async fn request(
		&self,
		invocation_id: Option<Str>,
		reasons: Vec<ApprovalSpec>,
		created_at_ms: u64,
	) -> ApprovalTicket {
		let query_id = self.relay.next_query.fetch_add(1, Ordering::Relaxed);
		let mut ticket = ApprovalTicket {
			ticket_id: sf!("relay-{query_id}"),
			invocation_id,
			reasons,
			state: TicketState::Pending,
			decision: None,
			created_at_ms,
		};
		let decision = self.decide(query_id, &ticket).await;
		ticket.state = TicketState::Decided;
		ticket.decision = Some(decision);
		ticket
	}

	async fn decide(&self, query_id: u64, ticket: &ApprovalTicket) -> ApprovalDecision {
		let (reply, response) = flume::bounded(1);
		let responses = {
			// Registering under the response lock means a concurrent disconnect
			// either refuses this prompt here or clears it after registration.
			let open = self.relay.responses.lock();
			let Some(responses) = open.as_ref() else {
				return ticket.unreachable_decision(DISCONNECTED);
			};
			let mut pending = self.relay.pending.lock();
			if pending.len() >= RELAY_MAX_PENDING {
				return ticket.unreachable_decision(
					"too many approval prompts are open on the issuing connection",
				);
			}
			pending.insert(query_id, PendingApproval { request_id: self.request_id, reply });
			responses.clone()
		};
		let _withdraw = QueryGuard { relay: &self.relay, request_id: self.request_id, query_id };
		let query = pb::ApprovalQuery {
			query_id,
			invocation_id: ticket.invocation_id.as_ref().map(ToString::to_string),
			reasons: ticket.reasons.iter().map(wire_spec).collect(),
			created_at_ms: ticket.created_at_ms,
		};
		let sent = responses
			.send_async(frame(self.request_id, server_frame::Body::ApprovalQuery(query)))
			.await
			.is_ok();
		drop(responses);
		if !sent {
			return ticket.unreachable_decision(DISCONNECTED);
		}
		let backstop = ticket
			.reasons
			.iter()
			.map(|reason| reason.timeout_ms)
			.filter(|timeout| *timeout != 0)
			.min()
			.map(|timeout| Duration::from_millis(timeout).saturating_add(RELAY_GRACE));
		let answered = match backstop {
			Some(backstop) => time::timeout(backstop, response.recv_async()).await.ok(),
			None => Some(response.recv_async().await),
		};
		match answered {
			Some(Ok(decision)) => decision,
			// The connection closed: its disconnect dropped this reply channel.
			Some(Err(_)) => ticket.unreachable_decision(DISCONNECTED),
			None => ticket.timeout_decision(),
		}
	}
}

/// Withdraws a query its requester stopped waiting for: the command was
/// cancelled or finished, or the backstop deadline passed.
///
/// A query already answered, or cleared by a disconnect, is no longer
/// pending and draws no withdrawal.
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
			server_frame::Body::ApprovalWithdrawn(pb::ApprovalWithdrawn { query_id: self.query_id }),
		);
		// A drop cannot await. A full channel hands the frame to a task when a
		// runtime is current; otherwise the client's own timeout retires the
		// prompt.
		if let Err(flume::TrySendError::Full(withdrawn)) = responses.try_send(withdrawn)
			&& let Ok(runtime) = Handle::try_current()
		{
			runtime.spawn(async move {
				let _ = responses.send_async(withdrawn).await;
			});
		}
	}
}

/// The approver that answers one environment-side prompt.
pub enum EnvApprover {
	/// The connection that issued the command, over the wire.
	Relay(OwnedApprovals),
	/// The route an in-process composition bound on this host.
	Route(ApprovalRoute),
}

impl EnvApprover {
	/// Whether a prompt can reach a human at all.
	pub fn is_reachable(&self) -> bool {
		match self {
			Self::Relay(relay) => relay.is_live(),
			Self::Route(_) => true,
		}
	}

	/// Asks the approver to decide one prompt and returns the decided ticket.
	pub async fn request(
		&self,
		invocation_id: Option<Str>,
		reasons: Vec<ApprovalSpec>,
		created_at_ms: u64,
	) -> ApprovalTicket {
		match self {
			Self::Relay(relay) => relay.request(invocation_id, reasons, created_at_ms).await,
			Self::Route(route) => route.request(invocation_id, reasons, created_at_ms).await,
		}
	}
}

/// The approval route an attached session answers relayed prompts with.
///
/// The driver binds the kernel's route through
/// `ProjectEnvironment::bind_approval_authority`. While none is bound, a
/// relayed prompt is decided by its unreachable rules at once.
pub type ApprovalRelayBinding = Arc<RwLock<Option<ApprovalRoute>>>;

/// Reason recorded for a relayed prompt that arrives while no route is bound.
const UNBOUND: &str = "no approval route is bound to the session";

/// Starts the task that answers the prompts the daemon relays to `client`,
/// through the route bound in the binding it returns.
pub fn spawn_approval_pump(
	client: &EnvClient,
	shutdown: &CancellationToken,
	tasks: &mut Vec<JoinHandle<()>>,
) -> ApprovalRelayBinding {
	let route = ApprovalRelayBinding::default();
	tasks.push(tokio::spawn(pump_approval_queries(
		client.clone(),
		Arc::clone(&route),
		shutdown.clone(),
	)));
	route
}

/// Answers every approval query the daemon relays to `client`, until the
/// queue closes with the transport or `shutdown` fires.
///
/// Each query is filed on the bound route with `request_cancellable`, so it is
/// journaled, decided and timed out exactly as an in-process prompt is, and
/// its decision goes back as an `ApprovalAnswer` on the query's request. A
/// withdrawal cancels the filed prompt and sends nothing; a closed queue and
/// shutdown do the same for every prompt still open.
pub async fn pump_approval_queries(
	client: EnvClient,
	route: ApprovalRelayBinding,
	shutdown: CancellationToken,
) {
	let queries = client.approval_queries();
	// Withdrawal handles of the prompts still being decided, keyed as the
	// client keys its open queries: `(request_id, query_id)`.
	let mut open = FastHashMap::<(u64, u64), CancellationToken>::default();
	let mut deciding = JoinSet::new();
	loop {
		tokio::select! {
			() = shutdown.cancelled() => break,
			event = queries.recv_async() => match event {
				Ok(ApprovalQueryEvent::Requested { request_id, query }) => {
					let key = (request_id, query.query_id);
					let withdrawn = CancellationToken::new();
					open.insert(key, withdrawn.clone());
					let route = route.read().clone();
					let client = client.clone();
					deciding.spawn(async move {
						answer_query(&client, route, request_id, query, &withdrawn).await;
						key
					});
				},
				Ok(ApprovalQueryEvent::Withdrawn { request_id, query_id }) => {
					if let Some(withdrawn) = open.remove(&(request_id, query_id)) {
						withdrawn.cancel();
					}
				},
				Err(_) => break,
			},
			Some(decided) = deciding.join_next(), if !deciding.is_empty() => match decided {
				Ok(key) => {
					open.remove(&key);
				},
				Err(error) if !error.is_cancelled() => {
					tracing::warn!(%error, "relayed approval task failed");
				},
				Err(_) => {},
			},
		}
	}
	// No answer can reach the daemon any more: withdraw every open prompt.
	for withdrawn in open.into_values() {
		withdrawn.cancel();
	}
	while deciding.join_next().await.is_some() {}
}

/// Decides one relayed query on `route` and answers it, unless it was
/// withdrawn first.
async fn answer_query(
	client: &EnvClient,
	route: Option<ApprovalRoute>,
	request_id: u64,
	query: pb::ApprovalQuery,
	withdrawn: &CancellationToken,
) {
	let query_id = query.query_id;
	let invocation_id = query.invocation_id.map(Str::from);
	let reasons = query.reasons.into_iter().map(spec_from_wire).collect();
	let mut ticket = match route {
		Some(route) => {
			route
				.request_cancellable(invocation_id, reasons, query.created_at_ms, withdrawn.clone())
				.await
		},
		None => ApprovalTicket {
			ticket_id: Str::default(),
			invocation_id,
			reasons,
			state: TicketState::Pending,
			decision: None,
			created_at_ms: query.created_at_ms,
		},
	};
	if withdrawn.is_cancelled() {
		return;
	}
	let decision = ticket
		.decision
		.take()
		.unwrap_or_else(|| ticket.unreachable_decision(UNBOUND));
	if let Err(error) = client
		.answer_approval(request_id, query_id, wire_decision(decision))
		.await
	{
		// The daemon withdrew the query meanwhile, or the transport closed.
		tracing::debug!(
			error = &error as &dyn std::error::Error,
			request_id,
			query_id,
			"relayed approval answer was not sent"
		);
	}
}

const fn frame(request_id: u64, body: server_frame::Body) -> pb::ServerFrame {
	pb::ServerFrame { request_id, body: Some(body), props: None }
}

/// The wire form of one requirement, field for field.
fn wire_spec(spec: &ApprovalSpec) -> pb::ApprovalSpec {
	pb::ApprovalSpec {
		title:           spec.title.to_string(),
		body:            spec.body.to_string(),
		subject:         spec.subject.to_string(),
		kind:            spec.kind.to_string(),
		scopes:          spec.scopes.iter().map(ToString::to_string).collect(),
		timeout_default: spec.default,
		route:           spec.route.to_string(),
		approver:        spec.approver.as_ref().map(ToString::to_string),
		timeout_ms:      spec.timeout_ms,
		unreachable:     spec.unreachable.to_string(),
		require_human:   spec.require_human,
		pattern:         spec.pattern.as_ref().map(ToString::to_string),
		evidence:        spec.evidence.iter().map(ToString::to_string).collect(),
	}
}

/// The decision a client's answer carries. A missing decision, an unknown
/// source, or an empty scope is malformed and denies.
fn decision_from_wire(decision: Option<pb::ApprovalDecision>) -> ApprovalDecision {
	let Some(decision) = decision else {
		return malformed("the approval answer carried no decision");
	};
	let Ok(source) = decision.source.parse::<ApprovalSource>() else {
		return malformed("the approval answer named an unknown decision source");
	};
	let scope = match decision.scope.parse::<ApprovalScope>() {
		Ok(scope) if !decision.scope.is_empty() => scope,
		_ => return malformed("the approval answer named no grant scope"),
	};
	ApprovalDecision {
		approved: decision.approved,
		scope,
		source,
		decided_by: decision.decided_by.map(Str::from),
		reason: decision.reason.map(Str::from),
		audited: decision.audited,
	}
}

/// The session's form of one relayed requirement, field for field.
fn spec_from_wire(spec: pb::ApprovalSpec) -> ApprovalSpec {
	ApprovalSpec {
		title:         Str::from(spec.title),
		body:          Str::from(spec.body),
		subject:       Str::from(spec.subject),
		kind:          Str::from(spec.kind),
		scopes:        spec.scopes.into_iter().map(Str::from).collect(),
		default:       spec.timeout_default,
		route:         Str::from(spec.route),
		approver:      spec.approver.map(Str::from),
		timeout_ms:    spec.timeout_ms,
		unreachable:   Str::from(spec.unreachable),
		require_human: spec.require_human,
		pattern:       spec.pattern.map(Str::from),
		evidence:      spec.evidence.into_iter().map(Str::from).collect(),
	}
}

/// The wire form of the session's decision; scope and source travel as their
/// strum spellings.
fn wire_decision(decision: ApprovalDecision) -> pb::ApprovalDecision {
	pb::ApprovalDecision {
		approved:   decision.approved,
		scope:      decision.scope.as_str().to_owned(),
		source:     <&'static str>::from(decision.source).to_owned(),
		decided_by: decision.decided_by.as_ref().map(ToString::to_string),
		reason:     decision.reason.as_ref().map(ToString::to_string),
		audited:    decision.audited,
	}
}

const fn malformed(reason: &'static str) -> ApprovalDecision {
	ApprovalDecision {
		approved:   false,
		scope:      ApprovalScope::Once,
		source:     ApprovalSource::Unavailable,
		decided_by: None,
		reason:     Some(Str::new_static(reason)),
		audited:    false,
	}
}

#[cfg(test)]
mod tests {
	use tokio::time::Instant;

	use super::*;

	const REQUEST: u64 = 7;

	fn amendment(timeout_ms: u64) -> ApprovalSpec {
		ApprovalSpec {
			title: sf!("Approve scoped sandbox amendment"),
			body: sf!("The sandbox denied a write. Approve write for this exact command one time?"),
			subject: sf!("/workspace/.git"),
			kind: sf!("sandbox_amendment"),
			scopes: vec![sf!("once")],
			default: Some(false),
			route: sf!("local"),
			approver: None,
			timeout_ms,
			unreachable: sf!("fail_closed"),
			require_human: true,
			pattern: Some(sf!("echo x > .git/a")),
			evidence: vec![sf!("write"), sf!("/workspace/.git")],
		}
	}

	fn client_decision(approved: bool, scope: &str, source: &str) -> pb::ApprovalDecision {
		pb::ApprovalDecision {
			approved,
			scope: scope.to_owned(),
			source: source.to_owned(),
			decided_by: Some("tester".to_owned()),
			reason: None,
			audited: false,
		}
	}

	fn relay(capacity: usize) -> (ConnectionApprovals, flume::Receiver<pb::ServerFrame>) {
		let (responses, frames) = flume::bounded(capacity);
		(ConnectionApprovals::new(responses), frames)
	}

	fn spawn_request(
		owned: &OwnedApprovals,
		spec: ApprovalSpec,
	) -> tokio::task::JoinHandle<ApprovalTicket> {
		let owned = owned.clone();
		tokio::spawn(async move { owned.request(None, vec![spec], 1_000).await })
	}

	async fn next_query(frames: &flume::Receiver<pb::ServerFrame>) -> (u64, pb::ApprovalQuery) {
		let frame = frames.recv_async().await.expect("relay frame");
		let Some(server_frame::Body::ApprovalQuery(query)) = frame.body else {
			panic!("expected an approval query, got {:?}", frame.body);
		};
		(frame.request_id, query)
	}

	fn withdrawn(frame: pb::ServerFrame) -> (u64, u64) {
		let Some(server_frame::Body::ApprovalWithdrawn(withdrawn)) = frame.body else {
			panic!("expected an approval withdrawal, got {:?}", frame.body);
		};
		(frame.request_id, withdrawn.query_id)
	}

	fn decided(ticket: &ApprovalTicket) -> &ApprovalDecision {
		assert_eq!(ticket.state, TicketState::Decided);
		ticket.decision.as_ref().expect("decided ticket")
	}

	#[tokio::test(start_paused = true)]
	async fn an_approval_decides_the_daemon_requirements_on_the_owner_request() {
		let (approvals, frames) = relay(8);
		let owned = approvals.owned(REQUEST);
		let request = spawn_request(&owned, amendment(120_000));
		let (request_id, query) = next_query(&frames).await;
		assert_eq!(request_id, REQUEST);
		assert_eq!(query.invocation_id, None);
		assert_eq!(query.created_at_ms, 1_000);
		assert_eq!(query.reasons, vec![pb::ApprovalSpec {
			title:           "Approve scoped sandbox amendment".to_owned(),
			body:            "The sandbox denied a write. Approve write for this exact command one \
			                  time?"
				.to_owned(),
			subject:         "/workspace/.git".to_owned(),
			kind:            "sandbox_amendment".to_owned(),
			scopes:          vec!["once".to_owned()],
			timeout_default: Some(false),
			route:           "local".to_owned(),
			approver:        None,
			timeout_ms:      120_000,
			unreachable:     "fail_closed".to_owned(),
			require_human:   true,
			pattern:         Some("echo x > .git/a".to_owned()),
			evidence:        vec!["write".to_owned(), "/workspace/.git".to_owned()],
		}]);

		approvals.answer(REQUEST, pb::ApprovalAnswer {
			query_id: query.query_id,
			decision: Some(client_decision(true, "once", "user")),
		});
		let ticket = request.await.expect("request task");
		assert_eq!(decided(&ticket), &ApprovalDecision {
			approved:   true,
			scope:      ApprovalScope::Once,
			source:     ApprovalSource::User,
			decided_by: Some(sf!("tester")),
			reason:     None,
			audited:    false,
		});
		assert_eq!(ticket.reasons, vec![amendment(120_000)], "the client cannot change the subject");
		assert!(frames.is_empty(), "an answered query draws no withdrawal");
	}

	#[tokio::test(start_paused = true)]
	async fn an_unanswered_query_times_out_after_the_grace_and_is_withdrawn() {
		let (approvals, frames) = relay(8);
		let started = Instant::now();
		let mut request = spawn_request(&approvals.owned(REQUEST), amendment(120_000));
		let (_, query) = next_query(&frames).await;
		assert!(
			time::timeout(Duration::from_millis(120_000), &mut request)
				.await
				.is_err(),
			"the daemon decided before the session's own timeout plus the grace"
		);
		let ticket = request.await.expect("request task");
		assert!(started.elapsed() >= Duration::from_millis(120_000) + RELAY_GRACE);
		let decision = decided(&ticket);
		assert!(!decision.approved);
		assert_eq!(decision.source, ApprovalSource::Timeout);
		let frame = frames.try_recv().expect("withdrawal frame");
		assert_eq!(withdrawn(frame), (REQUEST, query.query_id));

		approvals.answer(REQUEST, pb::ApprovalAnswer {
			query_id: query.query_id,
			decision: Some(client_decision(true, "once", "user")),
		});
		assert!(frames.is_empty(), "a late answer draws no reply");
	}

	#[tokio::test(start_paused = true)]
	async fn dropping_the_request_withdraws_its_query() {
		let (approvals, frames) = relay(8);
		let request = spawn_request(&approvals.owned(REQUEST), amendment(120_000));
		let (_, query) = next_query(&frames).await;
		request.abort();
		assert!(request.await.expect_err("aborted request").is_cancelled());
		let frame = frames.recv_async().await.expect("withdrawal frame");
		assert_eq!(withdrawn(frame), (REQUEST, query.query_id));
		approvals.answer(REQUEST, pb::ApprovalAnswer {
			query_id: query.query_id,
			decision: Some(client_decision(true, "once", "user")),
		});
		assert!(frames.is_empty(), "an answer to a withdrawn query draws no reply");
	}

	#[tokio::test(start_paused = true)]
	async fn a_full_channel_still_withdraws_through_a_task() {
		let (approvals, frames) = relay(1);
		let request = spawn_request(&approvals.owned(REQUEST), amendment(120_000));
		let (_, query) = next_query(&frames).await;
		// Fill the one slot so the withdrawal cannot be sent inline.
		approvals
			.relay
			.responses
			.lock()
			.as_ref()
			.expect("live relay")
			.try_send(pb::ServerFrame::default())
			.expect("fill the response channel");
		request.abort();
		let _ = request.await;
		assert_eq!(frames.recv_async().await.expect("filler").body, None);
		let frame = frames.recv_async().await.expect("withdrawal frame");
		assert_eq!(withdrawn(frame), (REQUEST, query.query_id));
	}

	#[tokio::test(start_paused = true)]
	async fn a_disconnect_fails_prompts_closed_and_releases_the_connection() {
		let (approvals, frames) = relay(8);
		let owned = approvals.owned(REQUEST);
		let request = spawn_request(&owned, amendment(120_000));
		let _ = next_query(&frames).await;
		approvals.disconnect();
		let ticket = request.await.expect("request task");
		let decision = decided(&ticket);
		assert!(!decision.approved);
		assert_eq!(decision.source, ApprovalSource::Unavailable);
		assert!(!owned.is_live());

		// A command that outlives its connection fails closed at once.
		let later = owned.request(None, vec![amendment(120_000)], 2_000).await;
		assert!(!decided(&later).approved);
		assert_eq!(decided(&later).source, ApprovalSource::Unavailable);
		// The relay and its owned handles no longer hold the response sender.
		assert!(frames.is_disconnected(), "the relay kept the connection's writer alive");
		assert!(frames.is_empty(), "a disconnect sends nothing");
	}

	#[tokio::test(start_paused = true)]
	async fn malformed_answers_deny() {
		let (approvals, frames) = relay(8);
		let owned = approvals.owned(REQUEST);
		for answer in [
			None,
			Some(client_decision(true, "once", "robot")),
			Some(client_decision(true, "", "user")),
		] {
			let request = spawn_request(&owned, amendment(120_000));
			let (_, query) = next_query(&frames).await;
			approvals
				.answer(REQUEST, pb::ApprovalAnswer { query_id: query.query_id, decision: answer });
			let ticket = request.await.expect("request task");
			let decision = decided(&ticket);
			assert!(!decision.approved, "{decision:?}");
			assert_eq!(decision.source, ApprovalSource::Unavailable);
		}
		assert!(frames.is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn answers_that_match_no_open_query_are_ignored() {
		let (approvals, frames) = relay(8);
		let request = spawn_request(&approvals.owned(REQUEST), amendment(120_000));
		let (_, query) = next_query(&frames).await;
		// An unknown query, and the right query on another request.
		approvals.answer(REQUEST, pb::ApprovalAnswer {
			query_id: query.query_id + 100,
			decision: Some(client_decision(true, "once", "user")),
		});
		approvals.answer(REQUEST + 1, pb::ApprovalAnswer {
			query_id: query.query_id,
			decision: Some(client_decision(true, "once", "user")),
		});
		tokio::task::yield_now().await;
		assert!(!request.is_finished(), "a mismatched answer decided the prompt");
		approvals.answer(REQUEST, pb::ApprovalAnswer {
			query_id: query.query_id,
			decision: Some(client_decision(false, "once", "user")),
		});
		let ticket = request.await.expect("request task");
		assert!(!decided(&ticket).approved);
		assert_eq!(decided(&ticket).source, ApprovalSource::User);
		assert!(frames.is_empty(), "ignored answers draw no reply");
	}

	/// The session half on a client whose frames the test plays: queries go in
	/// as daemon frames, answers come out as client frames.
	struct SessionPump {
		route:     ApprovalRelayBinding,
		responses: flume::Sender<pb::ServerFrame>,
		answers:   flume::Receiver<pb::ClientFrame>,
		shutdown:  CancellationToken,
		pump:      JoinHandle<()>,
	}

	/// Bounded wait for the session half, which runs on real threads.
	const PUMP_WAIT: Duration = Duration::from_secs(10);

	impl SessionPump {
		fn start(route: Option<ApprovalRoute>) -> Self {
			let (outgoing, answers) = flume::unbounded();
			let (responses, incoming) = flume::unbounded();
			let client = EnvClient::from_channels(outgoing, incoming);
			let shutdown = CancellationToken::new();
			let mut tasks = Vec::new();
			let bound = spawn_approval_pump(&client, &shutdown, &mut tasks);
			*bound.write() = route;
			let pump = tasks.pop().expect("pump task");
			Self { route: bound, responses, answers, shutdown, pump }
		}

		fn query(&self, request_id: u64, query_id: u64) {
			self
				.responses
				.send(frame(
					request_id,
					server_frame::Body::ApprovalQuery(pb::ApprovalQuery {
						query_id,
						invocation_id: None,
						reasons: vec![wire_spec(&amendment(120_000))],
						created_at_ms: 1_000,
					}),
				))
				.expect("send approval query");
		}

		fn withdraw(&self, request_id: u64, query_id: u64) {
			self
				.responses
				.send(frame(
					request_id,
					server_frame::Body::ApprovalWithdrawn(pb::ApprovalWithdrawn { query_id }),
				))
				.expect("send approval withdrawal");
		}

		async fn answer(&self) -> (u64, Option<pb::InvocationScope>, pb::ApprovalAnswer) {
			let frame = time::timeout(PUMP_WAIT, self.answers.recv_async())
				.await
				.expect("approval answer timed out")
				.expect("approval answer");
			let Some(pb::client_frame::Body::ApprovalAnswer(answer)) = frame.body else {
				panic!("expected an approval answer, got {:?}", frame.body);
			};
			(frame.request_id, frame.scope, answer)
		}
	}

	async fn filed(inbox: &omp_agent::ApprovalInbox) -> omp_agent::ApprovalRequest {
		time::timeout(PUMP_WAIT, inbox.recv())
			.await
			.expect("relayed prompt was not filed")
			.expect("approval inbox")
	}

	async fn abandoned(request: &omp_agent::ApprovalRequest) {
		time::timeout(PUMP_WAIT, async {
			while !request.is_abandoned() {
				time::sleep(Duration::from_millis(5)).await;
			}
		})
		.await
		.expect("the filed prompt was not withdrawn");
	}

	fn session_route() -> (ApprovalRoute, omp_agent::ApprovalInbox) {
		ApprovalRoute::new(Arc::new(omp_agent::ApprovalBook::new()), None)
	}

	#[test]
	fn relayed_requirements_and_decisions_survive_the_wire() {
		let spec = amendment(120_000);
		assert_eq!(spec_from_wire(wire_spec(&spec)), spec);
		let bare = ApprovalSpec {
			default: None,
			approver: Some(sf!("ops")),
			pattern: None,
			evidence: Vec::new(),
			..amendment(0)
		};
		assert_eq!(spec_from_wire(wire_spec(&bare)), bare);
		for (scope, source) in [
			(ApprovalScope::Once, ApprovalSource::User),
			(ApprovalScope::Session, ApprovalSource::Config),
			(ApprovalScope::Custom(sf!("lease")), ApprovalSource::Forwarded),
			(ApprovalScope::Once, ApprovalSource::Timeout),
			(ApprovalScope::Once, ApprovalSource::Unavailable),
		] {
			let decision = ApprovalDecision {
				approved: matches!(source, ApprovalSource::User | ApprovalSource::Config),
				scope,
				source,
				decided_by: Some(sf!("tester")),
				reason: Some(sf!("because")),
				audited: true,
			};
			assert_eq!(decision_from_wire(Some(wire_decision(decision.clone()))), decision);
		}
	}

	#[tokio::test]
	async fn the_session_answers_through_its_bound_route_on_the_query_request() {
		let (route, inbox) = session_route();
		let session = SessionPump::start(Some(route));
		session.query(5, 1);
		let request = filed(&inbox).await;
		assert_eq!(request.ticket.invocation_id, None, "a sandbox amendment blocks no invocation");
		assert_eq!(request.ticket.reasons, vec![amendment(120_000)]);
		assert_eq!(request.ticket.created_at_ms, 1_000);
		let decision = ApprovalDecision {
			approved:   true,
			scope:      ApprovalScope::Once,
			source:     ApprovalSource::User,
			decided_by: Some(sf!("tester")),
			reason:     None,
			audited:    false,
		};
		request
			.respond(decision.clone())
			.expect("the relayed prompt is waiting");
		let (request_id, scope, answer) = session.answer().await;
		assert_eq!((request_id, scope), (5, None), "the answer rides the query's request unscoped");
		assert_eq!(answer.query_id, 1);
		assert_eq!(decision_from_wire(answer.decision), decision);
		session.shutdown.cancel();
		time::timeout(PUMP_WAIT, session.pump)
			.await
			.expect("pump stopped on shutdown")
			.expect("pump task");
	}

	#[tokio::test]
	async fn a_withdrawn_query_cancels_its_prompt_and_is_never_answered() {
		let (route, inbox) = session_route();
		let session = SessionPump::start(Some(route.clone()));
		session.query(5, 1);
		let withdrawn = filed(&inbox).await;
		session.withdraw(5, 1);
		abandoned(&withdrawn).await;
		assert!(route.pending().is_empty(), "the withdrawn prompt is still filed");
		assert!(
			withdrawn.respond(approve()).is_err(),
			"a withdrawn prompt still accepted a decision"
		);

		// The next query is answered first: the withdrawn one never is.
		session.query(6, 2);
		filed(&inbox)
			.await
			.respond(approve())
			.expect("the next prompt is waiting");
		let (request_id, _, answer) = session.answer().await;
		assert_eq!((request_id, answer.query_id), (6, 2));
		assert!(session.answers.is_empty(), "the withdrawn query was answered");
	}

	#[tokio::test]
	async fn a_closed_transport_cancels_open_prompts_without_answering() {
		let (route, inbox) = session_route();
		let session = SessionPump::start(Some(route.clone()));
		session.query(5, 1);
		let open = filed(&inbox).await;
		let SessionPump { responses, answers, pump, .. } = session;
		drop(responses);
		time::timeout(PUMP_WAIT, pump)
			.await
			.expect("pump stopped when the transport closed")
			.expect("pump task");
		abandoned(&open).await;
		assert!(route.pending().is_empty(), "a prompt outlived the transport");
		assert!(answers.is_empty(), "a closed transport drew an answer");
	}

	#[tokio::test]
	async fn an_unbound_session_denies_relayed_prompts_as_unreachable() {
		let session = SessionPump::start(None);
		session.query(5, 1);
		let (request_id, _, answer) = session.answer().await;
		assert_eq!((request_id, answer.query_id), (5, 1));
		assert_eq!(
			answer.decision,
			Some(pb::ApprovalDecision {
				approved:   false,
				scope:      "once".to_owned(),
				source:     "unavailable".to_owned(),
				decided_by: None,
				reason:     Some(UNBOUND.to_owned()),
				audited:    false,
			})
		);

		// Binding the route later answers the next prompt through it.
		let (route, inbox) = session_route();
		*session.route.write() = Some(route);
		session.query(6, 2);
		filed(&inbox)
			.await
			.respond(approve())
			.expect("the bound route files the next prompt");
		let (request_id, _, answer) = session.answer().await;
		assert_eq!((request_id, answer.query_id), (6, 2));
		assert!(answer.decision.expect("decision").approved);
	}

	fn approve() -> ApprovalDecision {
		ApprovalDecision {
			approved:   true,
			scope:      ApprovalScope::Once,
			source:     ApprovalSource::User,
			decided_by: None,
			reason:     None,
			audited:    false,
		}
	}

	#[tokio::test(start_paused = true)]
	async fn prompts_past_the_cap_fail_closed() {
		let (approvals, frames) = relay(RELAY_MAX_PENDING + 1);
		let owned = approvals.owned(REQUEST);
		let waiting = (0..RELAY_MAX_PENDING)
			.map(|_| spawn_request(&owned, amendment(0)))
			.collect::<Vec<_>>();
		for _ in 0..RELAY_MAX_PENDING {
			let _ = next_query(&frames).await;
		}
		let refused = owned.request(None, vec![amendment(0)], 3_000).await;
		assert!(!decided(&refused).approved);
		assert_eq!(decided(&refused).source, ApprovalSource::Unavailable);
		assert!(frames.is_empty(), "a refused prompt sends no query");
		approvals.disconnect();
		for request in waiting {
			assert!(!decided(&request.await.expect("request task")).approved);
		}
	}
}
