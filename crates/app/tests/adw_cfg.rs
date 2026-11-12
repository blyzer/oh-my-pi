//! `omp adw run` composes its workflow host under the operator's cfg.

use std::{env, fs, path::Path};

use omp_adw::{PhaseKind, Posture, WriteScope};
use omp_app::adw_cmd::open_host;
use omp_core::Str;
use omp_driver::adw::AdwHost as _;

fn reported_posture(project: &Path, data: &Path) -> Posture {
	open_host(project, data.to_path_buf(), Str::new_static("unused-model"), false)
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

#[test]
fn an_explicit_model_flag_keeps_review_phases_on_that_model() {
	let project = tempfile::tempdir().expect("project");
	let data = project.path().join("data");
	let flagged = open_host(project.path(), data.clone(), Str::new_static("flag/model"), true)
		.expect("flagged host");
	assert_eq!(flagged.model_phase_selector_for(PhaseKind::Review), ("flag/model", true));
	let unflagged =
		open_host(project.path(), data, Str::new_static("default"), false).expect("unflagged host");
	assert_eq!(unflagged.model_phase_selector_for(PhaseKind::Review), ("@judge", true));
	assert_eq!(unflagged.model_phase_selector_for(PhaseKind::Agent), ("default", false));
}
