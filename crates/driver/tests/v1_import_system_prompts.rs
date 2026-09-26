//! Proves the v1 import puts `SYSTEM.md`, `APPEND_SYSTEM.md` and
//! `TITLE_SYSTEM.md` where v2's prompt discovery reads them
//! (`prompt_input::user_prompt_path`), for the default and a named profile,
//! with both versions resolved from the process environment as production
//! resolves them.

use std::{fs, path::Path};

use omp_driver::{
	prompt_input::{discover_prompt_file, user_prompt_path},
	v1_import::{
		CredentialAccess, ImportMode, ImportStep, ProfileSelection, V1Inputs, V1Source, V2Roots,
		plan, run,
	},
};

const PROMPTS: [&str; 3] = ["SYSTEM.md", "APPEND_SYSTEM.md", "TITLE_SYSTEM.md"];

fn write(path: &Path, contents: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, contents).expect("write");
}

#[test]
fn v1_system_prompts_land_where_user_prompt_path_reads_them() {
	let root = tempfile::tempdir().expect("scratch root");
	let home = root.path().join("home");
	let project = root.path().join("project");
	fs::create_dir_all(&project).expect("project");
	for (agent, tag) in
		[(home.join(".omp/agent"), "default"), (home.join(".omp/profiles/work/agent"), "work")]
	{
		for name in PROMPTS {
			write(&agent.join(name), &format!("{name} from {tag}\n"));
		}
	}
	// SAFETY: nextest runs each test in its own process, and nothing else in
	// this one reads the environment concurrently.
	unsafe {
		std::env::set_var("HOME", &home);
		std::env::set_var("OMP_CONFIG_DIR", root.path().join("o2"));
		std::env::set_var("OMP_DATA_DIR", root.path().join("data"));
		std::env::set_var("OMP_STATE_DIR", root.path().join("state"));
		std::env::set_var("OMP_CACHE_DIR", root.path().join("cache"));
		for name in ["OMP_PROFILE", "PI_PROFILE", "PI_CONFIG_DIR", "PI_CODING_AGENT_DIR"] {
			std::env::remove_var(name);
		}
	}
	let inputs =
		V1Inputs { project: Some(project.clone()), ..V1Inputs::from_process().expect("home") };
	let roots = V2Roots::from_process().expect("v2 roots");
	let pairs = plan(&V1Source::new(inputs), &roots, &ProfileSelection::All).expect("plan");
	let report = run(&pairs, ImportMode::Apply, CredentialAccess::Offline(&omp_con::Ctx::new()));
	assert!(
		report
			.entries()
			.filter(|entry| entry.step == ImportStep::SystemPrompts)
			.all(|entry| entry.outcome.kind() == omp_driver::v1_import::OutcomeKind::Imported),
		"{report:#?}"
	);

	for (profile, tag) in [(None, "default"), (Some("work"), "work")] {
		// SAFETY: as above.
		unsafe {
			match profile {
				Some(profile) => std::env::set_var("OMP_PROFILE", profile),
				None => std::env::remove_var("OMP_PROFILE"),
			}
		}
		for name in PROMPTS {
			let path = user_prompt_path(&home, name).expect("prompt path");
			assert!(path.starts_with(root.path().join("o2")), "{}", path.display());
			assert_eq!(fs::read_to_string(&path).expect("imported"), format!("{name} from {tag}\n"));
			assert_eq!(
				discover_prompt_file(&project, &home, name)
					.expect("discover")
					.as_deref(),
				Some(format!("{name} from {tag}\n").as_str())
			);
		}
	}
}
