//! Per-session replay bridge for read-only session attach (ADR 0039 A6).
//!
//! One [`SessionBridge`] exists per attached session. It owns the kernel's
//! in-process DOM subscription and fans it out to any number of subscribers
//! without ever blocking the kernel actor:
//!
//! - a drain task owns the `flume::Receiver<Event>` handed back by
//!   `Up::Subscribe` and never awaits anything but that receiver, so the
//!   kernel's unbounded subscription channel stays drained;
//! - every drained event is appended to a **bounded replay ring**, bounded both
//!   by an event count ([`SV_CONTROL_REPLAY_EVENTS`]) and by a byte total
//!   ([`SV_CONTROL_REPLAY_BYTES`]), evicted from the front;
//! - each event carries a host-assigned [`Seq`]. `Patch::cause` is *not* a
//!   cursor: a session switch republishes the same entry id, so it cannot order
//!   the stream;
//! - each subscriber has a bounded outbound queue
//!   ([`SV_CONTROL_SUBSCRIBER_BYTES`]). Consecutive `Stream` appends for one
//!   `sid` merge losslessly while queued; past the bound the subscriber is
//!   resynced rather than silently gapped.
//!
//! [`Event::Reset`] is an ordinary ring entry, never a resync: a client that
//! resumes before a reset replays the reset in position.
//!
//! Redaction is applied per subscriber on the way out, so the ring holds one
//! canonical encoding shared by every unredacted subscriber. The redaction
//! base is the bridge's current replica materialization. For a live event
//! that is the state the event just produced; for a replayed historical event
//! it is a later state, so a node removed after the event no longer resolves
//! and its values are blanked. Redaction therefore only ever over-blanks on
//! replay, and never leaks.

use std::{collections::VecDeque, sync::Arc};

use bytes::Bytes;
use omp_agent::DomSubscription;
use omp_core::{FastHashMap, StrMut};
use omp_dom::{
	Dom, Event, PropPolicy, Snapshot, StreamOp, redact_event,
	wire::{self, encode_event},
};
use parking_lot::Mutex;
use smallvec::SmallVec;
use thiserror::Error;

use crate::sessions::SessionId;

/// Default replay-ring depth, in events, retained per attached session.
pub const DEFAULT_REPLAY_EVENTS: u32 = 4096;
/// Default replay-ring budget, in bytes, retained per attached session.
pub const DEFAULT_REPLAY_BYTES: u32 = 16 * 1024 * 1024;
/// Default outbound budget, in bytes, held for one attached subscriber.
pub const DEFAULT_SUBSCRIBER_BYTES: u32 = 4 * 1024 * 1024;

omp_con::var! {
	/// Replay-ring depth, in events, retained per attached session.
	pub static SV_CONTROL_REPLAY_EVENTS = sv_control_replay_events: u32 {
		default: DEFAULT_REPLAY_EVENTS,
		min: 1,
		flags: archive,
	};
	/// Replay-ring budget, in canonical event bytes, per attached session.
	///
	/// The newest event is always retained, so a single event larger than
	/// this budget still replays.
	pub static SV_CONTROL_REPLAY_BYTES = sv_control_replay_bytes: u32 {
		default: DEFAULT_REPLAY_BYTES,
		min: 1,
		flags: archive,
	};
	/// Outbound queue budget, in canonical event bytes, per subscriber.
	///
	/// A subscriber whose queue exceeds this budget is resynced: its queue is
	/// dropped and it receives [`Delivery::Resync`] followed by a fresh
	/// snapshot.
	pub static SV_CONTROL_SUBSCRIBER_BYTES = sv_control_subscriber_bytes: u32 {
		default: DEFAULT_SUBSCRIBER_BYTES,
		min: 1,
		flags: archive,
	};
}

/// Bounds a bridge places on its replay ring and on each subscriber.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BridgeLimits {
	/// Maximum number of events retained in the replay ring.
	pub replay_events:    usize,
	/// Maximum canonical byte total retained in the replay ring.
	pub replay_bytes:     usize,
	/// Maximum canonical byte total queued for one subscriber.
	pub subscriber_bytes: usize,
}

impl BridgeLimits {
	/// Reads the operator's live configuration.
	#[must_use]
	pub fn from_con(ctx: &omp_con::Ctx) -> Self {
		Self {
			replay_events:    widen(SV_CONTROL_REPLAY_EVENTS.get(ctx)),
			replay_bytes:     widen(SV_CONTROL_REPLAY_BYTES.get(ctx)),
			subscriber_bytes: widen(SV_CONTROL_SUBSCRIBER_BYTES.get(ctx)),
		}
	}
}

impl Default for BridgeLimits {
	fn default() -> Self {
		Self {
			replay_events:    widen(DEFAULT_REPLAY_EVENTS),
			replay_bytes:     widen(DEFAULT_REPLAY_BYTES),
			subscriber_bytes: widen(DEFAULT_SUBSCRIBER_BYTES),
		}
	}
}

fn widen(value: u32) -> usize {
	usize::try_from(value).unwrap_or(usize::MAX)
}

/// Identifies one bridge start.
///
/// Minted randomly whenever a bridge starts, so a cursor issued by a previous
/// host process can never be mistaken for one of this bridge's sequences.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Epoch(u64);

impl Epoch {
	/// Mints a fresh epoch.
	#[must_use]
	pub fn fresh() -> Self {
		use rand::RngExt as _;
		Self(rand::rng().random::<u64>())
	}

	/// Returns the epoch as its wire integer.
	#[must_use]
	pub const fn get(self) -> u64 {
		self.0
	}

	/// Reconstructs an epoch received from a client cursor.
	#[must_use]
	pub const fn from_wire(value: u64) -> Self {
		Self(value)
	}
}

/// A bridge-assigned event sequence.
///
/// Monotonic from [`Seq::START`] within one [`Epoch`]. Sequence zero names
/// the state before the bridge's first event.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Seq(u64);

impl Seq {
	/// The sequence assigned to a bridge's first event.
	pub const START: Self = Self(1);
	/// The sequence of a bridge that has not yet published an event.
	pub const ZERO: Self = Self(0);

	/// Returns the sequence as its wire integer.
	#[must_use]
	pub const fn get(self) -> u64 {
		self.0
	}

	/// Reconstructs a sequence received from a client cursor.
	#[must_use]
	pub const fn from_wire(value: u64) -> Self {
		Self(value)
	}

	const fn next(self) -> Self {
		Self(self.0.saturating_add(1))
	}
}

/// A resume position: the session, the bridge start, and the last sequence
/// the client observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cursor {
	/// Session the cursor belongs to.
	pub session: SessionId,
	/// Bridge start that issued the sequence.
	pub epoch:   Epoch,
	/// Last sequence the client observed.
	pub seq:     Seq,
}

/// Why a subscriber's stream restarted from a fresh snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ResyncReason {
	/// The cursor named a different session.
	Session,
	/// The cursor was issued by a different bridge start.
	Epoch,
	/// The cursor named a sequence this bridge has not published.
	Unknown,
	/// The requested sequence has been evicted from the replay ring.
	Evicted,
	/// The subscriber's outbound queue exceeded its byte budget.
	Lagging,
	/// The subscriber's redacted projection could not be encoded.
	Projection,
}

/// One item handed to a subscriber, in delivery order.
#[derive(Clone, Debug)]
pub enum Delivery {
	/// The stream restarts; the next delivery is a [`Delivery::Welcome`].
	Resync {
		/// Why the stream restarted.
		reason: ResyncReason,
	},
	/// A complete materialization and the sequence it was taken at.
	Welcome {
		/// Resume position the snapshot corresponds to.
		cursor:   Cursor,
		/// Materialization, redacted for this subscriber.
		snapshot: Arc<Snapshot>,
	},
	/// One ordered event as canonical `omp_dom` envelope bytes.
	Event {
		/// Resume position after this event.
		cursor: Cursor,
		/// Canonical `omp_dom` envelope bytes, redacted for this subscriber.
		bytes:  Bytes,
	},
}

/// A fault that ends a bridge's drain loop.
#[derive(Debug, Error)]
pub enum BridgeFault {
	/// The bridge replica refused an event the kernel published.
	#[error("the session bridge replica rejected a kernel DOM event")]
	Replica(#[from] omp_dom::DomError),
	/// An event the kernel published could not be encoded canonically.
	#[error("a kernel DOM event could not be encoded as canonical bytes")]
	Encode(#[from] serde_json::Error),
}

/// One admitted event.
///
/// `bytes` is the single canonical encoding every unredacted subscriber
/// shares; `event` is retained so a redacting subscriber can project it
/// without decoding.
struct Entry {
	seq:   Seq,
	event: Event,
	bytes: Bytes,
}

/// One item queued for a single subscriber.
enum Queued {
	Resync(ResyncReason),
	Welcome { seq: Seq, snapshot: Arc<Snapshot> },
	Event(Arc<Entry>),
}

/// One subscriber's bounded outbound queue.
struct Outbound {
	queue: VecDeque<Queued>,
	bytes: usize,
	wake:  flume::Sender<()>,
}

impl Outbound {
	fn new(wake: flume::Sender<()>) -> Self {
		Self { queue: VecDeque::new(), bytes: 0, wake }
	}

	/// Queues one event, merging it into a queued append for the same stream
	/// when it is itself an append. Returns whether the queue is now over
	/// budget.
	fn push_event(&mut self, entry: Arc<Entry>, limits: &BridgeLimits) -> bool {
		let merged = match self.queue.back() {
			Some(Queued::Event(tail)) => {
				merge_appends(tail, &entry).map(|merged| (tail.bytes.len(), merged))
			},
			_ => None,
		};
		match merged {
			Some((replaced, merged)) => {
				self.bytes = self
					.bytes
					.saturating_sub(replaced)
					.saturating_add(merged.bytes.len());
				if let Some(slot) = self.queue.back_mut() {
					*slot = Queued::Event(Arc::new(merged));
				}
			},
			None => {
				self.bytes = self.bytes.saturating_add(entry.bytes.len());
				self.queue.push_back(Queued::Event(entry));
			},
		}
		self.bytes > limits.subscriber_bytes
	}

	/// Queues a snapshot without a preceding resync, for a fresh subscriber.
	fn welcome(&mut self, seq: Seq, snapshot: Arc<Snapshot>) {
		self.queue.push_back(Queued::Welcome { seq, snapshot });
	}

	/// Drops everything queued and restarts the stream from a snapshot.
	///
	/// The recovery pair is not charged against the byte budget: charging it
	/// could make a subscriber whose snapshot alone exceeds the budget resync
	/// forever.
	fn restart(&mut self, reason: ResyncReason, seq: Seq, snapshot: Arc<Snapshot>) {
		self.queue.clear();
		self.bytes = 0;
		self.queue.push_back(Queued::Resync(reason));
		self.welcome(seq, snapshot);
	}

	/// Signals the subscriber. Returns `false` once it has been dropped.
	fn wake(&self) -> bool {
		!matches!(self.wake.try_send(()), Err(flume::TrySendError::Disconnected(())))
	}
}

/// Merges two consecutive stream appends for one `sid`.
///
/// Lossless: appends concatenate. The merged entry takes the later entry's
/// sequence and cause, so a client that resumes from it has observed both.
fn merge_appends(tail: &Entry, next: &Entry) -> Option<Entry> {
	let (
		Event::Stream { sid: queued, op: StreamOp::Append, text: Some(head), .. },
		Event::Stream { cause, sid, op: StreamOp::Append, text: Some(more), .. },
	) = (&tail.event, &next.event)
	else {
		return None;
	};
	if queued != sid {
		return None;
	}
	let mut text = StrMut::with_capacity(head.len().saturating_add(more.len()));
	text.push_str(head.as_str());
	text.push_str(more.as_str());
	let event = Event::Stream {
		cause: *cause,
		sid:   *sid,
		op:    StreamOp::Append,
		node:  None,
		prop:  None,
		text:  Some(text.freeze()),
	};
	let bytes = Bytes::from(encode_event(&event).ok()?);
	Some(Entry { seq: next.seq, event, bytes })
}

/// Everything behind the bridge's single lock.
struct State {
	replica:     Dom,
	base:        Option<Arc<Snapshot>>,
	ring:        VecDeque<Arc<Entry>>,
	ring_bytes:  usize,
	head:        Seq,
	subscribers: FastHashMap<u64, Outbound>,
	next_id:     u64,
	closed:      bool,
}

impl State {
	/// Materializes the replica once per advance, shared by every subscriber
	/// that needs a redaction base or a fresh snapshot.
	fn base(&mut self) -> Arc<Snapshot> {
		let replica = &self.replica;
		Arc::clone(
			self
				.base
				.get_or_insert_with(|| Arc::new(replica.snapshot())),
		)
	}

	fn drop_front(&mut self) {
		if let Some(entry) = self.ring.pop_front() {
			self.ring_bytes = self.ring_bytes.saturating_sub(entry.bytes.len());
		}
	}

	/// Evicts from the front until both ring bounds hold. The newest entry is
	/// always retained.
	fn evict(&mut self, limits: &BridgeLimits) {
		while self.ring.len() > limits.replay_events.max(1) {
			self.drop_front();
		}
		while self.ring_bytes > limits.replay_bytes && self.ring.len() > 1 {
			self.drop_front();
		}
	}

	fn fanout(&mut self, entry: &Arc<Entry>, limits: &BridgeLimits) {
		let mut lagging: SmallVec<u64, 4> = SmallVec::new();
		let mut gone: SmallVec<u64, 4> = SmallVec::new();
		for (&id, out) in &mut self.subscribers {
			if out.push_event(Arc::clone(entry), limits) {
				lagging.push(id);
			}
			if !out.wake() {
				gone.push(id);
			}
		}
		for id in gone {
			self.subscribers.remove(&id);
		}
		if lagging.is_empty() {
			return;
		}
		let head = self.head;
		let base = self.base();
		for id in lagging {
			if let Some(out) = self.subscribers.get_mut(&id) {
				out.restart(ResyncReason::Lagging, head, Arc::clone(&base));
				out.wake();
			}
		}
	}

	fn wake_all(&self) {
		for out in self.subscribers.values() {
			out.wake();
		}
	}
}

/// What a pop found in a subscriber's queue.
enum Pop {
	Ready(Delivery),
	Empty,
	Closed,
}

struct Shared {
	session: SessionId,
	epoch:   Epoch,
	limits:  BridgeLimits,
	state:   Mutex<State>,
}

impl Shared {
	fn cursor(&self, seq: Seq) -> Cursor {
		Cursor { session: self.session.clone(), epoch: self.epoch, seq }
	}

	/// Appends one drained event to the ring and fans it out.
	fn admit(&self, event: Event) -> Result<(), BridgeFault> {
		let bytes = Bytes::from(encode_event(&event)?);
		let mut state = self.state.lock();
		state.replica.apply_event(&event)?;
		state.base = None;
		let seq = state.head.next();
		state.head = seq;
		let entry = Arc::new(Entry { seq, event, bytes });
		state.ring_bytes = state.ring_bytes.saturating_add(entry.bytes.len());
		state.ring.push_back(Arc::clone(&entry));
		state.evict(&self.limits);
		state.fanout(&entry, &self.limits);
		Ok(())
	}

	fn close(&self) {
		let mut state = self.state.lock();
		state.closed = true;
		state.wake_all();
	}

	/// Classifies a resume cursor, returning why it cannot be served.
	fn unserviceable(&self, state: &State, cursor: &Cursor) -> Option<ResyncReason> {
		if cursor.session != self.session {
			return Some(ResyncReason::Session);
		}
		if cursor.epoch != self.epoch {
			return Some(ResyncReason::Epoch);
		}
		if cursor.seq > state.head {
			return Some(ResyncReason::Unknown);
		}
		if cursor.seq == state.head {
			return None;
		}
		match state.ring.front() {
			Some(front) if front.seq <= cursor.seq.next() => None,
			_ => Some(ResyncReason::Evicted),
		}
	}

	fn pop(&self, id: u64, policy: Option<&dyn PropPolicy>) -> Pop {
		let mut state = self.state.lock();
		let Some(out) = state.subscribers.get_mut(&id) else {
			return Pop::Closed;
		};
		let item = out.queue.pop_front();
		if let Some(Queued::Event(entry)) = item.as_ref() {
			out.bytes = out.bytes.saturating_sub(entry.bytes.len());
		}
		match item {
			None => {
				if state.closed {
					Pop::Closed
				} else {
					Pop::Empty
				}
			},
			Some(Queued::Resync(reason)) => Pop::Ready(Delivery::Resync { reason }),
			Some(Queued::Welcome { seq, snapshot }) => {
				let snapshot = match policy {
					Some(policy) => Arc::new(snapshot.redact(policy)),
					None => snapshot,
				};
				Pop::Ready(Delivery::Welcome { cursor: self.cursor(seq), snapshot })
			},
			Some(Queued::Event(entry)) => {
				let Some(policy) = policy else {
					return Pop::Ready(Delivery::Event {
						cursor: self.cursor(entry.seq),
						bytes:  entry.bytes.clone(),
					});
				};
				let base = state.base();
				let projected = redact_event(entry.event.clone(), &base, policy);
				match encode_event(&projected) {
					Ok(bytes) => Pop::Ready(Delivery::Event {
						cursor: self.cursor(entry.seq),
						bytes:  Bytes::from(bytes),
					}),
					Err(error) => {
						tracing::error!(
							session = %self.session,
							seq = entry.seq.get(),
							?error,
							"redacted session projection could not be encoded",
						);
						let head = state.head;
						let base = state.base();
						let Some(out) = state.subscribers.get_mut(&id) else {
							return Pop::Closed;
						};
						out.restart(ResyncReason::Projection, head, base);
						out.queue.pop_front();
						Pop::Ready(Delivery::Resync { reason: ResyncReason::Projection })
					},
				}
			},
		}
	}
}

/// A read-only fan-out of one session's DOM subscription.
///
/// Dropping the bridge stops its drain task and closes every subscriber.
pub struct SessionBridge {
	shared: Arc<Shared>,
	drain:  tokio::task::JoinHandle<()>,
}

impl SessionBridge {
	/// Starts a bridge over one kernel subscription.
	///
	/// The initial snapshot seeds the bridge's replica; the receiver is
	/// drained by a task spawned on the current runtime.
	#[must_use]
	pub fn start(session: SessionId, subscription: DomSubscription, limits: BridgeLimits) -> Self {
		let (snapshot, events) = subscription;
		let shared = Arc::new(Shared {
			session,
			epoch: Epoch::fresh(),
			limits,
			state: Mutex::new(State {
				replica:     Dom::from_snapshot(&snapshot),
				base:        Some(Arc::new(snapshot)),
				ring:        VecDeque::new(),
				ring_bytes:  0,
				head:        Seq::ZERO,
				subscribers: FastHashMap::default(),
				next_id:     0,
				closed:      false,
			}),
		});
		let drain = tokio::spawn(drain(Arc::clone(&shared), events));
		Self { shared, drain }
	}

	/// Returns the session this bridge serves.
	#[must_use]
	pub fn session(&self) -> &SessionId {
		&self.shared.session
	}

	/// Returns the epoch minted when this bridge started.
	#[must_use]
	pub fn epoch(&self) -> Epoch {
		self.shared.epoch
	}

	/// Returns the newest sequence this bridge has published.
	#[must_use]
	pub fn head(&self) -> Seq {
		self.shared.state.lock().head
	}

	/// Returns the oldest sequence still replayable from the ring.
	#[must_use]
	pub fn oldest(&self) -> Option<Seq> {
		self.shared.state.lock().ring.front().map(|entry| entry.seq)
	}

	/// Attaches a subscriber.
	///
	/// With no cursor the subscriber opens on a snapshot at the current head.
	/// With a cursor this bridge can serve, it replays `(seq, head]` and no
	/// snapshot is sent. Any other cursor opens with [`Delivery::Resync`]
	/// followed by a snapshot taken at the head sequence under the same lock,
	/// so no event is lost or duplicated across the boundary.
	#[must_use]
	pub fn subscribe(&self, cursor: Option<&Cursor>) -> Subscriber {
		let (wake_tx, wake_rx) = flume::bounded(1);
		let limits = &self.shared.limits;
		let mut state = self.shared.state.lock();
		let id = state.next_id;
		state.next_id = state.next_id.saturating_add(1);
		let mut out = Outbound::new(wake_tx);
		let refused = cursor.and_then(|cursor| self.shared.unserviceable(&state, cursor));
		match (cursor, refused) {
			(Some(cursor), None) => {
				let mut over = false;
				for entry in state.ring.iter().filter(|entry| entry.seq > cursor.seq) {
					over |= out.push_event(Arc::clone(entry), limits);
				}
				if over {
					let head = state.head;
					let base = state.base();
					out.restart(ResyncReason::Lagging, head, base);
				}
			},
			(Some(_), Some(reason)) => {
				let head = state.head;
				let base = state.base();
				out.restart(reason, head, base);
			},
			(None, _) => {
				let head = state.head;
				let base = state.base();
				out.welcome(head, base);
			},
		}
		out.wake();
		state.subscribers.insert(id, out);
		Subscriber { shared: Arc::clone(&self.shared), id, wake: wake_rx }
	}
}

impl Drop for SessionBridge {
	fn drop(&mut self) {
		self.drain.abort();
		self.shared.close();
	}
}

/// Drains the kernel subscription into the ring.
///
/// The loop awaits only the subscription receiver; every other step is
/// non-blocking, so a slow subscriber can never stall the kernel.
async fn drain(shared: Arc<Shared>, events: flume::Receiver<Event>) {
	while let Ok(event) = events.recv_async().await {
		if let Err(fault) = shared.admit(event) {
			tracing::error!(
				session = %shared.session,
				error = ?fault,
				"session bridge stopped draining its kernel subscription",
			);
			break;
		}
	}
	shared.close();
}

/// One attached reader of a [`SessionBridge`].
pub struct Subscriber {
	shared: Arc<Shared>,
	id:     u64,
	wake:   flume::Receiver<()>,
}

impl Subscriber {
	/// Returns the next delivery without waiting, projecting through
	/// `policy` when one is supplied.
	#[must_use]
	pub fn try_next(&self, policy: Option<&dyn PropPolicy>) -> Option<Delivery> {
		match self.shared.pop(self.id, policy) {
			Pop::Ready(delivery) => Some(delivery),
			Pop::Empty | Pop::Closed => None,
		}
	}

	/// Awaits the next delivery, projecting through `policy` when one is
	/// supplied. Returns `None` once the bridge has closed and the queue is
	/// drained.
	pub async fn next(&self, policy: Option<&dyn PropPolicy>) -> Option<Delivery> {
		loop {
			match self.shared.pop(self.id, policy) {
				Pop::Ready(delivery) => return Some(delivery),
				Pop::Closed => return None,
				Pop::Empty => {
					if self.wake.recv_async().await.is_err() {
						return None;
					}
				},
			}
		}
	}
}

impl Drop for Subscriber {
	fn drop(&mut self) {
		self.shared.state.lock().subscribers.remove(&self.id);
	}
}

/// Decodes one delivered event for a caller that needs the typed form.
///
/// The bridge delivers canonical bytes; this is the inverse every observer
/// that materializes a replica applies.
pub fn decode_delivered(bytes: &[u8]) -> Result<Event, wire::EventDecodeError> {
	wire::decode_event(bytes)
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use omp_core::Str;
	use omp_dom::{Handle, KnownTag, NodeSpec, Op, PropId, Sid, Txn, Value};
	use omp_journal::EntryId;

	use super::*;

	/// Mirrors the kernel: a controller-side `Dom` whose published events are
	/// forwarded verbatim into a bridge.
	struct Producer {
		dom:   Dom,
		drain: flume::Receiver<Event>,
		to:    flume::Sender<Event>,
		turn:  Handle,
	}

	impl Producer {
		/// Seeds a `<turn>` before subscribing, so it belongs to the bridge's
		/// initial snapshot and costs no sequence.
		fn start(limits: BridgeLimits) -> (Self, SessionBridge) {
			let mut dom = Dom::new();
			let body = dom.body();
			dom.apply(&Txn {
				cause: EntryId::default(),
				label: None,
				ops:   vec![Op::Ins {
					parent: body,
					after:  None,
					node:   NodeSpec::new(KnownTag::Turn),
				}],
			})
			.expect("a turn inserts under the body");
			let turn = dom.children(body).last().copied().expect("the seeded turn");
			let (snapshot, drain) = dom.subscribe();
			let (to, events) = flume::unbounded();
			let bridge = SessionBridge::start(
				SessionId::new_static("session-under-test"),
				(snapshot, events),
				limits,
			);
			(Self { dom, drain, to, turn }, bridge)
		}

		/// Publishes whatever `edit` produces and forwards it to the bridge.
		fn publish(&mut self, edit: impl FnOnce(&mut Dom)) {
			edit(&mut self.dom);
			for event in self.drain.try_iter() {
				self.to.send(event).expect("bridge drain task is alive");
			}
		}

		fn insert(&mut self, node: NodeSpec) {
			let parent = self.turn;
			self.publish(|dom| {
				dom.apply(&Txn {
					cause: EntryId::default(),
					label: None,
					ops:   vec![Op::Ins { parent, after: None, node }],
				})
				.expect("insert applies");
			});
		}

		fn turn(&mut self, text: &str) {
			self.insert(
				NodeSpec::new(KnownTag::User).with_prop(PropId::Text, Value::Str(Str::new(text))),
			);
		}

		fn assistant(&mut self) -> Handle {
			self.insert(NodeSpec::new(KnownTag::Assistant));
			self
				.dom
				.children(self.turn)
				.last()
				.copied()
				.expect("inserted node")
		}

		fn open_stream(&mut self, node: Handle) -> Sid {
			let mut sid = None;
			self.publish(|dom| {
				sid = Some(
					dom.stream_open(EntryId::default(), node, PropId::Text.into())
						.expect("assistant text accepts a stream"),
				);
			});
			sid.expect("stream opened")
		}

		fn append(&mut self, sid: Sid, text: &str) {
			self.publish(|dom| {
				dom.stream_append(EntryId::default(), sid, text)
					.expect("open stream appends");
			});
		}

		fn reset(&mut self) {
			let snapshot = self.dom.snapshot();
			self.publish(|dom| dom.reset(snapshot));
		}
	}

	fn limits(replay_events: usize, replay_bytes: usize, subscriber_bytes: usize) -> BridgeLimits {
		BridgeLimits { replay_events, replay_bytes, subscriber_bytes }
	}

	/// Waits until the bridge has admitted `seq`, so a test observes the ring
	/// after the drain task has caught up.
	async fn settle(bridge: &SessionBridge, seq: Seq) {
		tokio::time::timeout(Duration::from_secs(5), async {
			while bridge.head() < seq {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("drain task admits every published event");
	}

	fn drain_ready(subscriber: &Subscriber) -> Vec<Delivery> {
		let mut out = Vec::new();
		while let Some(delivery) = subscriber.try_next(None) {
			out.push(delivery);
		}
		out
	}

	fn event_of(delivery: &Delivery) -> Event {
		let Delivery::Event { bytes, .. } = delivery else {
			panic!("delivery is an event, not {delivery:?}");
		};
		decode_delivered(bytes).expect("bridge delivers canonical bytes")
	}

	fn seq_of(delivery: &Delivery) -> Seq {
		match delivery {
			Delivery::Event { cursor, .. } | Delivery::Welcome { cursor, .. } => cursor.seq,
			Delivery::Resync { reason } => panic!("resync {reason} carries no sequence"),
		}
	}

	#[tokio::test]
	async fn ring_evicts_by_event_count() {
		let (mut producer, bridge) = Producer::start(limits(3, usize::MAX, usize::MAX));
		for index in 0..5u8 {
			producer.turn(&format!("turn {index}"));
		}
		settle(&bridge, Seq::from_wire(5)).await;

		assert_eq!(bridge.head(), Seq::from_wire(5));
		assert_eq!(bridge.oldest(), Some(Seq::from_wire(3)));
	}

	/// Canonical byte length of each event `turns` calls to
	/// [`Producer::turn`] publish, measured on a throwaway bridge so a budget
	/// can be stated exactly rather than approximated.
	async fn event_sizes(turns: u8) -> Vec<usize> {
		let (mut producer, bridge) = Producer::start(limits(usize::MAX, usize::MAX, usize::MAX));
		let reader = bridge.subscribe(None);
		for index in 0..turns {
			producer.turn(&format!("turn {index}"));
		}
		settle(&bridge, Seq::from_wire(u64::from(turns))).await;
		drain_ready(&reader)
			.iter()
			.filter_map(|delivery| match delivery {
				Delivery::Event { bytes, .. } => Some(bytes.len()),
				_ => None,
			})
			.collect()
	}

	#[tokio::test]
	async fn ring_evicts_by_byte_total() {
		// Budget exactly the last two events, so the front sequence is
		// arithmetic rather than approximate.
		let sizes = event_sizes(4).await;
		let budget = sizes[2] + sizes[3];

		let (mut producer, bridge) = Producer::start(limits(16, budget, usize::MAX));
		for index in 0..4u8 {
			producer.turn(&format!("turn {index}"));
		}
		settle(&bridge, Seq::from_wire(4)).await;

		assert_eq!(bridge.head(), Seq::from_wire(4));
		assert_eq!(bridge.oldest(), Some(Seq::from_wire(3)));
	}

	#[tokio::test]
	async fn reset_replays_in_position_without_a_resync() {
		let (mut producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		let subscriber = bridge.subscribe(None);
		producer.turn("before");
		producer.reset();
		producer.turn("after");
		settle(&bridge, Seq::from_wire(3)).await;

		let delivered = drain_ready(&subscriber);
		assert!(
			!delivered
				.iter()
				.any(|delivery| matches!(delivery, Delivery::Resync { .. })),
			"a reset is an ordinary ring entry",
		);
		assert_eq!(seq_of(&delivered[0]), Seq::ZERO);
		assert!(matches!(delivered[0], Delivery::Welcome { .. }));
		assert!(matches!(event_of(&delivered[1]), Event::Patch(_)));
		assert!(matches!(event_of(&delivered[2]), Event::Reset { .. }));
		assert!(matches!(event_of(&delivered[3]), Event::Patch(_)));
		assert_eq!(seq_of(&delivered[3]), Seq::from_wire(3));
	}

	#[tokio::test]
	async fn resuming_before_a_reset_replays_the_reset_in_position() {
		let (mut producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		producer.turn("before");
		producer.reset();
		producer.turn("after");
		settle(&bridge, Seq::from_wire(3)).await;

		let cursor = Cursor {
			session: bridge.session().clone(),
			epoch:   bridge.epoch(),
			seq:     Seq::from_wire(1),
		};
		let subscriber = bridge.subscribe(Some(&cursor));
		let delivered = drain_ready(&subscriber);

		assert_eq!(delivered.len(), 2, "replay is (1, 3]");
		assert!(matches!(event_of(&delivered[0]), Event::Reset { .. }));
		assert_eq!(seq_of(&delivered[0]), Seq::from_wire(2));
		assert!(matches!(event_of(&delivered[1]), Event::Patch(_)));
	}

	#[tokio::test]
	async fn stale_epoch_resyncs_then_welcomes() {
		let (mut producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		producer.turn("first");
		settle(&bridge, Seq::START).await;

		let cursor = Cursor {
			session: bridge.session().clone(),
			epoch:   Epoch::from_wire(bridge.epoch().get() ^ 1),
			seq:     Seq::START,
		};
		let delivered = drain_ready(&bridge.subscribe(Some(&cursor)));

		assert_eq!(delivered.len(), 2);
		assert!(matches!(delivered[0], Delivery::Resync { reason: ResyncReason::Epoch }));
		let Delivery::Welcome { cursor, snapshot } = &delivered[1] else {
			panic!("a resync is followed by a welcome");
		};
		assert_eq!(cursor.seq, Seq::START);
		assert_eq!(cursor.epoch, bridge.epoch());
		assert_eq!(
			snapshot.as_ref(),
			&producer.dom.snapshot(),
			"the fresh snapshot is the replica at the head sequence",
		);
	}

	#[tokio::test]
	async fn evicted_sequence_resyncs_then_welcomes() {
		let (mut producer, bridge) = Producer::start(limits(2, usize::MAX, usize::MAX));
		for index in 0..5u8 {
			producer.turn(&format!("turn {index}"));
		}
		settle(&bridge, Seq::from_wire(5)).await;

		let cursor =
			Cursor { session: bridge.session().clone(), epoch: bridge.epoch(), seq: Seq::START };
		let delivered = drain_ready(&bridge.subscribe(Some(&cursor)));

		assert_eq!(delivered.len(), 2);
		assert!(matches!(delivered[0], Delivery::Resync { reason: ResyncReason::Evicted }));
		assert_eq!(seq_of(&delivered[1]), Seq::from_wire(5));
		assert!(matches!(delivered[1], Delivery::Welcome { .. }));
	}

	#[tokio::test]
	async fn queued_stream_appends_merge_into_exactly_the_unmerged_text() {
		let (mut producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		let eager = bridge.subscribe(None);
		let lazy = bridge.subscribe(None);
		let node = producer.assistant();
		let sid = producer.open_stream(node);

		// The eager subscriber reads between appends, so nothing merges.
		let mut unmerged = String::new();
		for chunk in ["al", "pha", "-beta"] {
			producer.append(sid, chunk);
			loop {
				let delivery = tokio::time::timeout(Duration::from_secs(5), eager.next(None))
					.await
					.expect("bridge delivers")
					.expect("bridge is open");
				if !matches!(delivery, Delivery::Event { .. }) {
					continue;
				}
				if let Event::Stream { text: Some(text), op: StreamOp::Append, .. } =
					event_of(&delivery)
				{
					unmerged.push_str(text.as_str());
					break;
				}
			}
		}
		settle(&bridge, Seq::from_wire(5)).await;

		let delivered = drain_ready(&lazy);
		let appends: Vec<Event> = delivered
			.iter()
			.filter(|delivery| matches!(delivery, Delivery::Event { .. }))
			.map(event_of)
			.filter(|event| matches!(event, Event::Stream { op: StreamOp::Append, .. }))
			.collect();

		assert_eq!(appends.len(), 1, "three queued appends merge into one");
		let Event::Stream { text: Some(merged), .. } = &appends[0] else {
			panic!("the merged delivery is an append");
		};
		assert_eq!(merged.as_str(), unmerged);
		assert_eq!(unmerged, "alpha-beta");
		assert_eq!(
			seq_of(delivered.last().expect("a delivery")),
			Seq::from_wire(5),
			"the merged entry carries the later sequence",
		);
	}

	#[tokio::test]
	async fn a_subscriber_past_its_byte_bound_resyncs_without_a_sequence_gap() {
		let one = event_sizes(1).await[0];

		let (mut producer, bridge) = Producer::start(limits(16, usize::MAX, one));
		let subscriber = bridge.subscribe(None);
		producer.turn("turn 0");
		producer.turn("turn 1");
		settle(&bridge, Seq::from_wire(2)).await;
		producer.turn("turn 2");
		producer.turn("turn 3");
		settle(&bridge, Seq::from_wire(4)).await;

		let delivered = drain_ready(&subscriber);
		assert!(matches!(delivered[0], Delivery::Resync { reason: ResyncReason::Lagging }));
		let Delivery::Welcome { cursor: welcome, .. } = &delivered[1] else {
			panic!("a resync is followed by a welcome");
		};
		let mut expected = welcome.seq;
		for delivery in &delivered[2..] {
			expected = expected.next();
			assert_eq!(seq_of(delivery), expected, "no sequence gap after a resync");
		}
		assert_eq!(expected, bridge.head(), "the subscriber caught up to the head");
	}

	#[tokio::test]
	async fn the_drain_task_never_blocks_the_sender_while_the_ring_is_saturated() {
		let mut dom = Dom::new();
		let body = dom.body();
		dom.apply(&Txn {
			cause: EntryId::default(),
			label: None,
			ops:   vec![Op::Ins { parent: body, after: None, node: NodeSpec::new(KnownTag::Turn) }],
		})
		.expect("a turn inserts under the body");
		let turn = dom.children(body).last().copied().expect("the seeded turn");
		let (snapshot, published) = dom.subscribe();
		// One slot: every send blocks unless the drain task is pulling.
		let (to, events) = flume::bounded(1);
		let bridge = SessionBridge::start(
			SessionId::new_static("saturated"),
			(snapshot, events),
			// A one-event ring and a one-byte subscriber budget keep both
			// bounds saturated for the whole run.
			limits(1, 1, 1),
		);
		let _stalled = bridge.subscribe(None);

		tokio::time::timeout(Duration::from_secs(30), async {
			for index in 0..512u32 {
				dom.apply(&Txn {
					cause: EntryId::default(),
					label: None,
					ops:   vec![Op::Ins {
						parent: turn,
						after:  None,
						node:   NodeSpec::new(KnownTag::User)
							.with_prop(PropId::Text, Value::Int(i64::from(index))),
					}],
				})
				.expect("insert applies");
				for event in published.try_iter() {
					to.send_async(event)
						.await
						.expect("the drain task keeps pulling");
				}
			}
		})
		.await
		.expect("a saturated ring never blocks the kernel");

		settle(&bridge, Seq::from_wire(512)).await;
		assert_eq!(bridge.head(), Seq::from_wire(512));
		assert_eq!(bridge.oldest(), Some(Seq::from_wire(512)), "the newest entry is retained");
	}
}
