//! A loopback HTTP server for sandbox network proofs: it stands in for one
//! package host that the scoped egress broker may reach under
//! `sv_sandbox_allow_localhost`.

use std::{
	io::{BufRead as _, BufReader, Write as _},
	net::{Ipv4Addr, TcpListener, TcpStream},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	thread::{self, JoinHandle},
	time::Duration,
};

/// The body every request is answered with.
pub const BODY: &str = "ok\n";

/// Answers every request `200` with [`BODY`] on its own thread. Dropping it
/// stops serving.
pub struct LoopbackUpstream {
	port:    u16,
	stop:    Arc<AtomicBool>,
	serving: Option<JoinHandle<()>>,
}

impl LoopbackUpstream {
	/// Starts serving on a free loopback port.
	pub fn serve() -> Self {
		let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("upstream");
		let port = listener.local_addr().expect("upstream address").port();
		let stop = Arc::new(AtomicBool::new(false));
		let stopped = Arc::clone(&stop);
		let serving = thread::spawn(move || {
			for stream in listener.incoming() {
				if stopped.load(Ordering::Acquire) {
					break;
				}
				let Ok(mut stream) = stream else { continue };
				let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
				let Ok(reading) = stream.try_clone() else {
					continue;
				};
				let mut reader = BufReader::new(reading);
				let mut line = String::new();
				while reader.read_line(&mut line).is_ok_and(|read| read > 0) && line != "\r\n" {
					line.clear();
				}
				let _ = write!(
					stream,
					"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
					BODY.len()
				);
			}
		});
		Self { port, stop, serving: Some(serving) }
	}

	/// The loopback port it serves on.
	pub const fn port(&self) -> u16 {
		self.port
	}

	/// A command fetching this host once through the broker. `--noproxy ''`
	/// keeps a host `NO_PROXY` from sending curl around it.
	pub fn fetch(&self) -> String {
		format!("/usr/bin/curl --noproxy '' -sf http://localhost:{}/", self.port)
	}
}

impl Drop for LoopbackUpstream {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::Release);
		// One connection wakes the blocked `accept` to see the flag.
		if TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)).is_ok()
			&& let Some(serving) = self.serving.take()
		{
			let _ = serving.join();
		}
	}
}
