//! How a scoped egress listener recovers from a failed `accept`.
//!
//! Two listeners carry scoped sandbox networking: the session's egress broker
//! in `omp-envd` and the Linux relay inside each Bubblewrap network namespace.
//! Both serve for as long as their owner lives, so one failure that belongs
//! to a single connection, or a burst of descriptor exhaustion, must never end
//! them. They share this classification, so neither survives a failure the
//! other dies of.

use std::{io, time::Duration};

/// What a scoped egress listener does after one failed `accept` on its stream
/// listener, or one failed wait for a pending connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptFailure {
	/// The failure belonged to the call or to one connection: a signal
	/// interrupted it, a nonblocking listener had nothing pending after all,
	/// or the peer abandoned the connection while it waited in the backlog
	/// (Linux also reports that connection's pending network error here).
	/// The listener accepts again at once.
	Transient,
	/// The process or the system ran out of descriptors, socket buffers or
	/// kernel memory. The pending connection stays queued, so accepting again
	/// at once would spin: the listener waits for [`AcceptBackoff`] first, and
	/// keeps retrying for as long as the shortage lasts.
	Exhausted,
	/// The listener itself is unusable. The listener stops serving.
	Fatal,
}

impl AcceptFailure {
	/// Classifies one failed `accept`, or one failed wait for a pending
	/// connection, on a stream listener.
	///
	/// An operating-system error is classified by its code; any other error by
	/// its [`io::ErrorKind`]. A code this table does not name is
	/// [`Self::Fatal`]: retrying an unknown failure could spin forever.
	pub fn of(error: &io::Error) -> Self {
		#[cfg(any(unix, windows))]
		if let Some(code) = error.raw_os_error() {
			return Self::of_code(code);
		}
		Self::of_kind(error.kind())
	}

	/// Classifies an `accept(2)` errno. Linux passes a pending connection's
	/// network error through `accept`, and `accept(2)` asks callers to retry on
	/// the TCP/IP ones like `EAGAIN`. `EOPNOTSUPP` is among them: it would
	/// otherwise mean a listener that is not `SOCK_STREAM`, which a stream
	/// listener never is. `EPERM` (a firewall refusal on Linux) stays fatal,
	/// because a seccomp filter denies with the same code and retrying that
	/// would spin.
	#[cfg(unix)]
	const fn of_code(code: i32) -> Self {
		match code {
			libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM => Self::Exhausted,
			// `EWOULDBLOCK` is `EAGAIN` on every Unix this crate builds for.
			libc::EINTR | libc::EAGAIN | libc::ECONNABORTED | libc::ECONNRESET | libc::EPROTO => {
				Self::Transient
			},
			#[cfg(any(target_os = "linux", target_os = "android"))]
			libc::ENETDOWN
			| libc::ENOPROTOOPT
			| libc::EHOSTDOWN
			| libc::ENONET
			| libc::EHOSTUNREACH
			| libc::EOPNOTSUPP
			| libc::ENETUNREACH => Self::Transient,
			_ => Self::Fatal,
		}
	}

	/// Classifies an `accept` Winsock error. Winsock reports exhaustion as
	/// `WSAEMFILE` or `WSAENOBUFS` and a connection the peer abandoned in the
	/// backlog as `WSAECONNRESET`.
	#[cfg(windows)]
	const fn of_code(code: i32) -> Self {
		use windows_sys::Win32::Networking::WinSock::{
			WSAECONNABORTED, WSAECONNRESET, WSAEINTR, WSAEMFILE, WSAENOBUFS, WSAEWOULDBLOCK,
		};

		match code {
			WSAEMFILE | WSAENOBUFS => Self::Exhausted,
			WSAEINTR | WSAEWOULDBLOCK | WSAECONNABORTED | WSAECONNRESET => Self::Transient,
			_ => Self::Fatal,
		}
	}

	/// Classifies an error that carries no operating-system code.
	const fn of_kind(kind: io::ErrorKind) -> Self {
		match kind {
			io::ErrorKind::Interrupted
			| io::ErrorKind::WouldBlock
			| io::ErrorKind::ConnectionAborted
			| io::ErrorKind::ConnectionReset => Self::Transient,
			io::ErrorKind::OutOfMemory => Self::Exhausted,
			_ => Self::Fatal,
		}
	}
}

/// The bounded wait between accepts while [`AcceptFailure::Exhausted`]
/// lasts: [`Self::FIRST`], doubling with every further exhausted accept up to
/// [`Self::MAX`], and back to [`Self::FIRST`] once a connection is accepted.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptBackoff {
	/// The last wait handed out, or zero when no shortage is ongoing.
	delay: Duration,
}

impl AcceptBackoff {
	/// The wait after the first exhausted accept.
	pub const FIRST: Duration = Duration::from_millis(5);
	/// The longest wait. A shortage that never ends costs the listener one
	/// accept per this interval, never a spin.
	pub const MAX: Duration = Duration::from_secs(1);

	/// Returns the wait before the next accept after one more exhausted
	/// accept.
	pub fn next_delay(&mut self) -> Duration {
		self.delay = if self.delay.is_zero() {
			Self::FIRST
		} else {
			self.delay.saturating_mul(2).min(Self::MAX)
		};
		self.delay
	}

	/// Ends the shortage: a connection was accepted.
	pub const fn reset(&mut self) {
		self.delay = Duration::ZERO;
	}
}

#[cfg(test)]
mod tests {
	use std::io;

	use super::{AcceptBackoff, AcceptFailure};

	#[cfg(unix)]
	#[test]
	fn accept_errnos_are_classified_by_what_they_say_about_the_listener() {
		for code in [libc::EINTR, libc::EAGAIN, libc::ECONNABORTED, libc::ECONNRESET, libc::EPROTO] {
			let error = io::Error::from_raw_os_error(code);
			assert_eq!(AcceptFailure::of(&error), AcceptFailure::Transient, "{error}");
		}
		for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
			let error = io::Error::from_raw_os_error(code);
			assert_eq!(AcceptFailure::of(&error), AcceptFailure::Exhausted, "{error}");
		}
		for code in [libc::EBADF, libc::EINVAL, libc::ENOTSOCK, libc::EFAULT, libc::EPERM] {
			let error = io::Error::from_raw_os_error(code);
			assert_eq!(AcceptFailure::of(&error), AcceptFailure::Fatal, "{error}");
		}
	}

	#[cfg(any(target_os = "linux", target_os = "android"))]
	#[test]
	fn linux_pending_network_errors_are_transient() {
		for code in [
			libc::ENETDOWN,
			libc::ENOPROTOOPT,
			libc::EHOSTDOWN,
			libc::ENONET,
			libc::EHOSTUNREACH,
			libc::EOPNOTSUPP,
			libc::ENETUNREACH,
		] {
			let error = io::Error::from_raw_os_error(code);
			assert_eq!(AcceptFailure::of(&error), AcceptFailure::Transient, "{error}");
		}
	}

	#[test]
	fn errors_without_a_code_are_classified_by_kind() {
		let classify = |kind| AcceptFailure::of(&io::Error::from(kind));
		assert_eq!(classify(io::ErrorKind::Interrupted), AcceptFailure::Transient);
		assert_eq!(classify(io::ErrorKind::WouldBlock), AcceptFailure::Transient);
		assert_eq!(classify(io::ErrorKind::ConnectionAborted), AcceptFailure::Transient);
		assert_eq!(classify(io::ErrorKind::ConnectionReset), AcceptFailure::Transient);
		assert_eq!(classify(io::ErrorKind::OutOfMemory), AcceptFailure::Exhausted);
		assert_eq!(classify(io::ErrorKind::InvalidInput), AcceptFailure::Fatal);
		assert_eq!(classify(io::ErrorKind::Other), AcceptFailure::Fatal);
	}

	#[test]
	fn backoff_doubles_to_its_bound_and_restarts_after_an_accept() {
		let mut backoff = AcceptBackoff::default();
		let mut delays = Vec::new();
		for _ in 0..10 {
			delays.push(backoff.next_delay().as_millis());
		}
		assert_eq!(delays, [5, 10, 20, 40, 80, 160, 320, 640, 1000, 1000]);
		backoff.reset();
		assert_eq!(backoff.next_delay(), AcceptBackoff::FIRST);
	}
}
