//! Coalesces streamed journal text so a burst of provider deltas commits as
//! one `stream@1` append instead of one per delta.
//!
//! Every journal append is durable: `Journal::append` syncs the file before it
//! returns, and on macOS that sync is `F_FULLFSYNC`, measured at 4.6 ms per
//! call on the self-hosted M1 Pro runner. One append per provider delta
//! therefore held the agent loop to roughly 190 deltas per second and spent
//! 99.6% of the P8 full-loop recording inside the sync. Holding a delta back
//! for at most [`WINDOW`] keeps every committed entry durable while bounding
//! the syncs to one per window per stream.
//!
//! Kernel-event subscribers still receive each delta the moment it arrives;
//! session (DOM) subscribers see the text when it commits, at most one window
//! later. The window also bounds what a crash can lose from an open stream:
//! at most [`WINDOW`] of text that had not been committed yet.

use std::time::Duration;

use tokio::time::Instant;

/// Longest a buffered delta waits before it is committed to the journal.
pub const WINDOW: Duration = Duration::from_millis(16);

/// Buffered bytes that commit immediately, whatever the window says. It
/// bounds one entry well inside the journal's 1 MiB payload limit even when
/// every byte needs a six-byte JSON escape.
pub const MAX_BYTES: usize = 32 * 1024;

/// Text streamed into one journal stream that has not been committed yet.
#[derive(Debug, Default)]
pub struct CoalescedStream {
	sid:      Option<u32>,
	text:     String,
	deadline: Option<Instant>,
}

/// What the caller must do after [`CoalescedStream::push`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pushed {
	/// The delta is buffered and the window was already running.
	Buffered,
	/// The delta opened a new window; the caller arms its timer for
	/// [`CoalescedStream::deadline`].
	Armed,
	/// The buffer reached [`MAX_BYTES`]; the caller commits it now.
	Full,
}

impl CoalescedStream {
	/// Stream the buffered text belongs to, if any is buffered.
	#[must_use]
	pub const fn sid(&self) -> Option<u32> {
		self.sid
	}

	/// When the buffered text must be committed, if any is buffered.
	#[must_use]
	pub const fn deadline(&self) -> Option<Instant> {
		self.deadline
	}

	/// Buffers `delta` for `sid`.
	///
	/// The buffer holds one stream at a time: callers commit it first when
	/// `sid` differs from [`Self::sid`], so journal order follows arrival
	/// order across interleaved streams.
	pub fn push(&mut self, sid: u32, delta: &str, now: Instant) -> Pushed {
		debug_assert!(
			self.sid.is_none_or(|current| current == sid),
			"commit the buffered stream before switching streams"
		);
		let armed = self.deadline.is_none();
		if armed {
			self.sid = Some(sid);
			self.deadline = Some(now + WINDOW);
		}
		self.text.push_str(delta);
		if self.text.len() >= MAX_BYTES {
			Pushed::Full
		} else if armed {
			Pushed::Armed
		} else {
			Pushed::Buffered
		}
	}

	/// The buffered stream and text, if anything is waiting to commit.
	#[must_use]
	pub fn pending(&self) -> Option<(u32, &str)> {
		self.sid.map(|sid| (sid, self.text.as_str()))
	}

	/// Forgets the buffered text once it has been committed. The text
	/// allocation is kept for the next window.
	pub fn clear(&mut self) {
		self.sid = None;
		self.deadline = None;
		self.text.clear();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn first_delta_arms_one_window_and_later_deltas_join_it() {
		let now = Instant::now();
		let mut stream = CoalescedStream::default();
		assert_eq!(stream.pending(), None);
		assert_eq!(stream.push(3, "a", now), Pushed::Armed);
		assert_eq!(
			stream.push(3, "b", now + Duration::from_millis(5)),
			Pushed::Buffered,
			"a later delta joins the running window"
		);
		assert_eq!(stream.deadline(), Some(now + WINDOW), "the window does not slide");
		assert_eq!(stream.pending(), Some((3, "ab")));
	}

	#[test]
	fn clear_empties_the_buffer_and_the_next_delta_rearms() {
		let now = Instant::now();
		let mut stream = CoalescedStream::default();
		stream.push(1, "committed", now);
		stream.clear();
		assert_eq!(stream.pending(), None);
		assert_eq!(stream.sid(), None);
		assert_eq!(stream.deadline(), None);
		let later = now + Duration::from_secs(1);
		assert_eq!(stream.push(2, "next", later), Pushed::Armed);
		assert_eq!(stream.pending(), Some((2, "next")));
		assert_eq!(stream.deadline(), Some(later + WINDOW));
	}

	#[test]
	fn reaching_the_byte_bound_asks_for_an_immediate_commit() {
		let now = Instant::now();
		let mut stream = CoalescedStream::default();
		let chunk = "x".repeat(MAX_BYTES / 2);
		assert_eq!(stream.push(7, &chunk, now), Pushed::Armed);
		assert_eq!(stream.push(7, &chunk, now), Pushed::Full);
		assert_eq!(stream.pending().map(|(_, text)| text.len()), Some(MAX_BYTES));
	}

	#[test]
	fn one_oversized_delta_is_full_on_arrival() {
		let mut stream = CoalescedStream::default();
		assert_eq!(stream.push(7, &"x".repeat(MAX_BYTES), Instant::now()), Pushed::Full);
	}
}
