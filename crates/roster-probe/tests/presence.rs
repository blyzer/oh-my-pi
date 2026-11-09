//! The probe is live after `compose_kernel`'s attach path, and this crate is
//! the only new code that names it.

use std::{fs, sync::Arc};

use omp_envd::{
	ActiveContentInputs, AttachOptions, ProjectEnvironment, RegistryBridges,
	exec_settings::{ExecSandboxMode, SV_SANDBOX_MODE},
};
use omp_ext::workspace_trust::WorkspaceTrust;
use omp_roster_probe::{NAME, register_probe};
use omp_tools::roster::SelectedRoster;

#[tokio::test]
async fn compose_registry_contains_the_probe() {
	let scratch = tempfile::tempdir().expect("scratch");
	let root = scratch.path().join("workspace");
	let state = scratch.path().join("state");
	fs::create_dir_all(&root).expect("workspace");
	fs::create_dir_all(&state).expect("state");
	let root = fs::canonicalize(&root).expect("canonical workspace");
	let con = Arc::new(omp_con::Ctx::new());
	SV_SANDBOX_MODE
		.set(&con, ExecSandboxMode::Off)
		.expect("sandbox off");
	let environment = ProjectEnvironment::attach(&root, &state, AttachOptions {
		py_eval: false,
		approval_mode: None,
		trusted_extensions: Vec::new(),
		contributed_values: Vec::new(),
		con,
		bridges: RegistryBridges {
			content: ActiveContentInputs {
				workspace_trust: WorkspaceTrust::Trusted,
				..ActiveContentInputs::default()
			},
			selected_roster: Some(SelectedRoster::production().with_extension(register_probe)),
			..RegistryBridges::default()
		},
		spawn_idle_timeout: Some(2),
		spawn_policy: None,
	})
	.await
	.expect("embedded environment");
	assert!(
		environment.fallback_notice.is_some(),
		"this binary has no project daemon, so attach embeds the host"
	);
	assert!(
		environment.registry().live_identity(NAME).is_some(),
		"the roster extension must register {NAME}"
	);
	assert!(
		omp_tools::builtin_tool_identities()
			.iter()
			.all(|identity| identity.name != NAME),
		"the probe stays off the production identity list"
	);
}
