//! Joined proofs over the environment wire for plan-mode writes: the real
//! `write` tool, the real document host, and the invoking session's
//! `local://` scratch root, reached the way the agent's `EnvToolExecutor`
//! reaches them (`InvokeTool` with a session principal).

use std::{
	fs,
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use bytes::Bytes;
use omp_proto::env::v1 as wire;
use serde_json::{Value, json};
use tokio::time;

use crate::{ProjectEnvironment, RegistryBridges, tool_url::local::session_local_root};

const SESSION: &str = "plan-session";

/// One embedded environment over a scratch workspace.
struct Fixture {
	_directory:  tempfile::TempDir,
	root:        PathBuf,
	state:       PathBuf,
	environment: ProjectEnvironment,
}

/// What a `write` invocation settled as.
#[derive(Debug)]
enum Settled {
	/// The verdict's `CallOutcome` JSON.
	Verdict(Value),
	/// The environment refused the invocation before its tool ran.
	Refused(omp_env::ClientError),
}

impl Settled {
	fn is_ok(&self) -> bool {
		matches!(self, Self::Verdict(outcome) if outcome["kind"] == "ok")
	}

	fn text(&self) -> String {
		match self {
			Self::Verdict(outcome) => outcome.to_string(),
			Self::Refused(refusal) => refusal.to_string(),
		}
	}
}

impl Fixture {
	async fn start() -> Self {
		let directory = tempfile::tempdir().expect("scratch");
		let root = directory.path().join("workspace");
		let state = directory.path().join("state");
		fs::create_dir_all(&root).expect("workspace");
		fs::create_dir_all(&state).expect("state");
		let root = fs::canonicalize(&root).expect("canonical root");
		let environment = ProjectEnvironment::start_embedded(
			&root,
			&state,
			&omp_env::project_state::document_socket(&state),
			false,
			&[],
			&[],
			Arc::new(omp_con::Ctx::new()),
			RegistryBridges::default(),
		)
		.await
		.expect("environment");
		Self { _directory: directory, root, state, environment }
	}

	/// The scratch root `local://` names for `session`.
	fn local_root(&self, session: &str) -> PathBuf {
		session_local_root(&self.state.join("sessions"), session)
	}

	/// Invokes `write` as `session`'s kernel would, with the request's roster
	/// restrictions, and waits for its terminal.
	async fn write(
		&self,
		session: &str,
		id: &str,
		args: &Value,
		restrictions: Option<&omp_tool::ToolRestrictions>,
	) -> Settled {
		self.invoke(session, "write", id, args, restrictions).await
	}

	/// Invokes `name@2` as `session`'s kernel would and waits for its
	/// terminal.
	async fn invoke(
		&self,
		session: &str,
		name: &str,
		id: &str,
		args: &Value,
		restrictions: Option<&omp_tool::ToolRestrictions>,
	) -> Settled {
		let client = self
			.environment
			.client()
			.with_principal(session, "kernel")
			.expect("principal");
		let mut invocation = client
			.invoke(wire::InvokeTool {
				invocation_id: id.to_owned(),
				name: name.to_owned(),
				rev: "2".to_owned(),
				restrictions: restrictions.map(Into::into),
				..wire::InvokeTool::default()
			})
			.await
			.expect("invoke");
		let accepted = invocation.next_event().await.expect("accepted event");
		assert!(matches!(accepted, Some(omp_env::InvocationEvent::Accepted(_))), "{accepted:?}");
		invocation
			.commit_args(
				Bytes::from(serde_json::to_vec(args).expect("arguments")),
				Bytes::from_static(b"plan-writes-effect-token"),
				1000,
				None,
			)
			.await
			.expect("commit args");
		time::timeout(Duration::from_secs(10), async {
			loop {
				match invocation.next_event().await {
					Ok(Some(omp_env::InvocationEvent::Verdict(verdict))) => {
						break Settled::Verdict(
							serde_json::from_slice(&verdict.json).expect("CallOutcome JSON"),
						);
					},
					Ok(Some(_)) => {},
					Ok(None) => panic!("invocation closed before its verdict"),
					Err(error) => break Settled::Refused(error),
				}
			}
		})
		.await
		.expect("the invocation settles")
	}
}

fn read(path: &Path) -> String {
	fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Plan mode's default plan file is `local://PLAN.md`. `write` creates and
/// overwrites it inside the invoking session's scratch root (the root
/// `local://` reads and the plan review read), in both spellings, and refuses
/// targets that leave that root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_creates_the_local_plan_file_in_the_session_scratch_root() {
	let fixture = Fixture::start().await;
	let plan = fixture.local_root(SESSION).join("PLAN.md");

	let created = fixture
		.write(SESSION, "plan-1", &json!({"path": "local://PLAN.md", "content": "# Plan\n"}), None)
		.await;
	assert!(created.is_ok(), "{}", created.text());
	assert!(created.text().contains("local://PLAN.md"), "{}", created.text());
	assert_eq!(read(&plan), "# Plan\n");

	let rewritten = fixture
		.write(SESSION, "plan-2", &json!({"path": "local:/PLAN.md", "content": "# Plan v2\n"}), None)
		.await;
	assert!(rewritten.is_ok(), "{}", rewritten.text());
	assert_eq!(read(&plan), "# Plan v2\n");

	let nested = fixture
		.write(SESSION, "plan-3", &json!({"path": "local://plans/a.md", "content": "a\n"}), None)
		.await;
	assert!(nested.is_ok(), "{}", nested.text());
	assert_eq!(read(&fixture.local_root(SESSION).join("plans/a.md")), "a\n");

	for (id, escape) in [
		("escape-1", "local://../PLAN.md"),
		("escape-2", "local://x/../../PLAN.md"),
		("escape-3", "local:///etc/PLAN.md"),
		("escape-4", "local://"),
	] {
		let refused = fixture
			.write(SESSION, id, &json!({"path": escape, "content": "x"}), None)
			.await;
		assert!(!refused.is_ok(), "{escape}: {}", refused.text());
		assert!(
			refused.text().contains("local://"),
			"{escape} names the scheme it refused: {}",
			refused.text()
		);
	}
	assert!(!fixture.root.join("local:").exists(), "nothing lands in the workspace");
	assert!(
		!fixture
			.local_root(SESSION)
			.join("..")
			.join("PLAN.md")
			.exists()
	);

	// Another session's scratch root is its own.
	let other = fixture
		.write(
			"other-session",
			"other-1",
			&json!({"path": "local://PLAN.md", "content": "o\n"}),
			None,
		)
		.await;
	assert!(other.is_ok(), "{}", other.text());
	assert_eq!(read(&fixture.local_root("other-session").join("PLAN.md")), "o\n");
	assert_eq!(read(&plan), "# Plan v2\n");
}

/// A symlink inside the scratch root never carries a `local://` write out
/// of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_writes_refuse_symlinks_that_leave_the_scratch_root() {
	let fixture = Fixture::start().await;
	let local = fixture.local_root(SESSION);
	fs::create_dir_all(&local).expect("scratch root");
	let outside = fixture.root.join("outside");
	fs::create_dir_all(&outside).expect("outside directory");
	std::os::unix::fs::symlink(&outside, local.join("link")).expect("symlink");
	let refused = fixture
		.write(SESSION, "link-1", &json!({"path": "local://link/PLAN.md", "content": "x"}), None)
		.await;
	assert!(!refused.is_ok(), "{}", refused.text());
	assert!(refused.text().contains("escapes the session scratch root"), "{}", refused.text());
	assert!(!outside.join("PLAN.md").exists());
}

/// The kernel names its session by the SHA-256 of the journal path. That
/// session's `local://` root is the directory beside its journal, the root
/// the app's plan review reads (`<sessions>/<stem>/local`), and a `read` of
/// the plan file resolves to the same file the `write` created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_journaled_session_writes_its_plan_beside_its_journal() {
	let fixture = Fixture::start().await;
	let sessions = fixture.state.join("sessions");
	fs::create_dir_all(&sessions).expect("sessions directory");
	let journal = sessions.join("01JPLANSESSION.oms");
	fs::write(&journal, b"").expect("journal");
	let principal = omp_core::Hash32::sum(journal.as_os_str().as_encoded_bytes()).to_hex();
	let principal = principal.as_str();

	let written = fixture
		.write(principal, "plan-1", &json!({"path": "local://PLAN.md", "content": "# Plan\n"}), None)
		.await;
	assert!(written.is_ok(), "{}", written.text());
	assert_eq!(read(&sessions.join("01JPLANSESSION/local/PLAN.md")), "# Plan\n");
	assert!(
		!fixture.local_root(principal).exists(),
		"nothing lands in a root keyed by the digest, which `omp gc` would collect"
	);
	let read_back = fixture
		.invoke(principal, "read", "read-1", &json!({"path": "local://PLAN.md"}), None)
		.await;
	assert!(read_back.is_ok(), "{}", read_back.text());
	assert!(read_back.text().contains("# Plan"), "{}", read_back.text());
}

fn plan_restrictions() -> omp_tool::ToolRestrictions {
	omp_tool::ToolRestrictions::default()
		.with_allowlist(
			["read", "write", "lsp"]
				.map(omp_core::Str::new_static)
				.to_vec(),
			Some(omp_core::Str::new_static("plan")),
		)
		.with_plan_file(omp_core::Str::new_static("local://PLAN.md"))
}

fn read_only_restrictions() -> omp_tool::ToolRestrictions {
	omp_tool::ToolRestrictions::default()
		.with_read_only_ceiling(["read", "lsp"].map(omp_core::Str::new_static).to_vec())
}

/// Defense in depth behind the agent's dispatch check, over the real wire:
/// an invocation that carries plan mode's restrictions changes nothing but
/// the plan file, whatever reaches the environment, and the confinement ends
/// with plan mode (the next request carries no plan file).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_invocations_change_only_the_plan_file() {
	let fixture = Fixture::start().await;
	let plan = plan_restrictions();

	let off_plan = fixture
		.write(SESSION, "off-1", &json!({"path": "src/lib.rs", "content": "x"}), Some(&plan))
		.await;
	assert!(!off_plan.is_ok(), "{}", off_plan.text());
	assert!(
		off_plan
			.text()
			.contains("plan mode is active: the environment refused to change"),
		"{}",
		off_plan.text()
	);
	assert!(
		off_plan
			.text()
			.contains("only the plan file local://PLAN.md may change")
	);
	assert!(!fixture.root.join("src").exists(), "not even the parent directory appeared");

	for (id, target) in [
		("off-2", "local://notes.md".to_owned()),
		("off-3", fixture.state.join("elsewhere.md").display().to_string()),
	] {
		let refused = fixture
			.write(SESSION, id, &json!({"path": target, "content": "x"}), Some(&plan))
			.await;
		assert!(!refused.is_ok(), "{target}: {}", refused.text());
	}
	assert!(!fixture.local_root(SESSION).join("notes.md").exists());

	let planned = fixture
		.write(
			SESSION,
			"plan-1",
			&json!({"path": "local://PLAN.md", "content": "# Plan\n"}),
			Some(&plan),
		)
		.await;
	assert!(planned.is_ok(), "{}", planned.text());
	assert_eq!(read(&fixture.local_root(SESSION).join("PLAN.md")), "# Plan\n");

	// A tool that could write around the scoped writers (`ast_edit` applies
	// with `std::fs`) is refused at the boundary before it runs.
	fs::write(fixture.root.join("a.ts"), "old(1);\n").expect("source");
	let ast_edit = fixture
		.invoke(
			SESSION,
			"ast_edit",
			"ast-1",
			&json!({"ops": [{"pat": "old($A)", "out": "new($A)"}], "paths": ["a.ts"]}),
			Some(&plan),
		)
		.await;
	assert!(
		ast_edit
			.text()
			.contains("plan mode is active: the environment refused `ast_edit`"),
		"{}",
		ast_edit.text()
	);
	assert_eq!(read(&fixture.root.join("a.ts")), "old(1);\n");

	// Plan mode ended: the next invocation carries no plan file.
	let after = fixture
		.write(SESSION, "after-1", &json!({"path": "src/lib.rs", "content": "x"}), None)
		.await;
	assert!(after.is_ok(), "{}", after.text());
	assert_eq!(read(&fixture.root.join("src/lib.rs")), "x");
}

/// A subagent of a plan-mode session (its calls carry the read-only
/// ceiling) changes nothing, not even the parent's plan file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_subagent_invocations_change_nothing() {
	let fixture = Fixture::start().await;
	let ceiling = read_only_restrictions();
	for (id, target) in [("ro-1", "src/lib.rs"), ("ro-2", "local://PLAN.md")] {
		let refused = fixture
			.write(SESSION, id, &json!({"path": target, "content": "x"}), Some(&ceiling))
			.await;
		assert!(!refused.is_ok(), "{target}: {}", refused.text());
		assert!(
			refused.text().contains(
				"this agent is a read-only subagent of a plan-mode session: the environment refused \
				 to change"
			),
			"{}",
			refused.text()
		);
	}
	assert!(!fixture.root.join("src").exists());
	assert!(!fixture.local_root(SESSION).join("PLAN.md").exists());
}
