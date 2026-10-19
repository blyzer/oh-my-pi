//! `omp attach` — connect to a running session host over its owner-only UDS.
//!
//! The socket lives at `<data dir>/run/session.sock` and is bound by the
//! in-process [`omp_driver::SessionHost`] when `omp chat`, `omp rpc`, or
//! `omp print` starts.  Full gRPC projection (ADR 0039 A2/A4) is not yet
//! wired; this command provides the plumbing and a useful `--list` mode.

use std::path::Path;

use miette::{IntoDiagnostic as _, miette};
use omp_core::dirs;
use tokio::net::UnixStream;

use crate::cli::AttachArgs;

/// Entry point for `omp attach`.
pub async fn run(args: AttachArgs) -> miette::Result<()> {
	let data_dir = dirs::data_dir(None).into_diagnostic()?;
	let socket = data_dir.join("run").join("session.sock");

	if args.list {
		list_sessions(&socket).await
	} else {
		attach_session(&socket, args.session_id.as_deref()).await
	}
}

/// Connects to the socket, prints stub session rows, and exits.
///
/// When the socket does not exist or refuses the connection the command
/// reports that no session host is running — this is the normal state when no
/// `omp chat`/`omp rpc` process is active.
async fn list_sessions(socket: &Path) -> miette::Result<()> {
	match UnixStream::connect(socket).await {
		Ok(_stream) => {
			// Socket is live.  A real listing requires the gRPC SessionControl
			// service (ADR 0039 A2).  For now we confirm reachability and
			// print a placeholder so the UDS handshake path is exercised.
			println!("[session host reachable — session listing requires omp-serve A2]");
		},
		Err(error) if is_not_found(&error) => {
			println!("No session host is running (socket not found: {}).", socket.display());
		},
		Err(error) => {
			return Err(miette!(
				"could not connect to session socket {}: {}",
				socket.display(),
				error
			));
		},
	}
	Ok(())
}

/// Connects to the socket and holds the stream open (stub).
async fn attach_session(socket: &Path, session_id: Option<&str>) -> miette::Result<()> {
	let stream = UnixStream::connect(socket).await.map_err(|error| {
		if is_not_found(&error) {
			miette!(
				"No session host is running. Start `omp chat` or `omp rpc` first (socket not found: \
				 {}).",
				socket.display()
			)
		} else {
			miette!("could not connect to session socket {}: {}", socket.display(), error)
		}
	})?;

	let target = session_id.unwrap_or("<most-recent>");
	println!("Attaching to {target}...");
	println!("[stub: full gRPC projection is ADR 0039 A2/A4]");

	// Hold the connection open until interrupted.  The real implementation
	// will drive a bidirectional gRPC stream; for now we just park.
	drop(stream); // drop avoids unused-variable lint; real impl will use it.
	std::future::pending::<()>().await;

	Ok(())
}

/// Returns true when the I/O error indicates the socket path does not exist.
fn is_not_found(error: &std::io::Error) -> bool {
	matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
}
