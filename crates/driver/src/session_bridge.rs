//! Per-session replay bridge for read-only session attach (ADR 0039 A6).
//!
//! One [`SessionBridge`] exists per attached session. It owns the kernel's
//! in-process DOM subscription and fans it out to any number of subscribers
//! without ever blocking the kernel actor:
//!
//! - a drain task owns the `flume::Receiver<Event>` handed back by
//!   `Up::Subscribe` and never awaits anything but that receiver. That channel
//!   is **bounded**: `Dom::subscribe` opens `flume::bounded(512)` and
//!   `Dom::publish` fans out with `retain(|s| s.try_send(..).is_ok())`, so a
//!   kernel that cannot place an event drops the subscriber instead of blocking
//!   on it. Draining promptly is therefore not an optimization but the only
//!   thing that keeps this bridge subscribed, and a disconnect is an arbitrary
//!   truncation rather than an end of stream. [`Closure`] names the two exits
//!   apart so a projection can fault instead of reporting success;
//! - every drained event is appended to a **bounded replay ring**, bounded both
//!   by an event count ([`SV_CONTROL_REPLAY_EVENTS`]) and by a byte total
//!   ([`SV_CONTROL_REPLAY_BYTES`]), evicted from the front;
//! - each event carries a host-assigned [`Seq`]. `Patch::cause` is *not* a
//!   cursor: a session switch republishes the same entry id, so it cannot order
//!   the stream;
//! - each subscriber has a bounded outbound queue
//!   ([`SV_CONTROL_SUBSCRIBER_BYTES`]). Consecutive `Stream` appends for one
//!   `sid` merge losslessly while queued; past the bound the subscriber is
//!   resynced rather than silently gapped. A merged append is encoded only on
//!   the way out, so merging costs one text copy and no re-encode.
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
//!
//! That no-leak property rests on an `omp_dom` invariant neither crate states:
//! **a handle must name the same node in the base as it did in the event**,
//! because redaction resolves a handle against the base to find the tag whose
//! policy applies. `omp_dom` mints handles from a monotonic high-water mark,
//! which holds within one replica generation. `Dom::reset` adopts the incoming
//! snapshot's mark, and a snapshot of a shorter branch carries a lower one, so
//! a handle minted before a reset can be reissued to an unrelated node after
//! it. A ring entry older than such a reset, replayed to a redacting
//! subscriber, is projected against a base in which its handles may resolve to
//! differently tagged nodes, and can be under-blanked. The exposure is bounded
//! to entries that predate a mark-lowering reset and are still replayable;
//! closing it needs a per-entry redaction base, which `omp_dom` has no API to
//! materialize incrementally.
//!
//! Materializing the base is also why a redacting subscriber is the expensive
//! one: every admitted event invalidates it, so the next redacted delivery
//! pays one `Dom::snapshot` under the bridge lock. Redaction and encoding
//! themselves run with the lock released.

use std::{collections::VecDeque, sync::Arc};

use bytes::Bytes;
use omp_agent::DomSubscription;
use omp_core::{FastHashMap, Str, StrMut};
use omp_dom::{
	Dom, Event, PropPolicy, Sid, Snapshot, StreamOp, redact_event,
	wire::{self, encode_event},
};
use omp_journal::EntryId;
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
///
/// **Delivered sequences increase but are not contiguous.** Consecutive
/// stream appends queued for one subscriber merge into a single delivery that
/// carries the later sequence and the concatenated text, so that subscriber
/// observes 1, 2, 5 where another observed 1, 2, 3, 4, 5. No content is lost:
/// the merged delivery carries every byte the skipped sequences carried. A
/// client must therefore check `seq > last` and never `seq == last + 1`:
/// arithmetic gap detection resyncs forever on a healthy stream.
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
///
/// Resuming from `seq` replays `(seq, head]`, so a cursor is valid whether or
/// not its sequence was itself delivered: merging skips sequences, and a
/// skipped sequence names a position inside a delivery the client already
/// holds. See [`Seq`] for why a client must not test delivered sequences for
/// contiguity.
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
	/// A reset lowered the replica's handle high-water mark, so handles minted
	/// before it may now name differently tagged nodes. The ring is dropped and
	/// every subscriber restarts, because redaction resolves a handle against
	/// the base and would otherwise fail open.
	Rewound,
	/// The subscriber's outbound queue exceeded its byte budget.
	Lagging,
	/// The subscriber's projection of an event could not be produced.
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

impl BridgeFault {
	/// Returns the closure this fault ends the bridge with.
	const fn closure(&self) -> Closure {
		match self {
			Self::Replica(_) => Closure::Replica,
			Self::Encode(_) => Closure::Encode,
		}
	}
}

/// Why a bridge stopped delivering.
///
/// Only [`Closure::Detached`] is a clean end of stream. Every other variant
/// means delivery stopped mid-session after an arbitrary prefix, so a
/// projection serving a remote client must end its RPC with a fault rather
/// than with success: the client's cursor is still valid, but this bridge can
/// no longer honour it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum Closure {
	/// The bridge was dropped. The attach ended on the host's own terms and
	/// the stream is complete up to the last delivery.
	Detached,
	/// The kernel's subscription channel disconnected. The kernel drops a
	/// subscriber whose bounded channel it cannot place an event in, so the
	/// stream is truncated at an arbitrary point unless the session itself
	/// ended.
	SubscriptionLost,
	/// The bridge replica rejected an event the kernel published.
	Replica,
	/// An event the kernel published could not be encoded canonically.
	Encode,
	/// The drain task left by unwinding. The bridge has no drain behind it,
	/// and delivery stopped wherever the panic landed.
	Panicked,
}

/// One admitted event.
///
/// `bytes` is the single canonical encoding every unredacted subscriber
/// shares, and the only full representation retained. A redacting subscriber
/// decodes it, which costs no more than the clone that redaction's by-value
/// signature forces anyway, and keeps the ring's byte accounting equal to what
/// the ring actually holds.
struct Entry {
	seq:    Seq,
	bytes:  Bytes,
	/// Set when the entry is a stream append, so a queued merge needs neither
	/// a decode nor a re-encode. Its text is retained beside `bytes` and is
	/// charged as such.
	append: Option<Appended>,
}

/// The mergeable payload of one stream append.
struct Appended {
	cause: EntryId,
	sid:   Sid,
	text:  Str,
}

impl Entry {
	/// Bytes this entry keeps resident: the canonical encoding plus an
	/// append's separately retained text.
	fn cost(&self) -> usize {
		self
			.bytes
			.len()
			.saturating_add(self.append.as_ref().map_or(0, |append| append.text.len()))
	}
}

/// Extracts the mergeable payload of an event, if it has one.
fn appended(event: &Event) -> Option<Appended> {
	let Event::Stream { cause, sid, op: StreamOp::Append, text: Some(text), .. } = event else {
		return None;
	};
	Some(Appended { cause: *cause, sid: *sid, text: text.clone() })
}

/// One item queued for a single subscriber.
enum Queued {
	Resync(ResyncReason),
	Welcome {
		seq:      Seq,
		snapshot: Arc<Snapshot>,
	},
	Event(Arc<Entry>),
	/// Consecutive appends for one stream, concatenated while queued and
	/// encoded only on the way out.
	///
	/// Charged by accumulated text length. That undercounts the envelope the
	/// variant is encoded into on the way out, once per stream run queued at the
	/// same time: a second stream's append pushes the first off the tail, so a
	/// queue can hold an `Append` per concurrent `sid`. The charge still tracks
	/// residency exactly, because the variant holds only its text.
	Append {
		seq:   Seq,
		cause: EntryId,
		sid:   Sid,
		text:  StrMut,
	},
}

impl Queued {
	/// Bytes this item charges against its subscriber's budget.
	fn cost(&self) -> usize {
		match self {
			Self::Resync(_) | Self::Welcome { .. } => 0,
			Self::Event(entry) => entry.cost(),
			Self::Append { text, .. } => text.len(),
		}
	}
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
	fn push_event(&mut self, entry: &Arc<Entry>, limits: &BridgeLimits) -> bool {
		match entry.append.as_ref() {
			Some(next) => self.push_append(entry, next),
			None => self.push_back(Queued::Event(Arc::clone(entry))),
		}
		self.bytes > limits.subscriber_bytes
	}

	fn push_back(&mut self, item: Queued) {
		self.bytes = self.bytes.saturating_add(item.cost());
		self.queue.push_back(item);
	}

	/// Queues one stream append, extending a queued append for the same
	/// stream rather than queueing a second item.
	///
	/// Merging concatenates text and nothing else: the canonical encoding is
	/// built once, in `pop`, with the bridge lock released. A run of `n`
	/// appends therefore costs one copy of the text each rather than
	/// re-encoding the whole accumulation `n` times.
	fn push_append(&mut self, entry: &Arc<Entry>, next: &Appended) {
		match self.queue.pop_back() {
			Some(Queued::Append { sid, mut text, .. }) if sid == next.sid => {
				text.push_str(next.text.as_str());
				self.bytes = self.bytes.saturating_add(next.text.len());
				self
					.queue
					.push_back(Queued::Append { seq: entry.seq, cause: next.cause, sid, text });
			},
			tail => {
				if let Some(Queued::Event(head)) = &tail
					&& let Some(queued) = head.append.as_ref()
					&& queued.sid == next.sid
				{
					let mut text =
						StrMut::with_capacity(queued.text.len().saturating_add(next.text.len()));
					text.push_str(queued.text.as_str());
					text.push_str(next.text.as_str());
					self.bytes = self
						.bytes
						.saturating_sub(head.cost())
						.saturating_add(text.len());
					self.queue.push_back(Queued::Append {
						seq: entry.seq,
						cause: next.cause,
						sid: next.sid,
						text,
					});
					return;
				}
				if let Some(tail) = tail {
					self.queue.push_back(tail);
				}
				self.push_back(Queued::Event(Arc::clone(entry)));
			},
		}
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

/// Everything behind the bridge's single lock.
struct State {
	replica:     Dom,
	base:        Option<Arc<Snapshot>>,
	ring:        VecDeque<Arc<Entry>>,
	ring_bytes:  usize,
	head:        Seq,
	subscribers: FastHashMap<u64, Outbound>,
	next_id:     u64,
	/// Set once, by whichever exit closed the bridge first.
	closed:      Option<Closure>,
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
			self.ring_bytes = self.ring_bytes.saturating_sub(entry.cost());
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

	/// Contains a rewind: the applied reset lowered the replica's handle mark,
	/// so every entry already in the ring or in a subscriber's queue may carry
	/// a handle that now names a different node. Both are dropped, the reset
	/// becomes the ring's new base, and every subscriber restarts from a fresh
	/// snapshot taken at the same head.
	fn rewind(&mut self, entry: Arc<Entry>) {
		self.ring.clear();
		self.ring_bytes = entry.cost();
		self.ring.push_back(entry);
		let head = self.head;
		let base = self.base();
		let mut gone: SmallVec<u64, 4> = SmallVec::new();
		for (&id, out) in &mut self.subscribers {
			out.restart(ResyncReason::Rewound, head, Arc::clone(&base));
			if !out.wake() {
				gone.push(id);
			}
		}
		for id in gone {
			self.subscribers.remove(&id);
		}
	}

	fn fanout(&mut self, entry: &Arc<Entry>, limits: &BridgeLimits) {
		let mut lagging: SmallVec<u64, 4> = SmallVec::new();
		let mut gone: SmallVec<u64, 4> = SmallVec::new();
		for (&id, out) in &mut self.subscribers {
			if out.push_event(entry, limits) {
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

/// What a non-blocking poll of a [`Subscriber`] found.
#[derive(Debug)]
pub enum Polled {
	/// One delivery, in order.
	Ready(Delivery),
	/// Nothing is queued and the bridge is still live. A later poll may
	/// return a delivery.
	Empty,
	/// The bridge has closed and its queue is drained. No later poll returns
	/// a delivery.
	Closed(Closure),
}

/// What a pop removed from a subscriber's queue, carrying everything the
/// projection needs so redaction and encoding run with the lock released.
enum Taken {
	Ready(Delivery),
	Empty,
	Closed(Closure),
	/// A snapshot, still to be redacted.
	Welcome {
		seq:      Seq,
		snapshot: Arc<Snapshot>,
	},
	/// A shared canonical entry, with the redaction base when the subscriber
	/// redacts.
	Entry {
		seq:   Seq,
		bytes: Bytes,
		base:  Option<Arc<Snapshot>>,
	},
	/// A merged append, still to be encoded.
	Append {
		seq:   Seq,
		event: Event,
		base:  Option<Arc<Snapshot>>,
	},
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
	///
	/// Encoding happens before the lock is taken, and merging defers its own
	/// encoding to `pop`, so the drain's critical section is bounded by the
	/// replica apply and one text copy per queued append.
	fn admit(&self, event: Event) -> Result<(), BridgeFault> {
		let bytes = Bytes::from(encode_event(&event)?);
		let append = appended(&event);
		let mut state = self.state.lock();
		let before = state.replica.high_water();
		state.replica.apply_event(&event)?;
		let rewound = state.replica.high_water() < before;
		state.base = None;
		let seq = state.head.next();
		state.head = seq;
		let entry = Arc::new(Entry { seq, bytes, append });
		if rewound {
			state.rewind(entry);
			return Ok(());
		}
		state.ring_bytes = state.ring_bytes.saturating_add(entry.cost());
		state.ring.push_back(Arc::clone(&entry));
		state.evict(&self.limits);
		state.fanout(&entry, &self.limits);
		Ok(())
	}

	/// Closes the bridge. The first closure wins: a drain that already
	/// reported a truncation is not overwritten by the bridge's own drop.
	fn close(&self, closure: Closure) {
		let mut state = self.state.lock();
		state.closed.get_or_insert(closure);
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

	/// Removes the next queued item, with the lock held for exactly that.
	fn take(&self, id: u64, redacts: bool) -> Taken {
		let mut state = self.state.lock();
		let closed = state.closed;
		let item = {
			let Some(out) = state.subscribers.get_mut(&id) else {
				return Taken::Closed(closed.unwrap_or(Closure::Detached));
			};
			let Some(item) = out.queue.pop_front() else {
				return match closed {
					Some(closure) => Taken::Closed(closure),
					None => Taken::Empty,
				};
			};
			out.bytes = out.bytes.saturating_sub(item.cost());
			item
		};
		match item {
			Queued::Resync(reason) => Taken::Ready(Delivery::Resync { reason }),
			Queued::Welcome { seq, snapshot } => Taken::Welcome { seq, snapshot },
			Queued::Event(entry) => Taken::Entry {
				seq:   entry.seq,
				bytes: entry.bytes.clone(),
				base:  redacts.then(|| state.base()),
			},
			Queued::Append { seq, cause, sid, text } => Taken::Append {
				seq,
				event: Event::Stream {
					cause,
					sid,
					op: StreamOp::Append,
					node: None,
					prop: None,
					text: Some(text.freeze()),
				},
				base: redacts.then(|| state.base()),
			},
		}
	}

	/// Restarts one subscriber from a fresh snapshot, returning the resync
	/// that heads the restarted stream.
	fn restart(&self, id: u64, reason: ResyncReason) -> Polled {
		let mut state = self.state.lock();
		let closed = state.closed;
		let head = state.head;
		let base = state.base();
		let Some(out) = state.subscribers.get_mut(&id) else {
			return Polled::Closed(closed.unwrap_or(Closure::Detached));
		};
		out.restart(reason, head, base);
		out.queue.pop_front();
		Polled::Ready(Delivery::Resync { reason })
	}

	fn pop(&self, id: u64, policy: Option<&dyn PropPolicy>) -> Polled {
		// Everything below the `take` runs with the lock released: redaction
		// deep-clones, and encoding a reset walks a whole snapshot.
		let (seq, event, base) = match self.take(id, policy.is_some()) {
			Taken::Ready(delivery) => return Polled::Ready(delivery),
			Taken::Empty => return Polled::Empty,
			Taken::Closed(closure) => return Polled::Closed(closure),
			Taken::Welcome { seq, snapshot } => {
				let snapshot = match policy {
					Some(policy) => Arc::new(snapshot.redact(policy)),
					None => snapshot,
				};
				return Polled::Ready(Delivery::Welcome { cursor: self.cursor(seq), snapshot });
			},
			Taken::Entry { seq, bytes, base } => {
				let Some(base) = base else {
					return Polled::Ready(Delivery::Event { cursor: self.cursor(seq), bytes });
				};
				match wire::decode_event(&bytes) {
					Ok(event) => (seq, event, Some(base)),
					Err(error) => {
						tracing::error!(
							session = %self.session,
							seq = seq.get(),
							?error,
							"session bridge could not decode its own canonical bytes",
						);
						return self.restart(id, ResyncReason::Projection);
					},
				}
			},
			Taken::Append { seq, event, base } => (seq, event, base),
		};
		let projected = match (base, policy) {
			(Some(base), Some(policy)) => redact_event(event, &base, policy),
			_ => event,
		};
		match encode_event(&projected) {
			Ok(bytes) => {
				Polled::Ready(Delivery::Event { cursor: self.cursor(seq), bytes: Bytes::from(bytes) })
			},
			Err(error) => {
				tracing::error!(
					session = %self.session,
					seq = seq.get(),
					?error,
					"session projection could not be encoded",
				);
				self.restart(id, ResyncReason::Projection)
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
				closed:      None,
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
					over |= out.push_event(entry, limits);
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
		self.shared.close(Closure::Detached);
	}
}

/// Drains the kernel subscription into the ring.
///
/// The loop awaits only the subscription receiver; every other step is
/// non-blocking, so a slow subscriber can never stall the kernel. That is
/// load-bearing: the kernel's channel is bounded and its fan-out drops a
/// subscriber it cannot place an event in, so falling behind costs the
/// subscription itself.
async fn drain(shared: Arc<Shared>, events: flume::Receiver<Event>) {
	let mut guard = DrainGuard { shared, closure: None };
	let closure = loop {
		match events.recv_async().await {
			Ok(event) => {
				if let Err(fault) = guard.shared.admit(event) {
					tracing::error!(
						session = %guard.shared.session,
						error = ?fault,
						"session bridge stopped draining its kernel subscription",
					);
					break fault.closure();
				}
			},
			Err(flume::RecvError::Disconnected) => {
				tracing::warn!(
					session = %guard.shared.session,
					"kernel DOM subscription disconnected; attached streams are truncated here",
				);
				break Closure::SubscriptionLost;
			},
		}
	};
	guard.closure = Some(closure);
}

/// Closes the bridge however the drain leaves it, including by unwinding.
///
/// Without this a panic inside [`Shared::admit`] would leave the bridge open
/// with no drain behind it, and a subscriber would park on a wake channel that
/// has no sender left instead of receiving the fault [`Closure`] exists to
/// deliver.
struct DrainGuard {
	shared:  Arc<Shared>,
	closure: Option<Closure>,
}

impl Drop for DrainGuard {
	fn drop(&mut self) {
		self.shared.close(self.closure.unwrap_or(Closure::Panicked));
	}
}

/// One attached reader of a [`SessionBridge`].
///
/// Deliveries are strictly ordered, so both readers take `&mut self`: two
/// tasks sharing one subscriber could otherwise interleave the stream.
pub struct Subscriber {
	shared: Arc<Shared>,
	id:     u64,
	wake:   flume::Receiver<()>,
}

impl Subscriber {
	/// Returns the next delivery without waiting, projecting through
	/// `policy` when one is supplied.
	///
	/// [`Polled::Empty`] and [`Polled::Closed`] are distinct: only the second
	/// ends the stream, and its [`Closure`] says whether the end was clean.
	#[must_use]
	pub fn try_next(&mut self, policy: Option<&dyn PropPolicy>) -> Polled {
		self.shared.pop(self.id, policy)
	}

	/// Awaits the next delivery, projecting through `policy` when one is
	/// supplied.
	///
	/// Returns the [`Closure`] once the bridge has closed and the queue is
	/// drained. Only [`Closure::Detached`] is a clean end of stream; every
	/// other closure means the stream was truncated and a projection serving
	/// it must fault.
	pub async fn next(&mut self, policy: Option<&dyn PropPolicy>) -> Result<Delivery, Closure> {
		loop {
			match self.shared.pop(self.id, policy) {
				Polled::Ready(delivery) => return Ok(delivery),
				Polled::Closed(closure) => return Err(closure),
				Polled::Empty => {
					// The wake sender lives in this subscriber's own queue
					// slot, which only this subscriber's drop removes.
					if self.wake.recv_async().await.is_err() {
						return Err(Closure::Detached);
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
	use omp_dom::{Handle, KnownTag, NodeSpec, Op, PropId, PropKey, Sid, Tag, Txn, Value};
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
					// A property on the seed, so a welcome snapshot carries
					// something a policy is asked about: `blank` short-circuits
					// on an empty node, which would leave the welcome's
					// redaction path uncovered.
					node:   NodeSpec::new(KnownTag::Turn)
						.with_prop(PropId::Text, Value::Str(Str::new_static("seeded turn"))),
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

		/// Snapshots the producer, so a later [`Producer::rewind_to`] can reset
		/// onto a shorter branch.
		fn mark(&self) -> Snapshot {
			self.dom.snapshot()
		}

		/// Resets onto an earlier snapshot, which carries a lower handle
		/// high-water mark than anything published since it.
		fn rewind_to(&mut self, snapshot: Snapshot) {
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

	fn drain_ready(subscriber: &mut Subscriber) -> Vec<Delivery> {
		let mut out = Vec::new();
		loop {
			match subscriber.try_next(None) {
				Polled::Ready(delivery) => out.push(delivery),
				Polled::Empty | Polled::Closed(_) => break,
			}
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
		let mut reader = bridge.subscribe(None);
		for index in 0..turns {
			producer.turn(&format!("turn {index}"));
		}
		settle(&bridge, Seq::from_wire(u64::from(turns))).await;
		drain_ready(&mut reader)
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
		let mut subscriber = bridge.subscribe(None);
		producer.turn("before");
		producer.reset();
		producer.turn("after");
		settle(&bridge, Seq::from_wire(3)).await;

		let delivered = drain_ready(&mut subscriber);
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
		let mut subscriber = bridge.subscribe(Some(&cursor));
		let delivered = drain_ready(&mut subscriber);

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
		let delivered = drain_ready(&mut bridge.subscribe(Some(&cursor)));

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
		let delivered = drain_ready(&mut bridge.subscribe(Some(&cursor)));

		assert_eq!(delivered.len(), 2);
		assert!(matches!(delivered[0], Delivery::Resync { reason: ResyncReason::Evicted }));
		assert_eq!(seq_of(&delivered[1]), Seq::from_wire(5));
		assert!(matches!(delivered[1], Delivery::Welcome { .. }));
	}

	#[tokio::test]
	async fn queued_stream_appends_merge_into_exactly_the_unmerged_text() {
		let (mut producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		let mut eager = bridge.subscribe(None);
		let mut lazy = bridge.subscribe(None);
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

		let delivered = drain_ready(&mut lazy);
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
		// Three events of budget, six published: the fourth overflows and
		// resyncs, and the last two stay queued behind the recovery pair, so
		// the contiguity check below has deliveries to walk.
		let sizes = event_sizes(6).await;
		let budget = sizes[0] + sizes[1] + sizes[2];

		let (mut producer, bridge) = Producer::start(limits(16, usize::MAX, budget));
		let mut subscriber = bridge.subscribe(None);
		for index in 0..4u8 {
			producer.turn(&format!("turn {index}"));
		}
		settle(&bridge, Seq::from_wire(4)).await;
		for index in 4..6u8 {
			producer.turn(&format!("turn {index}"));
		}
		settle(&bridge, Seq::from_wire(6)).await;

		let delivered = drain_ready(&mut subscriber);
		assert_eq!(delivered.len(), 4, "a resync, a welcome, and the two events behind them");
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

	#[tokio::test]
	async fn a_lost_kernel_subscription_closes_as_a_truncation() {
		let (producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		let mut subscriber = bridge.subscribe(None);
		// The kernel drops a subscriber it cannot place an event in, so a
		// disconnect is indistinguishable from a truncation at the receiver.
		drop(producer);

		let welcome = tokio::time::timeout(Duration::from_secs(5), subscriber.next(None))
			.await
			.expect("the welcome is already queued");
		assert!(matches!(welcome, Ok(Delivery::Welcome { .. })));
		let closure = tokio::time::timeout(Duration::from_secs(5), subscriber.next(None))
			.await
			.expect("a disconnected subscription closes the bridge");
		assert_eq!(
			closure.expect_err("the stream ends"),
			Closure::SubscriptionLost,
			"a lost subscription must not look like a clean end of stream",
		);
	}

	#[tokio::test]
	async fn a_dropped_bridge_closes_cleanly() {
		let (_producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		let mut subscriber = bridge.subscribe(None);
		// `_producer` keeps the subscription alive, so this is the host's own
		// detach and nothing was truncated.
		drop(bridge);

		let welcome = tokio::time::timeout(Duration::from_secs(5), subscriber.next(None))
			.await
			.expect("the welcome is already queued");
		assert!(matches!(welcome, Ok(Delivery::Welcome { .. })));
		let closure = tokio::time::timeout(Duration::from_secs(5), subscriber.next(None))
			.await
			.expect("a dropped bridge closes its subscribers");
		assert_eq!(closure.expect_err("the stream ends"), Closure::Detached);
	}

	#[tokio::test]
	async fn try_next_separates_an_empty_queue_from_a_closed_bridge() {
		let (producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		let mut subscriber = bridge.subscribe(None);

		assert!(matches!(subscriber.try_next(None), Polled::Ready(Delivery::Welcome { .. })));
		assert!(
			matches!(subscriber.try_next(None), Polled::Empty),
			"a live bridge with nothing queued is empty, not closed",
		);

		drop(producer);
		let closure = tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				match subscriber.try_next(None) {
					Polled::Empty => tokio::task::yield_now().await,
					Polled::Closed(closure) => return closure,
					Polled::Ready(delivery) => panic!("nothing was published, got {delivery:?}"),
				}
			}
		})
		.await
		.expect("the bridge closes once its subscription disconnects");
		assert_eq!(closure, Closure::SubscriptionLost);
	}

	/// A policy that parks inside the first redaction it is asked for, so a
	/// test can observe what the bridge can still do while it is parked.
	struct ParkingPolicy {
		entered: flume::Sender<()>,
		release: Mutex<Option<flume::Receiver<()>>>,
	}

	impl ParkingPolicy {
		fn park(&self) {
			let Some(release) = self.release.lock().take() else {
				return;
			};
			let _ = self.entered.send(());
			let _ = release.recv();
		}
	}

	impl PropPolicy for ParkingPolicy {
		fn redacts(&self, _tag: &Tag, _prop: &PropKey) -> bool {
			self.park();
			false
		}

		fn redacts_content(&self, _tag: &Tag) -> bool {
			self.park();
			false
		}
	}

	/// Redaction is the most expensive thing a subscriber does, and the drain
	/// task must not wait behind it. Every other test in this module runs on a
	/// current-thread runtime and so can never contend the bridge lock; this
	/// one does, by parking inside redaction and then requiring the drain to
	/// admit an event and the head to stay readable.
	#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
	async fn redaction_runs_with_the_bridge_lock_released() {
		let (mut producer, bridge) = Producer::start(limits(64, usize::MAX, usize::MAX));
		let mut subscriber = bridge.subscribe(None);
		producer.turn("redacted under a parked policy");
		settle(&bridge, Seq::START).await;

		let (entered, parked) = flume::bounded(1);
		let (release, released) = flume::bounded(1);
		let policy = Arc::new(ParkingPolicy { entered, release: Mutex::new(Some(released)) });
		// `dyn PropPolicy` is not `Sync`, so the reader cannot be a tokio
		// task: it drives the subscriber from its own thread, which is what
		// contends the bridge lock in the first place.
		let reader = {
			let policy = Arc::clone(&policy);
			std::thread::spawn(move || {
				let mut seen = Vec::new();
				while seen.len() < 3 {
					match subscriber.try_next(Some(policy.as_ref())) {
						Polled::Ready(delivery) => seen.push(delivery),
						Polled::Empty => std::thread::yield_now(),
						Polled::Closed(closure) => panic!("the bridge closed early: {closure}"),
					}
				}
				seen
			})
		};

		tokio::time::timeout(Duration::from_secs(5), parked.recv_async())
			.await
			.expect("redaction starts")
			.expect("the policy signals before it parks");

		producer.turn("admitted while redaction is parked");
		let probe = tokio::spawn(async move {
			settle(&bridge, Seq::from_wire(2)).await;
			bridge
		});
		let bridge = tokio::time::timeout(Duration::from_secs(10), probe)
			.await
			.expect("the drain admits and the head stays readable while a subscriber redacts")
			.expect("the probe task");

		release.send(()).expect("the parked redaction is released");
		let seen = tokio::time::timeout(
			Duration::from_secs(5),
			tokio::task::spawn_blocking(move || reader.join()),
		)
		.await
		.expect("the released reader finishes")
		.expect("the join task")
		.expect("the reader thread");
		assert_eq!(seen.len(), 3, "a welcome and both events");
		assert_eq!(seq_of(&seen[2]), Seq::from_wire(2));
		assert_eq!(bridge.head(), Seq::from_wire(2));
	}

	/// A reset onto a shorter branch lowers the replica's handle high-water
	/// mark, so a handle minted before it can be reissued to an unrelated node.
	/// Redaction resolves handles against the base, so replaying such an entry
	/// would classify it by the wrong node's tag and fail open. The bridge
	/// drops the ring and restarts every subscriber instead.
	#[tokio::test]
	async fn a_mark_lowering_reset_rewinds_every_subscriber() {
		let (mut producer, bridge) = Producer::start(limits(64, usize::MAX, usize::MAX));
		let mut subscriber = bridge.subscribe(None);
		let shorter = producer.mark();
		producer.turn("minted after the mark");
		producer.turn("minted after the mark as well");
		settle(&bridge, Seq::from_wire(2)).await;
		let before = drain_ready(&mut subscriber);
		assert_eq!(before.len(), 3, "a welcome and both turns");

		producer.rewind_to(shorter);
		settle(&bridge, Seq::from_wire(3)).await;

		let after = drain_ready(&mut subscriber);
		assert!(
			matches!(after.first(), Some(Delivery::Resync { reason: ResyncReason::Rewound })),
			"a mark-lowering reset restarts the stream, got {after:?}",
		);
		assert!(
			matches!(after.get(1), Some(Delivery::Welcome { .. })),
			"the resync is followed by a fresh snapshot, got {after:?}",
		);
		assert_eq!(
			bridge.oldest(),
			Some(Seq::from_wire(3)),
			"entries minted under the higher mark leave the ring with it",
		);
	}

	/// A drain that leaves by unwinding still closes the bridge. Without the
	/// guard `closed` stays unset, and a subscriber parks on a wake channel with
	/// no sender left instead of receiving a closure.
	#[tokio::test]
	async fn an_unwinding_drain_closes_as_a_panic() {
		let (_producer, bridge) = Producer::start(limits(16, usize::MAX, usize::MAX));
		let mut subscriber = bridge.subscribe(None);
		assert!(matches!(subscriber.try_next(None), Polled::Ready(Delivery::Welcome { .. })));

		drop(DrainGuard { shared: Arc::clone(&bridge.shared), closure: None });

		assert!(
			matches!(subscriber.try_next(None), Polled::Closed(Closure::Panicked)),
			"an unwinding drain names itself, so a projection can fault",
		);
	}
}
