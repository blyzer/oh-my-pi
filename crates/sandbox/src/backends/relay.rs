//! The scoped egress relay's serving loop. Inside a Bubblewrap network
//! namespace it bridges the loopback port the confined command dials to the
//! session broker's Unix socket.
//!
//! Only Linux runs the relay: the Landlock helper forks it into the namespace,
//! binds its listener with [`bind`], confines it and hands the listener to
//! [`serve`]. Nothing in the loop itself is Linux-specific, so test builds
//! compile this module on every Unix, and its tests prove the accept handling
//! wherever the suite runs, macOS included.

use std::{
	io,
	net::{Ipv4Addr, TcpListener, TcpStream},
	os::unix::net::UnixStream,
	path::Path,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	thread,
	time::Duration,
};

use crate::{AcceptBackoff, AcceptFailure};

/// The most connections the relay carries at once. A connection past it is
/// closed as soon as it is accepted.
const MAX_CONNECTIONS: usize = 32;
/// How long a relayed connection may wait on either of its peers.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Binds the relay's listener on the namespace's loopback `port`.
///
/// The listener stays blocking: [`serve`] waits in `accept`, so an idle relay
/// costs no wakeups.
pub(super) fn bind(port: u16) -> io::Result<TcpListener> {
	TcpListener::bind((Ipv4Addr::LOCALHOST, port))
}

/// Where the relay accepts its connections: the loopback [`TcpListener`] from
/// [`bind`], or a scripted listener in tests.
pub(super) trait Listener {
	/// Accepts one connection, waiting until one is pending.
	fn accept(&self) -> io::Result<TcpStream>;

	/// Waits out one exhausted-accept backoff before the next `accept`.
	fn back_off(&self, delay: Duration) {
		thread::sleep(delay);
	}
}

impl Listener for TcpListener {
	fn accept(&self) -> io::Result<TcpStream> {
		Self::accept(self).map(|(stream, _)| stream)
	}
}

/// Bridges each connection `listener` accepts to the broker's Unix `socket`
/// until the listener fails.
///
/// The relay blocks in `accept`, so an idle relay costs no wakeups. It needs no
/// stop signal of its own, because signals end it wherever it blocks:
/// `PR_SET_PDEATHSIG` when its parent (the helper that became the target
/// command) exits, and the kernel when Bubblewrap's private PID namespace ends
/// with the sandbox. A failure that belongs to one connection never ends it,
/// and descriptor or memory exhaustion only slows it down ([`AcceptFailure`]);
/// only a failure of the listener itself returns.
pub(super) fn serve(socket: &Path, listener: &impl Listener) -> io::Result<()> {
	let live = Arc::new(AtomicUsize::new(0));
	let mut backoff = AcceptBackoff::default();
	loop {
		let accepted = listener.accept();
		if accepted.is_ok() {
			backoff.reset();
		}
		match accepted {
			// Dropping the accepted socket is the rejection: the peer sees an
			// immediate close rather than a hang against a full relay.
			Ok(_client) if live.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS => {
				live.fetch_sub(1, Ordering::AcqRel);
			},
			Ok(client) => {
				let socket = socket.to_path_buf();
				let worker_live = Arc::clone(&live);
				if thread::Builder::new()
					.name("omp-scoped-relay".into())
					.spawn(move || {
						let _ = relay_connection(client, &socket, IDLE_TIMEOUT);
						worker_live.fetch_sub(1, Ordering::AcqRel);
					})
					.is_err()
				{
					live.fetch_sub(1, Ordering::AcqRel);
				}
			},
			Err(error) => match AcceptFailure::of(&error) {
				AcceptFailure::Transient => {},
				AcceptFailure::Exhausted => listener.back_off(backoff.next_delay()),
				AcceptFailure::Fatal => return Err(error),
			},
		}
	}
}

/// Copies one accepted `client` to and from a new connection to the broker's
/// `socket` until either side closes or stays silent for `timeout`.
fn relay_connection(mut client: TcpStream, socket: &Path, timeout: Duration) -> io::Result<()> {
	client.set_read_timeout(Some(timeout))?;
	client.set_write_timeout(Some(timeout))?;
	let mut broker = UnixStream::connect(socket)?;
	broker.set_read_timeout(Some(timeout))?;
	broker.set_write_timeout(Some(timeout))?;
	let mut client_copy = client.try_clone()?;
	let closer = client_copy.try_clone()?;
	let mut broker_copy = broker.try_clone()?;
	let copied = thread::Builder::new()
		.name("omp-scoped-relay-copy".into())
		.spawn(move || io::copy(&mut client_copy, &mut broker_copy));
	let down = io::copy(&mut broker, &mut client);
	let _ = closer.shutdown(std::net::Shutdown::Both);
	if let Ok(copied) = copied {
		let _ = copied.join();
	}
	down.map(|_| ())
}

#[cfg(test)]
mod tests {
	use std::{
		io::{self, Read as _, Write as _},
		net::{SocketAddr, TcpListener, TcpStream},
		os::unix::net::UnixListener,
		path::Path,
		sync::{
			Arc,
			atomic::{AtomicBool, AtomicUsize, Ordering},
		},
		thread::{self, JoinHandle},
		time::{Duration, Instant},
	};

	use parking_lot::Mutex;

	use super::{Listener, bind, serve};
	use crate::AcceptBackoff;

	/// One scripted `accept`.
	#[derive(Clone, Copy)]
	enum Step {
		/// Fails with this errno and leaves any pending connection queued, as
		/// the kernel does.
		Fail(i32),
		/// Accepts from the real listener.
		Accept,
	}

	/// The relay's own listener from [`bind`] with its first accepts scripted.
	/// Past the script it accepts for real. Once stopped, it turns the
	/// connection that woke it into `EBADF`, a listener failure that ends
	/// [`serve`]. It counts every `accept` and records every backoff instead
	/// of sleeping through it.
	struct Scripted {
		inner:   TcpListener,
		script:  &'static [Step],
		calls:   AtomicUsize,
		delays:  Mutex<Vec<Duration>>,
		stopped: AtomicBool,
	}

	impl Scripted {
		fn new(script: &'static [Step]) -> Arc<Self> {
			Arc::new(Self {
				inner: bind(0).expect("relay listener"),
				script,
				calls: AtomicUsize::new(0),
				delays: Mutex::new(Vec::new()),
				stopped: AtomicBool::new(false),
			})
		}

		fn address(&self) -> SocketAddr {
			self.inner.local_addr().expect("relay address")
		}

		fn calls(&self) -> usize {
			self.calls.load(Ordering::Acquire)
		}
	}

	impl Listener for Scripted {
		fn accept(&self) -> io::Result<TcpStream> {
			let call = self.calls.fetch_add(1, Ordering::AcqRel);
			if let Some(Step::Fail(code)) = self.script.get(call) {
				return Err(io::Error::from_raw_os_error(*code));
			}
			let stream = Listener::accept(&self.inner)?;
			if self.stopped.load(Ordering::Acquire) {
				return Err(io::Error::from_raw_os_error(libc::EBADF));
			}
			Ok(stream)
		}

		fn back_off(&self, delay: Duration) {
			self.delays.lock().push(delay);
		}
	}

	/// Runs [`serve`] on its own thread, relaying to `socket`.
	fn spawn(listener: &Arc<Scripted>, socket: &Path) -> JoinHandle<io::Result<()>> {
		let listener = Arc::clone(listener);
		let socket = socket.to_path_buf();
		thread::spawn(move || serve(&socket, &*listener))
	}

	/// Waits, bounded, until the relay has entered its `calls`th `accept`.
	fn wait_for_calls(listener: &Scripted, calls: usize) {
		let deadline = Instant::now() + Duration::from_secs(5);
		while listener.calls() < calls {
			assert!(Instant::now() < deadline, "the relay never reached accept {calls}");
			thread::sleep(Duration::from_millis(1));
		}
	}

	/// Ends a relay blocked in `accept` the way a failed listener does, and
	/// returns the error it ended with. One connection wakes the relay, and the
	/// accept it wakes from fails.
	fn stop(relay: JoinHandle<io::Result<()>>, listener: &Scripted) -> io::Error {
		listener.stopped.store(true, Ordering::Release);
		let _wake = TcpStream::connect(listener.address()).expect("wake the relay");
		let deadline = Instant::now() + Duration::from_secs(5);
		while !relay.is_finished() {
			assert!(Instant::now() < deadline, "the relay outlived its failed listener");
			thread::sleep(Duration::from_millis(1));
		}
		relay
			.join()
			.expect("relay thread")
			.expect_err("the relay ended without its listener failing")
	}

	#[test]
	fn an_idle_relay_waits_in_a_single_accept() {
		let directory = tempfile::tempdir().expect("broker directory");
		let listener = Scripted::new(&[]);
		let relay = spawn(&listener, &directory.path().join("broker.sock"));
		wait_for_calls(&listener, 1);
		thread::sleep(Duration::from_millis(200));
		// A relay polling a nonblocking listener every 10 ms makes about twenty
		// accepts here, and one spinning on it thousands.
		assert_eq!(listener.calls(), 1, "an idle relay kept calling accept");
		assert_eq!(stop(relay, &listener).raw_os_error(), Some(libc::EBADF));
		assert!(listener.delays.lock().is_empty());
	}

	#[test]
	fn connection_failures_and_exhaustion_keep_the_relay_serving() {
		const SCRIPT: &[Step] = &[
			Step::Fail(libc::ECONNABORTED),
			Step::Fail(libc::EMFILE),
			Step::Fail(libc::EINTR),
			Step::Fail(libc::ENOBUFS),
			Step::Fail(libc::EPROTO),
			Step::Accept,
			Step::Fail(libc::EMFILE),
		];
		let directory = tempfile::tempdir().expect("broker directory");
		let socket = directory.path().join("broker.sock");
		let broker = UnixListener::bind(&socket).expect("broker socket");
		let broker_task = thread::spawn(move || {
			let (mut stream, _) = broker.accept().expect("relayed connection");
			let mut request = [0_u8; 4];
			stream.read_exact(&mut request).expect("relayed request");
			stream.write_all(b"pong").expect("broker reply");
			request
		});

		let listener = Scripted::new(SCRIPT);
		let relay = spawn(&listener, &socket);
		let mut client = TcpStream::connect(listener.address()).expect("connect the relay");
		client
			.set_read_timeout(Some(Duration::from_secs(5)))
			.expect("read timeout");
		client.write_all(b"ping").expect("request");
		let mut reply = Vec::new();
		client.read_to_end(&mut reply).expect("reply");
		assert_eq!(reply, b"pong");
		assert_eq!(&broker_task.join().expect("broker task"), b"ping");

		// The relay runs through the script and then waits in one more accept.
		wait_for_calls(&listener, SCRIPT.len() + 1);
		assert_eq!(stop(relay, &listener).raw_os_error(), Some(libc::EBADF));
		assert_eq!(listener.calls(), SCRIPT.len() + 1);
		// EMFILE and ENOBUFS waited out the first two backoff steps. The relayed
		// connection ended that shortage, so the EMFILE after it starts over.
		assert_eq!(*listener.delays.lock(), [
			AcceptBackoff::FIRST,
			AcceptBackoff::FIRST * 2,
			AcceptBackoff::FIRST,
		]);
	}

	#[test]
	fn a_failed_listener_ends_the_relay() {
		let directory = tempfile::tempdir().expect("broker directory");
		let listener = Scripted::new(&[Step::Fail(libc::EINVAL)]);
		let ended = serve(&directory.path().join("broker.sock"), &*listener)
			.expect_err("the relay outlived its failed listener");
		assert_eq!(ended.raw_os_error(), Some(libc::EINVAL));
		assert_eq!(listener.calls(), 1);
	}
}
