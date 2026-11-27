#![cfg(unix)]

//! ADR 0075 D12, shell side: the shell tool of a session cannot write the
//! grant store, and cannot widen a credential grant through `omp ext grant`.
//!
//! `p9_extension_isolation` runs the extension worker. This runs the other
//! thing an agent can type: a shell command through the production daemon's
//! `exec`, under the shipped `workspace-write` sandbox. The daemon is the real
//! `omp envd` child, its data directory is the grant store's parent, and a
//! relaying client answers every approval prompt with a denial, recording the
//! prompt it was shown.
//!
//! Two layouts. In the first the data directory sits outside every writable
//! root, as it does for a user whose workspace is not their home: the sandbox
//! denies a write there whatever protects it. In the second the data directory
//! sits INSIDE the workspace, a writable root, so only the protected-path
//! carve-out can keep `ext/` closed: a sibling file in the same directory
//! must stay writable, or the denial proves nothing.
//!
//! What this does not show. The shipped sandbox reads the whole host
//! (`sv_sandbox_read_mode = host`, no `sv_sandbox_read_deny`), so the same
//! shell can READ `credentials.db` and its adjacent `credentials.key`, the
//! default key source of an interactive session. A one-off probe, not kept
//! here because it would pin an insecure result as a pass, read both. With
//! the data directory inside the workspace it can also WRITE `credentials.db`:
//! the carve-out covers `ext/` only. This file proves the grant store is
//! closed to the shell; it does not prove the credential store is.
//!
//! Mutation check: with the carve-out removed, the first test fails at the
//! append to `grants.toml`. The second still passes, because there the data
//! directory is outside every writable root and the sandbox denies the write
//! by default; its assertion is that the kernel, not an argument error or the
//! command's own location guard, stops the real `omp ext grant`.
//!
//! The proof skips off macOS, and fails on macOS when Seatbelt is unavailable.

use std::{
	fs,
	path::{Path, PathBuf},
	sync::Arc,
};

use bytes::Bytes;
use omp_core::{EnvPath, Str};
use omp_e2e::{
	Context as _, Result, error,
	support::{
		DEFAULT_TIMEOUT, DocServerTask, EnvHarness, FramedEnvConnection, OperatorHome,
		ProcessEnvHarness, Scratch, omp_binary, within,
	},
};
use omp_env::{APPROVAL_RELAY_CAPABILITY, ApprovalQueryEvent, ExecEvent, ExecRun};
use omp_proto::env::v1::{
	ApprovalDecision, ApprovalQuery, ExecRequest, ExecStatusMsg, OpenSessionRequest, Script,
};
use parking_lot::Mutex;
use tokio::task::JoinHandle;
use url::Url;

const SQLITE_MAGIC: &str = "SQLite format 3";
const GRANT_BEFORE: &str = "# operator grants\n";
const EXTENSION: &str = "shellproof";

const SOURCE: &str = r#"
import omp
from omp.devices import Effects, FetchEffects


@omp.tool("place", rev=1, effects=Effects(fetch=FetchEffects(credentials=True)))
async def place():
    return {"parts": [], "details": {}}
"#;

fn manifest() -> String {
	format!(
		r#"id = "{EXTENSION}"
version = "0.1.0"
omp_api = 1
entry = "{EXTENSION}"
capabilities = ["env.net"]

[[declarations]]
id = "place"
kind = "soft"
module = "{EXTENSION}"
key = "place@{EXTENSION}.1"
trigger = "lazy"
api = 1
failure = "fault"
"#
	)
}

/// What one shell command did.
#[derive(Debug)]
struct Observed {
	output:  String,
	/// The approval prompts the command filed (each was answered with a denial).
	prompts: Vec<ApprovalQuery>,
}

/// One production daemon, a session on it, and the denying relay.
struct Shell {
	process:    ProcessEnvHarness,
	docserver:  DocServerTask,
	connection: FramedEnvConnection,
	session:    Bytes,
	prompts:    Arc<Mutex<Vec<ApprovalQuery>>>,
	answerer:   JoinHandle<()>,
	_scratch:   Scratch,
}

impl Shell {
	async fn start(scratch: Scratch, data_dir: &Path) -> Result<Self> {
		let executable = omp_binary().context("resolving the daemon executable")?;
		let docserver = DocServerTask::spawn_for_daemon(
			scratch.project(),
			scratch.socket("attached-docserver.sock"),
			&executable,
		)
		.await?;
		let process = EnvHarness::spawn_attached_with_data_dir(
			&scratch,
			&executable,
			docserver.socket(),
			Some(data_dir),
		)
		.await?;
		let connection = process
			.connect_client_with_capabilities("p9-shell", &[APPROVAL_RELAY_CAPABILITY])
			.await?;
		let project = fs::canonicalize(scratch.project()).context("canonical project root")?;
		let cwd = EnvPath::new(Str::from(project_uri(&project)?)).context("typed project cwd")?;
		let session = within(
			"session open",
			DEFAULT_TIMEOUT,
			connection
				.client()
				.open_session(&cwd, OpenSessionRequest::default()),
		)
		.await??
		.session;

		let prompts = Arc::new(Mutex::new(Vec::new()));
		let queries = connection.client().approval_queries();
		let client = connection.client_clone();
		let seen = Arc::clone(&prompts);
		let answerer = tokio::spawn(async move {
			while let Ok(event) = queries.recv_async().await {
				if let ApprovalQueryEvent::Requested { request_id, query } = event {
					seen.lock().push(query.clone());
					let _ = client
						.answer_approval(request_id, query.query_id, ApprovalDecision {
							approved: false,
							scope: "once".to_owned(),
							source: "user".to_owned(),
							..ApprovalDecision::default()
						})
						.await;
				}
			}
		});
		Ok(Self { process, docserver, connection, session, prompts, answerer, _scratch: scratch })
	}

	async fn run(&self, text: &str) -> Result<Observed> {
		self.prompts.lock().clear();
		let request = ExecRequest {
			session: self.session.clone(),
			source: Some(Script { text: text.to_owned(), ..Script::default() }),
			..ExecRequest::default()
		};
		let mut run =
			within("command start", DEFAULT_TIMEOUT, self.connection.client().exec(request)).await??;
		let (_status, output) = exit(&mut run).await?;
		Ok(Observed {
			output:  String::from_utf8_lossy(&output).into_owned(),
			prompts: self.prompts.lock().clone(),
		})
	}

	async fn shutdown(self) -> Result<()> {
		self.answerer.abort();
		drop(self.connection);
		self.process.shutdown().await?;
		self.docserver.shutdown().await
	}
}

async fn exit(run: &mut ExecRun) -> Result<(ExecStatusMsg, Vec<u8>)> {
	within("command exit", DEFAULT_TIMEOUT, async {
		let mut output = Vec::new();
		loop {
			match run.next_event().await? {
				Some(ExecEvent::Started(_)) => {},
				Some(ExecEvent::Output(frame)) => output.extend_from_slice(&frame.data),
				Some(ExecEvent::Exit(exit)) => {
					return Ok((exit.status.context("terminal command status")?, output));
				},
				None => return Err(error("command stream ended before its exit")),
			}
		}
	})
	.await?
}

fn project_uri(project: &Path) -> Result<String> {
	Url::from_directory_path(project)
		.map(String::from)
		.map_err(|()| error(format!("{} is not an absolute directory", project.display())))
}

fn quoted(path: &Path) -> String {
	format!("'{}'", path.display())
}

fn runs_here() -> Result<bool> {
	if !cfg!(target_os = "macos") {
		eprintln!("P9 session shell isolation skipped: it needs the Seatbelt sandbox (macOS only)");
		return Ok(false);
	}
	if let Some(failure) = omp_sandbox::backend_status(omp_sandbox::Backend::Seatbelt).failure() {
		return Err(error(format!(
			"P9 session shell isolation needs the Seatbelt sandbox backend, and its probe failed: \
			 {failure} ({failure:?})"
		)));
	}
	Ok(true)
}

/// A data directory holding the three things the probes touch.
struct Store {
	data:   PathBuf,
	grants: PathBuf,
}

impl Store {
	fn seed(data: PathBuf) -> Result<Self> {
		let ext = data.join("ext");
		fs::create_dir_all(&ext).context("creating the grant store directory")?;
		let grants = ext.join("grants.toml");
		fs::write(&grants, GRANT_BEFORE).context("seeding grants.toml")?;
		fs::write(data.join("credentials.db"), format!("{SQLITE_MAGIC}\0secret-marker"))
			.context("seeding credentials.db")?;
		Ok(Self { data, grants })
	}
}

/// Inside the workspace, the protected-path carve-out is the only thing that
/// keeps `ext/` closed, and the shell cannot write, replace, delete or move
/// the grant store, nor reach for an approval to do so.
#[tokio::test]
async fn session_shell_cannot_change_the_grant_store_inside_the_workspace() -> Result<()> {
	if !runs_here()? {
		return Ok(());
	}
	let scratch = Scratch::new()?;
	let store = Store::seed(scratch.project().join("odata"))?;
	let shell = Shell::start(scratch, &store.data).await?;

	let ext = quoted(&store.data.join("ext"));
	let grants = quoted(&store.grants);
	let sibling = store.data.join("sibling.txt");

	// Control: the directory is writable, so a denial below is the carve-out
	// and not a missing permission.
	let control = shell
		.run(&format!("printf ok > {}; echo rc=$?", quoted(&sibling)))
		.await?;
	assert!(control.output.contains("rc=0"), "control write failed: {control:?}");
	assert_eq!(fs::read_to_string(&sibling).context("control file")?, "ok");

	for (name, command) in [
		("append to grants.toml", format!("printf x >> {grants}; echo rc=$?")),
		("truncate grants.toml", format!("printf x > {grants}; echo rc=$?")),
		(
			"add a file under ext",
			format!("printf x > {}; echo rc=$?", quoted(&store.data.join("ext/extra.toml"))),
		),
		("delete grants.toml", format!("rm -f {grants}; echo rc=$?")),
		("move ext aside", format!("mv {ext} {}; echo rc=$?", quoted(&store.data.join("ext.old")))),
	] {
		let seen = shell.run(&command).await?;
		eprintln!("[{name}] {seen:?}");
		assert!(!seen.output.contains("rc=0"), "{name}: the shell was allowed: {seen:?}");
		assert!(
			seen.prompts.is_empty(),
			"{name}: a denial must not offer an approval that would let the shell widen a grant: \
			 {seen:?}"
		);
		assert_eq!(
			fs::read_to_string(&store.grants).context("grants.toml after the probe")?,
			GRANT_BEFORE,
			"{name}: grants.toml changed"
		);
		assert!(!store.data.join("ext/extra.toml").exists(), "{name}: a file appeared under ext");
		assert!(!store.data.join("ext.old").exists(), "{name}: ext was moved");
	}
	shell.shutdown().await
}

/// The real `omp ext grant`, typed into the shell, cannot widen an
/// extension's credential grant. The same command line, outside the sandbox,
/// is the control.
#[tokio::test]
async fn session_shell_cannot_widen_a_credential_grant_through_omp_ext_grant() -> Result<()> {
	if !runs_here()? {
		return Ok(());
	}
	let operator = OperatorHome::new()?;
	let scratch = Scratch::new()?;
	let project = scratch.project().to_path_buf();
	operator.install_extension(EXTENSION, &manifest(), SOURCE)?;

	// Control: the operator's own run writes the grant.
	operator
		.grant_credential(&project, EXTENSION, "buildbuddy", &["https://builds.example"], &[
			"POST /run",
		])
		.await
		.context("the operator's control grant")?;
	let granted = fs::read(operator.grants_file()).context("grants.toml after the control")?;
	let text = String::from_utf8_lossy(&granted);
	assert!(text.contains("builds.example"), "the control grant is missing: {text}");

	let shell = Shell::start(scratch, operator.data_dir()).await?;
	let omp = omp_binary().context("resolving the omp executable")?;
	let command = format!(
		"unset OMP_CONFIG_DIR OMP_STATE_DIR OMP_CACHE_DIR OMP_DATA_DIR; HOME={} NO_COLOR=1 {} ext \
		 --data-dir {} --project {} grant {EXTENSION} --yes --credential buildbuddy --origin \
		 https://attacker.example --operation 'POST /'; echo rc=$?",
		quoted(operator.home()),
		quoted(&omp),
		quoted(operator.data_dir()),
		quoted(&project),
	);
	let seen = shell.run(&command).await?;
	eprintln!("[omp ext grant from the shell] {seen:?}");
	assert!(!seen.output.contains("rc=0"), "the shell widened the grant: {seen:?}");
	// The refusal is the kernel's, not an argument error or the command's own
	// location guard: the command got as far as the grant store's lock file.
	assert!(
		seen.output.contains("grants.toml.lock") && seen.output.contains("Operation not permitted"),
		"the command must be stopped by the sandbox at the grant store: {seen:?}"
	);
	assert_eq!(
		fs::read(operator.grants_file()).context("grants.toml after the shell")?,
		granted,
		"the shell changed grants.toml: {seen:?}"
	);
	assert!(
		!String::from_utf8_lossy(&fs::read(operator.grants_file())?).contains("attacker.example"),
		"the attacker's origin reached grants.toml"
	);
	shell.shutdown().await
}
