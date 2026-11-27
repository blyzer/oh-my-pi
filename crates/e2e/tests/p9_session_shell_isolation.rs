#![cfg(unix)]

//! ADR 0075 D12, shell side: the shell tool of a session cannot write the
//! grant store, and cannot widen a credential grant through `omp ext grant`.
//!
//! `p9_extension_isolation` runs the extension worker. This runs the other
//! thing an agent can type: a shell command through the production daemon's
//! `exec`, under the shipped `workspace-write` sandbox. The daemon is the real
//! `omp envd` child, its data directory is the grant store's parent, and a
//! relaying client answers every approval prompt with YES, recording the
//! prompt it was shown: the worst case, a person who approves everything the
//! sandbox offers.
//!
//! Two layouts. In the first the data directory sits outside every writable
//! root, as it does for a user whose workspace is not their home: the sandbox
//! denies a write there whatever protects it. In the second the data directory
//! sits INSIDE the workspace, a writable root, so only the protected-path
//! carve-out can keep `ext/` closed: a sibling file in the same directory
//! must stay writable, or the denial proves nothing.
//!
//! The credential store. The shipped sandbox reads the whole host
//! (`sv_sandbox_read_mode = host`, no `sv_sandbox_read_deny`), and the key
//! that opens `credentials.db` sits beside it as `credentials.key`, the
//! default key source of an interactive session. The sandbox therefore denies
//! the shell both reads and writes of the credential store files by
//! construction, with no setting involved: the last two tests type commands
//! that read, copy, write, delete and move them, builtin and external, directly
//! and through a link, under both layouts, while a neighbouring file in the
//! same directory stays readable. They are the proof that the credential
//! store is closed to the shell as well as the grant store.
//!
//! A trailing `echo rc=$?` makes a command exit 0, and the sandbox only offers
//! an approval for a denial that is the command's final status. A proof that
//! types every command with that suffix therefore sees no prompt whatever the
//! sandbox would offer. `refused_and_never_offered` types each command both
//! ways; the bare form is the one that can be offered an approval.
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
const KEY_MARKER: &str = "key-marker-0123";
const DB_MARKER: &str = "secret-marker";
const NEIGHBOUR_MARKER: &str = "neighbour-ok";

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
	/// The approval prompts the command filed (each was answered as the shell
	/// was started to).
	prompts: Vec<ApprovalQuery>,
}

/// One production daemon, a session on it, and the relay that answers
/// approvals.
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
	/// A shell whose relaying client answers every approval prompt with
	/// `approve`, recording the prompt it was shown. Approving is the worst
	/// case: a person who clicks through everything the sandbox offers.
	async fn start_answering(scratch: Scratch, data_dir: &Path, approve: bool) -> Result<Self> {
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
							approved: approve,
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
		use std::os::unix::fs::PermissionsExt as _;

		let ext = data.join("ext");
		fs::create_dir_all(&ext).context("creating the grant store directory")?;
		let grants = ext.join("grants.toml");
		fs::write(&grants, GRANT_BEFORE).context("seeding grants.toml")?;
		fs::write(data.join("credentials.db"), format!("{SQLITE_MAGIC}\0{DB_MARKER}"))
			.context("seeding credentials.db")?;
		// SQLite leaves its write-ahead log beside the database while it is open.
		fs::write(data.join("credentials.db-wal"), DB_MARKER).context("seeding the sidecar")?;
		let key = data.join("credentials.key");
		fs::write(&key, KEY_MARKER).context("seeding credentials.key")?;
		fs::set_permissions(&key, fs::Permissions::from_mode(0o600))
			.context("restricting credentials.key like the real key file")?;
		fs::write(data.join("notes.txt"), NEIGHBOUR_MARKER).context("seeding the neighbour")?;
		Ok(Self { data, grants })
	}
}

/// Types `command` the two ways an agent can, against a client that answers
/// every approval prompt with yes.
///
/// Followed by `echo rc=$?` it shows the command was refused, but the script
/// then exits 0, so its denial is never the command's final status and the
/// sandbox never offers an approval for it. Typed bare, the denial IS the final
/// status, which is when the sandbox would offer one: a prompt the client
/// would approve, handing the shell the location. Neither form may be offered.
async fn refused_and_never_offered(shell: &Shell, name: &str, command: &str) -> Result<()> {
	let masked = shell.run(&format!("{command}; echo rc=$?")).await?;
	eprintln!("[{name}] {masked:?}");
	assert!(!masked.output.contains("rc=0"), "{name}: the shell was allowed: {masked:?}");
	assert!(
		masked.output.contains("denied") || masked.output.contains("not permitted"),
		"{name}: refused, but not by the sandbox (a missing file would read the same): {masked:?}"
	);
	assert!(
		!masked.output.contains(KEY_MARKER) && !masked.output.contains(DB_MARKER),
		"{name}: the shell saw a credential: {masked:?}"
	);
	let bare = shell.run(command).await?;
	eprintln!("[{name}, bare] {bare:?}");
	assert!(
		bare.prompts.is_empty(),
		"{name}: a denial offered an approval that, once approved, would open the location: {bare:?}"
	);
	assert!(
		!bare.output.contains(KEY_MARKER) && !bare.output.contains(DB_MARKER),
		"{name}: the shell saw a credential after an approval: {bare:?}"
	);
	Ok(())
}

/// Inside the workspace, the protected-path carve-out is the only thing that
/// keeps `ext/` closed, and the shell cannot write, replace, delete or move
/// the grant store, even when every approval it is offered is approved.
#[tokio::test]
async fn session_shell_cannot_change_the_grant_store_inside_the_workspace() -> Result<()> {
	if !runs_here()? {
		return Ok(());
	}
	let scratch = Scratch::new()?;
	let store = Store::seed(scratch.project().join("odata"))?;
	let shell = Shell::start_answering(scratch, &store.data, true).await?;

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
		("append to grants.toml", format!("printf x >> {grants}")),
		("truncate grants.toml", format!("printf x > {grants}")),
		(
			"add a file under ext",
			format!("printf x > {}", quoted(&store.data.join("ext/extra.toml"))),
		),
		("delete grants.toml", format!("rm -f {grants}")),
		("move ext aside", format!("mv {ext} {}", quoted(&store.data.join("ext.old")))),
		// An external program is stopped by the kernel, not by the interpreter.
		("external append to grants.toml", format!("/bin/sh -c \"printf x >> {grants}\"")),
	] {
		refused_and_never_offered(&shell, name, &command).await?;
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

	let shell = Shell::start_answering(scratch, operator.data_dir(), true).await?;
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

/// The shell reads and writes nothing of the credential store, and gets no
/// approval prompt that would let it, even when the client approves every
/// prompt it is shown. A neighbouring file in the same directory is the
/// control: it reads normally, so the denials below are the credential rule
/// and not a missing permission.
async fn credential_store_is_closed_to_the_shell(inside_workspace: bool) -> Result<()> {
	let scratch = Scratch::new()?;
	let data = if inside_workspace {
		scratch.project().join("odata")
	} else {
		scratch.root().join("odata")
	};
	let store = Store::seed(data)?;
	let key = store.data.join("credentials.key");
	let db = store.data.join("credentials.db");
	let neighbour = store.data.join("notes.txt");
	let link = scratch.project().join("innocent-link");
	std::os::unix::fs::symlink(&key, &link).context("linking to the key")?;
	let stolen = scratch.project().join("stolen-key");
	let database_before = fs::read(&db).context("credentials.db before")?;
	let shell = Shell::start_answering(scratch, &store.data, true).await?;

	for reader in ["cat", "/bin/cat"] {
		let seen = shell
			.run(&format!("{reader} {}; echo rc=$?", quoted(&neighbour)))
			.await?;
		assert!(
			seen.output.contains(NEIGHBOUR_MARKER) && seen.output.contains("rc=0"),
			"control: `{reader}` could not read the neighbouring file: {seen:?}"
		);
	}

	for (name, command) in [
		("builtin cat of the key", format!("cat {}", quoted(&key))),
		("external cat of the key", format!("/bin/cat {}", quoted(&key))),
		("external cat of the database", format!("/bin/cat {}", quoted(&db))),
		("head of the database", format!("/usr/bin/head -c 16 {}", quoted(&db))),
		("cat from a redirect", format!("/bin/cat < {}", quoted(&key))),
		("builtin cat through a link", format!("cat {}", quoted(&link))),
		("external cat through a link", format!("/bin/cat {}", quoted(&link))),
		// The write-ahead log SQLite leaves beside the database holds recent pages.
		(
			"builtin cat of the write-ahead log",
			format!("cat {}", quoted(&store.data.join("credentials.db-wal"))),
		),
		("copy the key out", format!("/bin/cp {} {}", quoted(&key), quoted(&stolen))),
		("append to the database", format!("printf x >> {}", quoted(&db))),
		("truncate the key", format!("printf x > {}", quoted(&key))),
		("delete the key", format!("rm -f {}", quoted(&key))),
		(
			"move the database aside",
			format!("mv {} {}", quoted(&db), quoted(&store.data.join("db.old"))),
		),
	] {
		refused_and_never_offered(&shell, name, &command).await?;
		assert_eq!(
			fs::read_to_string(&key).context("credentials.key after the probe")?,
			KEY_MARKER,
			"{name}: credentials.key changed"
		);
		assert_eq!(
			fs::read(&db).context("credentials.db after the probe")?,
			database_before,
			"{name}: credentials.db changed"
		);
		assert!(!store.data.join("db.old").exists(), "{name}: the database was moved");
		assert!(
			fs::read_to_string(&stolen).map_or(true, |text| !text.contains(KEY_MARKER)),
			"{name}: the key was copied out"
		);
	}
	shell.shutdown().await
}

/// The data directory inside the workspace, a writable and readable root: only
/// the built-in rule can close the credential store here.
#[tokio::test]
async fn session_shell_cannot_read_or_write_the_credential_store_inside_the_workspace() -> Result<()>
{
	if !runs_here()? {
		return Ok(());
	}
	credential_store_is_closed_to_the_shell(true).await
}

/// The data directory outside the workspace, where the default read mode would
/// otherwise let the shell read it.
#[tokio::test]
async fn session_shell_cannot_read_or_write_the_credential_store_outside_the_workspace()
-> Result<()> {
	if !runs_here()? {
		return Ok(());
	}
	credential_store_is_closed_to_the_shell(false).await
}
