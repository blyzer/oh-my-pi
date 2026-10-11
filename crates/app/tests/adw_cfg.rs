//! `omp adw run` composes its workflow host under the operator's cfg.

use std::{env, fs, path::Path};

use omp_adw::{Posture, WriteScope};
use omp_app::adw_cmd::open_host;
use omp_core::Str;
use omp_driver::adw::AdwHost as _;

fn reported_posture(project: &Path, data: &Path) -> Posture {
	open_host(project, data.to_path_buf(), Str::new_static("unused-model"))
		.expect("workflow host")
		.posture()
		.expect("posture")
}

/// A user `config.cfg` that turns the sandbox off changes the posture the
/// workflow host reports, so phases are admitted against what the operator
/// configured rather than convar defaults.
#[test]
fn user_config_cfg_decides_the_reported_posture() {
	let config = tempfile::tempdir().expect("config directory");
	let data = tempfile::tempdir().expect("data directory");
	let project = tempfile::tempdir().expect("project directory");
	// SAFETY: nextest runs each test in its own process; nothing else reads the
	// variable concurrently.
	unsafe { env::set_var("OMP_CONFIG_DIR", config.path()) };

	let shipped = reported_posture(project.path(), data.path());
	fs::write(config.path().join("config.cfg"), "sv_sandbox_mode off\n").expect("user cfg");
	let configured = reported_posture(project.path(), data.path());

	assert_eq!(configured.write, WriteScope::Unconfined, "the user turned the sandbox off");
	if shipped.write != WriteScope::Unconfined {
		assert_ne!(shipped, configured, "config.cfg changed the reported posture");
	}
}
