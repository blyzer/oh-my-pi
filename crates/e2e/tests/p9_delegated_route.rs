#![cfg(unix)]

//! Reproduces PR #720 review finding P1: the delegated backend sends
//! `exec/start` as a top-level `InvokeTool`, and the server cannot route it.
//!
//! `DelegatedBackend::call_device` opens `InvokeTool { name: "exec/<op>", rev:
//! "buildbuddy.1" }`. The server resolves that name against
//! `Registry::live_identity`. The BuildBuddy extension's leaves are device
//! leaves, mounted by `mount_many` into the device catalog on activation, and
//! the client protocol has no frame that invokes a mounted device leaf (device
//! calls reach `DevicesHost` as `ShellDyn`, from inside a session).
//!
//! This installs the real `extensions/buildbuddy` package as a user extension,
//! admits it through the production driver and makes that call. It pins what
//! happens today. When a route for delegated device leaves exists, this test is
//! the one to turn around: assert the call is accepted.
//!
//! It skips off macOS, where a user extension's host needs Seatbelt.

use std::{
	fs,
	path::{Path, PathBuf},
	time::Duration,
};

use omp_e2e::{
	Context as _, Result, error,
	support::{InProcessEnvHarness, OperatorHome, Scratch, within},
};
use omp_proto::env::v1::InvokeTool;

const EXTENSION_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/buildbuddy");
const EXTENSION_ID: &str = "omp.buildbuddy";
const STEP_LIMIT: Duration = Duration::from_secs(60);

fn runs_user_extension_hosts() -> Result<bool> {
	if !cfg!(target_os = "macos") {
		eprintln!("P9 delegated route skipped: a user extension's host needs Seatbelt (macOS only)");
		return Ok(false);
	}
	if let Some(failure) = omp_sandbox::backend_status(omp_sandbox::Backend::Seatbelt).failure() {
		return Err(error(format!("P9 delegated route needs Seatbelt: {failure} ({failure:?})")));
	}
	Ok(true)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
	fs::create_dir_all(to).context("creating a destination directory")?;
	for entry in fs::read_dir(from).context("reading a source directory")? {
		let entry = entry.context("reading a directory entry")?;
		let name = entry.file_name();
		if name == "__pycache__" || name == "tests" {
			continue;
		}
		let target = to.join(&name);
		if entry.file_type().context("reading a file type")?.is_dir() {
			copy_tree(&entry.path(), &target)?;
		} else {
			fs::copy(entry.path(), &target).context("copying a file")?;
		}
	}
	Ok(())
}

#[tokio::test]
async fn a_top_level_invoke_of_an_exec_leaf_is_not_found_today() -> Result<()> {
	if !runs_user_extension_hosts()? {
		return Ok(());
	}
	let operator = OperatorHome::new()?;
	let scratch = Scratch::new()?;
	let project = scratch.project().to_path_buf();
	let root: PathBuf = operator.extension_root().join(EXTENSION_ID);
	copy_tree(Path::new(EXTENSION_SRC), &root)?;
	// The shipped manifest declares `[requires] python = ">=3.14"`, a table, where
	// the native manifest parser takes `requires = [..]`, a list, so production
	// admission rejects it ("invalid type: map, expected a sequence"). Drop that
	// table here so the route question can be asked at all.
	let manifest = root.join("omp.toml");
	let text = fs::read_to_string(&manifest).context("reading the shipped manifest")?;
	let without_requires = text.split("\n[requires]").next().unwrap_or(&text);
	fs::write(&manifest, format!("{without_requires}\n"))
		.context("writing the adjusted manifest")?;

	let admission = operator.admit(&project, EXTENSION_ID)?;
	let harness =
		InProcessEnvHarness::spawn_with_admitted_extension(scratch, &admission.extension).await?;
	let registry = harness.server().registry();

	for name in ["exec", "exec/describe", "exec/start", "exec/poll", "exec/get_log"] {
		assert!(
			registry.live_identity(name).is_none(),
			"`{name}` is a live tool identity, so the finding no longer holds and this test must be \
			 turned around"
		);
	}
	let mounted = registry
		.devices()
		.map(|device| device.name.to_string())
		.collect::<Vec<_>>();
	assert!(
		!mounted.iter().any(|name| name == "exec"),
		"`exec` is mounted before anything activated the extension: {mounted:?}"
	);

	// The call `DelegatedBackend::call_device` makes, for every operation it uses.
	for leaf in ["exec/describe", "exec/start", "exec/poll", "exec/get_log"] {
		let mut invocation = within(
			"open the leaf invocation",
			STEP_LIMIT,
			harness.client().invoke(InvokeTool {
				invocation_id: format!("p9-delegated-{}", leaf.replace('/', "-")),
				name: leaf.to_owned(),
				rev: "buildbuddy.1".to_owned(),
				..InvokeTool::default()
			}),
		)
		.await?
		.with_context(|| format!("opening the {leaf} invocation"))?;
		let first = within("first event", STEP_LIMIT, invocation.next_event()).await?;
		let refusal = format!("{first:?}");
		assert!(
			refusal.contains("NotFound") && refusal.contains("not registered"),
			"`InvokeTool {{ name: {leaf:?} }}` was expected to be NotFound (the finding), got \
			 {refusal}"
		);
	}
	Ok(())
}
