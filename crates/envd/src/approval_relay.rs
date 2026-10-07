//! Relays the approval prompts a daemon-side command needs to the connection
//! that issued the command.
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
use omp_proto::env::v1::{self as pb, server_frame};
use parking_lot::Mutex;
use tokio::{runtime::Handle, time};

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

	fn wire_decision(approved: bool, scope: &str, source: &str) -> pb::ApprovalDecision {
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
			decision: Some(wire_decision(true, "once", "user")),
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
			decision: Some(wire_decision(true, "once", "user")),
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
			decision: Some(wire_decision(true, "once", "user")),
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
		for answer in
			[None, Some(wire_decision(true, "once", "robot")), Some(wire_decision(true, "", "user"))]
		{
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
			decision: Some(wire_decision(true, "once", "user")),
		});
		approvals.answer(REQUEST + 1, pb::ApprovalAnswer {
			query_id: query.query_id,
			decision: Some(wire_decision(true, "once", "user")),
		});
		tokio::task::yield_now().await;
		assert!(!request.is_finished(), "a mismatched answer decided the prompt");
		approvals.answer(REQUEST, pb::ApprovalAnswer {
			query_id: query.query_id,
			decision: Some(wire_decision(false, "once", "user")),
		});
		let ticket = request.await.expect("request task");
		assert!(!decided(&ticket).approved);
		assert_eq!(decided(&ticket).source, ApprovalSource::User);
		assert!(frames.is_empty(), "ignored answers draw no reply");
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
