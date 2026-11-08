//! The deliberately small git-CLI escape hatch.
//!
//! Three categories are allowed to spawn `git`, per the hybrid policy:
//! - **Credential-bound network transfers** — clone/fetch/push must reuse the
//!   user's ssh config and credential helpers, which are subprocess-based by
//!   design; no library reaches auth parity (and gitoxide has no send-pack).
//! - **Credential-helper lookups** — [`credential_fill`] lets HTTPS API clients
//!   authenticate with the same helper-held token push/fetch already use.
//! - **Reftable ref access** — no in-process reftable implementation exists.
//!
//! The runner keeps a hardened subprocess contract:
//! non-interactive env (`GIT_TERMINAL_PROMPT=0`, askpass rejection, `LC_ALL`
//! handling), `--no-optional-locks` for reads, fsmonitor/untracked-cache
//! disabled, ambient `GIT_DIR`-family vars stripped, bounded output capture,
//! and deadline + SIGTERM→SIGKILL termination via tokio.

use std::{path::Path, process::Stdio, time::Duration};

use omp_core::SecretString;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::GitRepo;
use crate::{
	error::{Error, Result},
	types::{CloneOptions, PushOptions},
};

/// Default deadline for local git plumbing via the fallback runner.
pub const COMMAND_TIMEOUT: Duration = Duration::from_mins(5);
/// Default deadline for network transfers (`clone`/`fetch`/`push`); large-repo
/// transfers legitimately outlive [`COMMAND_TIMEOUT`].
pub const NETWORK_TIMEOUT: Duration = Duration::from_mins(30);
/// Deadline for one non-interactive `git credential fill` lookup.
pub const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Deadline for synchronous plumbing on render paths (reftable HEAD reads):
/// a stalled invocation degrades instead of freezing the UI.
pub const SYNC_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum captured bytes per stream before truncation.
pub const OUTPUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;

const TERMINATE_GRACE: Duration = Duration::from_secs(5);
const TRUNCATION_MARKER: &str = "\n[git subprocess output truncated after 8 MiB]\n";

/// Captured result of a completed git invocation.
#[derive(Debug, Clone)]
pub struct CliOutput {
	/// Process exit code (`124` designates a deadline kill).
	pub exit_code: i32,
	/// Captured stdout, possibly truncated.
	pub stdout:    String,
	/// Captured stderr, possibly truncated.
	pub stderr:    String,
}

impl CliOutput {
	/// Convert a non-zero exit into [`Error::Cli`].
	pub fn into_checked(self, args: &[String]) -> Result<Self> {
		if self.exit_code == 0 {
			return Ok(self);
		}
		Err(Error::Cli {
			command:   format!("git {}", args.join(" ")),
			exit_code: self.exit_code,
			stdout:    self.stdout,
			stderr:    self.stderr,
		})
	}
}

/// Options for one fallback invocation.
#[derive(Debug, Default)]
pub struct RunOptions {
	/// Prefix `--no-optional-locks` and pin lock-free config for reads.
	pub read_only: bool,
	/// Deadline; [`COMMAND_TIMEOUT`] when unset.
	pub timeout:   Option<Duration>,
	/// Bytes piped to stdin (commit messages, `update-ref --stdin` scripts).
	pub stdin:     Option<Vec<u8>>,
	/// Cooperative cancellation: the child is terminated when triggered.
	pub cancel:    Option<CancellationToken>,
}

/// Build the hardened argv prefix: short-lived config pins that stop a
/// transient subprocess from mutating fsmonitor/untracked-cache state.
fn hardened_args(args: &[String], read_only: bool) -> Vec<String> {
	let mut out = Vec::with_capacity(args.len() + 5);
	for pin in ["core.fsmonitor=false", "core.untrackedCache=false"] {
		out.push("-c".to_owned());
		out.push(pin.to_owned());
	}
	if read_only && !args.iter().any(|arg| arg == "--no-optional-locks") {
		out.push("--no-optional-locks".to_owned());
	}
	out.extend(args.iter().cloned());
	out
}

/// Apply the non-interactive environment contract to a command builder:
/// prompts rejected, editors disabled, ambient repo-location overrides
/// stripped, `LC_MESSAGES=C` for parseable errors while preserving a UTF-8
/// character locale.
fn apply_env(cmd: &mut tokio::process::Command) {
	for stripped in [
		"GIT_DIR",
		"GIT_COMMON_DIR",
		"GIT_WORK_TREE",
		"GIT_INDEX_FILE",
		"GIT_OBJECT_DIRECTORY",
		"GIT_ALTERNATE_OBJECT_DIRECTORIES",
	] {
		cmd.env_remove(stripped);
	}
	if let Some(lc_all) = std::env::var_os("LC_ALL") {
		let lc_all = lc_all.to_string_lossy().into_owned();
		if is_utf8_locale(&lc_all) {
			cmd.env("LC_CTYPE", lc_all);
		}
	}
	cmd.env_remove("LC_ALL");
	cmd.env("LC_MESSAGES", "C");
	cmd.env("GIT_OPTIONAL_LOCKS", "0");
	cmd.env("GIT_ASKPASS", "true");
	cmd.env("GIT_EDITOR", "true");
	cmd.env("GIT_TERMINAL_PROMPT", "0");
	cmd.env("SSH_ASKPASS", "false");
}

/// Loose match for a UTF-8 character locale (`en_US.UTF-8`, `C.utf8`, …).
fn is_utf8_locale(value: &str) -> bool {
	let lower = value.to_ascii_lowercase();
	["utf-8", "utf8"].iter().any(|needle| {
		lower
			.split(['.', '_', '-', '@'])
			.any(|part| part == *needle)
			|| lower.ends_with(needle)
			|| lower.contains(&format!(".{needle}"))
	})
}

/// Run `git` asynchronously with bounded capture and a deadline.
/// Non-zero exits are returned in [`CliOutput`], not raised.
pub async fn run(cwd: &Path, args: &[String], options: &RunOptions) -> Result<CliOutput> {
	let argv = hardened_args(args, options.read_only);
	let mut cmd = tokio::process::Command::new("git");
	cmd.args(&argv)
		.current_dir(cwd)
		.stdin(if options.stdin.is_some() {
			Stdio::piped()
		} else {
			Stdio::null()
		})
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true);
	apply_env(&mut cmd);
	let mut child = cmd.spawn().map_err(|err| spawn_error(cwd, err))?;

	if let Some(stdin) = &options.stdin
		&& let Some(mut pipe) = child.stdin.take()
	{
		use tokio::io::AsyncWriteExt;
		// A child that exits early (e.g. usage error) closes the pipe;
		// that is its answer, not ours to fail on.
		let _ = pipe.write_all(stdin).await;
		let _ = pipe.shutdown().await;
	}

	let stdout = child.stdout.take().expect("stdout piped");
	let stderr = child.stderr.take().expect("stderr piped");
	let stdout_task = tokio::spawn(read_capped(stdout));
	let stderr_task = tokio::spawn(read_capped(stderr));

	let timeout = options.timeout.unwrap_or(COMMAND_TIMEOUT);
	let cancel = options.cancel.clone().unwrap_or_default();
	let exit = tokio::select! {
		status = tokio::time::timeout(timeout, child.wait()) => match status {
			Ok(status) => status.map_err(Error::Io)?,
			Err(_elapsed) => {
				terminate(&mut child).await;
				stdout_task.abort();
				stderr_task.abort();
				return Err(Error::CliTimeout { command: format!("git {}", argv.join(" ")) });
			},
		},
		() = cancel.cancelled() => {
			terminate(&mut child).await;
			stdout_task.abort();
			stderr_task.abort();
			return Err(Error::Canceled);
		},
	};

	let stdout = stdout_task
		.await
		.map_err(|err| Error::backend("git run", err))??;
	let stderr = stderr_task
		.await
		.map_err(|err| Error::backend("git run", err))??;
	Ok(CliOutput { exit_code: exit.code().unwrap_or(-1), stdout, stderr })
}

/// Run `git` asynchronously and map a non-zero exit to [`Error::Cli`].
pub async fn run_checked(cwd: &Path, args: &[String], options: &RunOptions) -> Result<CliOutput> {
	run(cwd, args, options).await?.into_checked(args)
}

/// Synchronous bounded runner for render paths (reftable ref reads). Enforces
/// [`SYNC_TIMEOUT`] by polling so a stalled git cannot freeze rendering.
pub fn run_sync(cwd: &Path, args: &[String]) -> Result<CliOutput> {
	let argv = hardened_args(args, true);
	let mut cmd = std::process::Command::new("git");
	cmd.args(&argv)
		.current_dir(cwd)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped());
	// The sync and async builders expose the same env API surface; reuse the
	// async configurator through a tokio wrapper would allocate a runtime, so
	// mirror the pins directly.
	for stripped in [
		"GIT_DIR",
		"GIT_COMMON_DIR",
		"GIT_WORK_TREE",
		"GIT_INDEX_FILE",
		"GIT_OBJECT_DIRECTORY",
		"GIT_ALTERNATE_OBJECT_DIRECTORIES",
	] {
		cmd.env_remove(stripped);
	}
	cmd.env("LC_MESSAGES", "C");
	cmd.env("GIT_OPTIONAL_LOCKS", "0");
	cmd.env("GIT_TERMINAL_PROMPT", "0");
	let mut child = cmd.spawn().map_err(|err| spawn_error(cwd, err))?;

	let deadline = std::time::Instant::now() + SYNC_TIMEOUT;
	loop {
		match child.try_wait()? {
			Some(status) => {
				let output = child.wait_with_output()?;
				return Ok(CliOutput {
					exit_code: status.code().unwrap_or(-1),
					stdout:    String::from_utf8_lossy(&output.stdout).into_owned(),
					stderr:    String::from_utf8_lossy(&output.stderr).into_owned(),
				});
			},
			None if std::time::Instant::now() >= deadline => {
				let _ = child.kill();
				let _ = child.wait();
				return Err(Error::CliTimeout { command: format!("git {}", argv.join(" ")) });
			},
			None => std::thread::sleep(Duration::from_millis(10)),
		}
	}
}

fn spawn_error(cwd: &Path, err: std::io::Error) -> Error {
	if err.kind() == std::io::ErrorKind::NotFound {
		// A deleted cwd also surfaces as spawn ENOENT; only blame the binary
		// when the working directory actually exists.
		if cwd.exists() {
			return Error::GitMissing;
		}
		return Error::CwdMissing { path: cwd.to_path_buf() };
	}
	Error::Io(err)
}

/// Read a stream to completion, capping retention at [`OUTPUT_LIMIT_BYTES`]
/// while continuing to drain so the child never blocks on a full pipe.
async fn read_capped(mut stream: impl tokio::io::AsyncRead + Unpin) -> Result<String> {
	let mut retained: Vec<u8> = Vec::new();
	let mut buf = [0u8; 8 * 1024];
	let mut truncated = false;
	loop {
		let n = stream.read(&mut buf).await.map_err(Error::Io)?;
		if n == 0 {
			break;
		}
		if truncated {
			continue;
		}
		let remaining = OUTPUT_LIMIT_BYTES - retained.len();
		if n <= remaining {
			retained.extend_from_slice(&buf[..n]);
		} else {
			retained.extend_from_slice(&buf[..remaining]);
			truncated = true;
		}
	}
	let mut text = String::from_utf8_lossy(&retained).into_owned();
	if truncated {
		text.push_str(TRUNCATION_MARKER);
	}
	Ok(text)
}

/// SIGTERM, grace period, then SIGKILL.
async fn terminate(child: &mut tokio::process::Child) {
	#[cfg(unix)]
	if let Some(pid) = child.id() {
		// SAFETY: plain kill(2) on a pid we own; no memory is touched.
		unsafe {
			libc::kill(pid as i32, libc::SIGTERM);
		}
		if tokio::time::timeout(TERMINATE_GRACE, child.wait())
			.await
			.is_ok()
		{
			return;
		}
	}
	let _ = child.kill().await;
	let _ = tokio::time::timeout(TERMINATE_GRACE, child.wait()).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Network operations (public API)
// ═══════════════════════════════════════════════════════════════════════════

impl GitRepo {
	/// Push the current branch (branch-scoped: never follows tags).
	///
	/// `--no-follow-tags` overrides a user's `push.followTags = true`, which
	/// would otherwise ride every reachable annotated tag along with the
	/// branch — rejected refs on remotes the user cannot tag (e.g. PR-head
	/// forks) would fail the call after the branch itself already updated.
	pub async fn push(
		&self,
		options: &PushOptions,
		cancel: Option<CancellationToken>,
	) -> Result<()> {
		let mut args = vec!["push".to_owned(), "--no-follow-tags".to_owned()];
		if let Some(lease) = &options.force_with_lease {
			args.push(if lease.is_empty() {
				"--force-with-lease".to_owned()
			} else {
				format!("--force-with-lease={lease}")
			});
		}
		if let Some(remote) = &options.remote {
			args.push(remote.clone());
		}
		if let Some(refspec) = &options.refspec {
			args.push(refspec.clone());
		}
		run_checked(self.root(), &args, &RunOptions {
			timeout: Some(NETWORK_TIMEOUT),
			cancel,
			..RunOptions::default()
		})
		.await?;
		Ok(())
	}

	/// Fetch `+source:target` from a remote.
	pub async fn fetch(
		&self,
		remote: &str,
		source: &str,
		target: &str,
		timeout: Option<Duration>,
		cancel: Option<CancellationToken>,
	) -> Result<()> {
		let args = vec!["fetch".to_owned(), remote.to_owned(), format!("+{source}:{target}")];
		run_checked(self.root(), &args, &RunOptions {
			timeout: Some(timeout.unwrap_or(NETWORK_TIMEOUT)),
			cancel,
			..RunOptions::default()
		})
		.await?;
		Ok(())
	}
}

/// Secret returned by the user's git credential helpers for one HTTPS host.
pub struct HelperCredential {
	/// Helper-reported username; informational only.
	pub username: String,
	/// Helper-held password or token.
	pub password: SecretString,
}

impl std::fmt::Debug for HelperCredential {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("HelperCredential")
			.field("username", &self.username)
			.finish_non_exhaustive()
	}
}

/// Asks the configured git credential helpers for `https://<host>`.
///
/// The lookup is strictly non-interactive: the hardened environment rejects
/// terminal and askpass prompts and `credential.interactive=false` keeps GUI
/// helpers (Git Credential Manager) from opening a window. A miss, a refused
/// prompt, an empty secret, or a host that could inject credential-protocol
/// fields all resolve to `Ok(None)`. Nothing is ever approved or rejected
/// back to the helpers, so a lookup never mutates the user's credential store.
pub async fn credential_fill(
	cwd: &Path,
	host: &str,
	cancel: Option<CancellationToken>,
) -> Result<Option<HelperCredential>> {
	if host.is_empty()
		|| host
			.bytes()
			.any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace() || byte == b'=')
	{
		return Ok(None);
	}
	let args = [
		"-c".to_owned(),
		"credential.interactive=false".to_owned(),
		"credential".to_owned(),
		"fill".to_owned(),
	];
	let output = run(cwd, &args, &RunOptions {
		read_only: true,
		timeout: Some(CREDENTIAL_TIMEOUT),
		stdin: Some(format!("protocol=https\nhost={host}\n\n").into_bytes()),
		cancel,
	})
	.await?;
	if output.exit_code != 0 {
		return Ok(None);
	}
	Ok(parse_credential_fill(&output.stdout))
}

/// Parses `git credential fill` output, discarding empty secrets.
fn parse_credential_fill(stdout: &str) -> Option<HelperCredential> {
	let mut username = String::new();
	let mut password = None;
	for line in stdout.lines() {
		if let Some(value) = line.strip_prefix("username=") {
			value.clone_into(&mut username);
		} else if let Some(value) = line.strip_prefix("password=")
			&& !value.is_empty()
		{
			password = Some(SecretString::from(value.to_owned()));
		}
	}
	password.map(|password| HelperCredential { username, password })
}

/// Clone `url` into `target_dir`, removing the partial clone on any failure.
///
/// Shallow (`--depth 1 --single-branch`) unless a specific SHA is pinned:
/// a shallow clone only fetches the tip, so checking out a non-tip commit
/// would fail with "reference is not a tree".
pub async fn clone(
	url: &str,
	target_dir: &Path,
	options: &CloneOptions,
	cancel: Option<CancellationToken>,
) -> Result<()> {
	let absolute = std::path::absolute(target_dir)?;
	let parent = absolute
		.parent()
		.map_or_else(|| absolute.clone(), Path::to_owned);
	tokio::fs::create_dir_all(&parent).await?;

	let shallow = options.sha.is_none();
	let mut args = vec!["clone".to_owned()];
	if shallow {
		args.push("--depth".to_owned());
		args.push("1".to_owned());
	}
	if let Some(ref_name) = &options.ref_name {
		args.push("--branch".to_owned());
		args.push(ref_name.clone());
		args.push("--single-branch".to_owned());
	} else if shallow {
		args.push("--single-branch".to_owned());
	}
	args.push(url.to_owned());
	args.push(absolute.to_string_lossy().into_owned());

	let run_options = RunOptions {
		timeout: Some(options.timeout.unwrap_or(NETWORK_TIMEOUT)),
		cancel: cancel.clone(),
		..RunOptions::default()
	};
	let outcome = run_checked(&parent, &args, &run_options).await;
	if let Err(err) = outcome {
		let _ = tokio::fs::remove_dir_all(&absolute).await;
		return Err(err);
	}

	if let Some(sha) = &options.sha {
		let checkout = run_checked(&absolute, &["checkout".to_owned(), sha.clone()], &RunOptions {
			cancel,
			..RunOptions::default()
		})
		.await;
		if checkout.is_err() {
			let _ = tokio::fs::remove_dir_all(&absolute).await;
			return Err(Error::backend(
				"git clone",
				format!("failed to checkout SHA {sha} in cloned repository {url}"),
			));
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use omp_core::ExposeSecret as _;

	use super::*;

	#[test]
	fn credential_fill_output_keeps_only_non_empty_secrets() {
		let filled =
			parse_credential_fill("protocol=https\nhost=github.com\nusername=x\npassword=tok\n")
				.expect("password present");
		assert_eq!(filled.username, "x");
		assert_eq!(filled.password.expose_secret(), "tok");
		assert!(
			parse_credential_fill("protocol=https\nhost=github.com\nusername=\npassword=\n").is_none()
		);
		assert!(parse_credential_fill("").is_none());
	}

	#[tokio::test]
	async fn credential_fill_rejects_protocol_injection_without_spawning() {
		let dir = tempfile::tempdir().unwrap();
		for host in ["", "github.com\npassword=x", "a b", "host=evil", "x\0y"] {
			assert!(
				credential_fill(dir.path(), host, None)
					.await
					.unwrap()
					.is_none(),
				"{host:?}"
			);
		}
	}

	#[tokio::test]
	async fn credential_fill_reads_a_configured_helper_and_misses_without_one() {
		let dir = tempfile::tempdir().unwrap();
		run_checked(dir.path(), &["init".to_owned(), "-q".to_owned()], &RunOptions::default())
			.await
			.unwrap();
		// An empty entry resets helpers inherited from global config.
		for helper in
			["", "!f() { test \"$1\" = get && echo username=bot && echo password=s3cret; }; f"]
		{
			run_checked(
				dir.path(),
				&[
					"config".to_owned(),
					"--add".to_owned(),
					"credential.helper".to_owned(),
					helper.to_owned(),
				],
				&RunOptions::default(),
			)
			.await
			.unwrap();
		}
		let filled = credential_fill(dir.path(), "github.example.com", None)
			.await
			.unwrap()
			.expect("helper answered");
		assert_eq!(filled.username, "bot");
		assert_eq!(filled.password.expose_secret(), "s3cret");

		let empty = tempfile::tempdir().unwrap();
		run_checked(empty.path(), &["init".to_owned(), "-q".to_owned()], &RunOptions::default())
			.await
			.unwrap();
		run_checked(
			empty.path(),
			&["config".to_owned(), "credential.helper".to_owned(), String::new()],
			&RunOptions::default(),
		)
		.await
		.unwrap();
		assert!(
			credential_fill(empty.path(), "github.example.com", None)
				.await
				.unwrap()
				.is_none()
		);
	}
}
