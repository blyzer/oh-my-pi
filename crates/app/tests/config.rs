//! Command-stream configuration persistence and v1 settings import contracts.

use std::{fmt::Write as _, fs};

use omp_app::{cli::ConfigScope, config_cmd::set_persisted};

/// Imports `yaml` as a project's v1 `.omp/config.yml` through the full
/// process registry and returns the `config.cfg` it writes.
fn import_v1_project_settings(yaml: &str) -> String {
	let root = tempfile::tempdir().expect("scratch");
	let project = root.path().join("repo");
	fs::create_dir_all(project.join(".omp")).expect(".omp");
	fs::write(project.join(".omp/config.yml"), yaml).expect("v1 config.yml");
	let roots = omp_driver::v1_import::V2Roots {
		config_dir:     root.path().join("config"),
		data_dir:       root.path().join("data"),
		state_dir:      root.path().join("state"),
		cache_dir:      root.path().join("cache"),
		active_profile: None,
	};
	let entries = omp_driver::v1_import::import_project_settings(
		&project,
		&roots,
		omp_driver::v1_import::ImportMode::Apply,
	);
	assert!(!entries.is_empty(), "the import reports");
	fs::read_to_string(project.join(".omp/config.cfg")).expect("imported config.cfg")
}

/// Folds `yaml` as a v1 settings document through every convar in the
/// process registry, the way the v1 settings step does before it drops values
/// that equal the default, and returns one `name value` line per convar.
fn v1_folds(yaml: &str) -> String {
	let scratch = tempfile::tempdir().expect("scratch");
	let path = scratch.path().join("config.yml");
	fs::write(&path, yaml).expect("v1 config.yml");
	let document = omp_driver::legacy_settings::read_yaml_document(&path).expect("v1 document");
	let ctx = omp_con::Ctx::new();
	let mut lines = String::new();
	for var in ctx.vars() {
		let Some(fold) = omp_driver::legacy_settings::fold_var(&document, &var) else {
			continue;
		};
		let value = fold
			.outcome
			.unwrap_or_else(|(path, error)| panic!("{path} does not convert: {error}"))
			.unwrap_or_else(|| panic!("{} folds to nothing", var.name));
		ctx.set(var.name, value, omp_con::Origin::Archive)
			.unwrap_or_else(|error| panic!("{} rejects the value: {error}", var.name));
		let effective = ctx.value(var.name).expect("registered convar");
		writeln!(lines, "{} {effective}", var.name).expect("write to a String");
	}
	lines
}

#[test]
fn v1_settings_reach_convars_declared_across_the_process() {
	let registry = omp_con::Ctx::new();
	let mut saw_retry_enabled = false;
	let mut saw_steering_mode = false;
	for var in registry.vars() {
		for path in var.meta_all("legacy.path") {
			assert!(!path.is_empty(), "legacy path for {} is empty", var.name);
			saw_retry_enabled |= path == "retry.enabled" && var.name == "ai_retry_enabled";
			saw_steering_mode |= path == "steeringMode" && var.name == "ai_steering_mode";
		}
	}
	assert!(saw_retry_enabled, "retry.enabled metadata is missing");
	assert!(saw_steering_mode, "steeringMode metadata is missing");

	let script = import_v1_project_settings(concat!(
		"steeringMode: all
",
		"hideThinkingBlock: true
",
		"display:
  hideToolActivity: true
",
		"retry:
  enabled: false
",
		"stt:
  enabled: true
  modelName: turbo
",
	));
	assert!(script.contains("ai_retry_enabled false"), "{script}");
	assert!(script.contains("ai_steering_mode all"), "{script}");
	assert!(script.contains("cl_showthinking false"), "{script}");
	assert!(script.contains("cl_showtools false"), "{script}");
	assert!(script.contains("cl_voice_stt_enabled true"), "{script}");
	assert!(script.contains("cl_stt_model turbo"), "{script}");
}

#[test]
fn v1_settings_preserve_output_limit_kibibyte_values() {
	let script = v1_folds(concat!(
		"tools:
",
		"  artifactSpillThreshold: 50
",
		"  artifactTailBytes: 2.5
",
		"  artifactHeadBytes: 20
",
		"  outputMaxColumns: 768
",
		"  artifactTailLines: 500
",
	));
	assert!(script.contains("sv_tools_output_spill_bytes 51200"), "{script}");
	assert!(script.contains("sv_tools_artifact_tail_bytes 2560"), "{script}");
	assert!(script.contains("sv_tools_artifact_head_bytes 20480"), "{script}");
	assert!(script.contains("sv_tools_output_max_columns 768"), "{script}");
	assert!(script.contains("sv_tools_artifact_tail_lines 500"), "{script}");
}

#[test]
fn v1_settings_convert_legacy_value_encodings() {
	let script = v1_folds(concat!(
		"doubleEscapeAction: rewind
",
		"completion:
  notify: \"off\"
",
		"error:
  notify: \"on\"
",
		"ask:
  notify: \"off\"
",
		"compaction:
  thresholdPercent: 80
  thresholdTokens: default
",
		"task:
  maxRuntimeMs: 0
  isolation:
    enabled: true
",
		"irc:
  timeoutMs: 30000
",
		"tools:
  maxTimeout: 60
",
		"edit:
  mode: hashline
",
		"providers:
",
		"  tinyModel: online
",
		"  memoryModel: online
",
		"  autoThinkingModel: online
",
		"  unexpectedStopModel: online
",
		"  fireworksTier: standard
",
		"share:
  store: blob
",
	));
	assert!(script.contains("cl_double_escape branch"), "{script}");
	assert!(script.contains("cl_notify_completion false"), "{script}");
	assert!(script.contains("cl_notify_error true"), "{script}");
	assert!(script.contains("cl_notify_ask false"), "{script}");
	assert!(script.contains("ai_compact_threshold 0.8"), "{script}");
	assert!(script.contains("ai_compaction_threshold_tokens -1"), "{script}");
	assert!(script.contains("sv_task_isolation_mode auto"), "{script}");
	assert!(script.contains("sv_task_max_runtime never"), "{script}");
	assert!(script.contains("sv_irc_timeout 30000ms"), "{script}");
	assert!(script.contains("sv_tools_max_timeout 60s"), "{script}");
	assert!(script.contains("sv_tools_edit_dialect hl.1"), "{script}");
	assert!(script.contains("ai_tiny_selector @tiny"), "{script}");
	assert!(script.contains("ai_memory_selector @tiny"), "{script}");
	assert!(script.contains("ai_auto_thinking_selector @tiny"), "{script}");
	assert!(script.contains("ai_unexpected_stop_selector @tiny"), "{script}");
	assert!(script.contains("ai_tier_fireworks none"), "{script}");
	assert!(script.contains("sv_share_store http"), "{script}");
}

#[test]
fn profile_selects_its_own_config_cfg() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: see above.
	unsafe {
		std::env::set_var("OMP_CONFIG_DIR", config.path());
		std::env::set_var("OMP_PROFILE", "work");
	}
	let project = tempfile::tempdir().expect("project directory");
	set_persisted(project.path(), ConfigScope::Global, "cl_showthinking", "false")
		.expect("set archived convar");
	set_persisted(project.path(), ConfigScope::Global, "sv_worktree_base", "/tmp/omp-worktrees")
		.expect("set worktree root");
	assert!(config.path().join("profiles/work/config.cfg").is_file());
	assert!(!config.path().join("config.cfg").exists());
	let ctx = omp_app::process_ctx(project.path()).expect("reload context");
	assert!(!ctx.get_typed::<bool>("cl_showthinking").expect("convar"));
	assert_eq!(
		omp_driver::settings::current()
			.expect("driver settings")
			.worktree
			.base
			.as_deref(),
		Some(std::path::Path::new("/tmp/omp-worktrees"))
	);
}

#[test]
fn exec_and_writecfg_use_the_installed_cfg_files() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: see above.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	fs::create_dir_all(project.path().join(".omp")).expect(".omp");
	fs::write(config.path().join("focus.cfg"), "cl_showthinking false\n").expect("user profile");
	fs::write(project.path().join(".omp/focus.cfg"), "ai_fastmode true\n").expect("project overlay");
	let ctx = omp_app::process_ctx(project.path()).expect("context");
	ctx.run("exec focus")
		.expect("exec resolves through the installed loader");
	assert!(!ctx.get_typed::<bool>("cl_showthinking").expect("convar"));
	assert!(ctx.get_typed::<bool>("ai_fastmode").expect("convar"));
	ctx.run("writecfg")
		.expect("writecfg resolves through the installed saver");
	let script = fs::read_to_string(config.path().join("config.cfg")).expect("config.cfg");
	assert!(script.contains("cl_showthinking false"));
	assert!(script.contains("ai_fastmode true"));
}

#[test]
fn chat_services_address_user_configuration_under_the_config_root() {
	let config = tempfile::tempdir().expect("config directory");
	let data = tempfile::tempdir().expect("data directory");
	// SAFETY: see above.
	unsafe {
		std::env::set_var("OMP_CONFIG_DIR", config.path());
		std::env::set_var("OMP_DATA_DIR", data.path());
	}
	let project = tempfile::tempdir().expect("project directory");

	// `/mcp` in chat and `omp config mcp` on the CLI must address one file.
	let chat = omp_app::chat_services::mcp_config_paths(project.path()).expect("mcp paths");
	let cli = omp_envd::mcp::McpConfigPaths::new(
		&omp_core::dirs::user_config_root().expect("config root"),
		project.path(),
	);
	assert_eq!(chat, cli);
	assert_eq!(chat.user, config.path().join("mcp.json"));
	assert_eq!(chat.project, project.path().join(".omp/mcp.json"));
	assert!(!chat.user.starts_with(data.path()), "user mcp.json must not live in the data dir");

	// `/share` redacts with the user `secrets.yml` under the same root.
	let [user, project_secrets] =
		omp_app::chat_services::secrets_files(project.path()).expect("secrets files");
	assert_eq!(user, config.path().join("secrets.yml"));
	assert_eq!(project_secrets, project.path().join(".omp/secrets.yml"));
}

#[test]
fn config_set_persists_and_get_reads_back() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: see above.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	set_persisted(project.path(), ConfigScope::Global, "cl_showthinking", "false")
		.expect("set archived convar");

	let script = fs::read_to_string(config.path().join("config.cfg")).expect("config.cfg");
	assert!(script.contains("cl_showthinking false"));
	let ctx = omp_app::process_ctx(project.path()).expect("reload context");
	assert_eq!(ctx.get_typed::<bool>("cl_showthinking").expect("convar"), false);
}

#[test]
fn explicit_default_survives_schema_default_changes() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: nextest runs each test in its own process.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	set_persisted(project.path(), ConfigScope::Global, "cl_showthinking", "true")
		.expect("persist explicit current default");
	let script = fs::read_to_string(config.path().join("config.cfg")).expect("config.cfg");
	assert!(
		script.contains("cl_showthinking true"),
		"an explicit value must not disappear merely because it equals this build's default"
	);
}

#[test]
fn concurrent_config_updates_preserve_distinct_assignments() {
	let config = tempfile::tempdir().expect("config directory");
	// SAFETY: nextest runs each test in its own process.
	unsafe { std::env::set_var("OMP_CONFIG_DIR", config.path()) };
	let project = tempfile::tempdir().expect("project directory");
	let project_path = project.path().to_path_buf();
	let first = std::thread::spawn({
		let project = project_path.clone();
		move || {
			set_persisted(&project, ConfigScope::Global, "cl_showthinking", "false")
				.expect("first update")
		}
	});
	let second = std::thread::spawn(move || {
		set_persisted(
			&project_path,
			ConfigScope::Global,
			"sv_worktree_base",
			"/tmp/concurrent-worktrees",
		)
		.expect("second update")
	});
	first.join().expect("first updater");
	second.join().expect("second updater");

	let script = fs::read_to_string(config.path().join("config.cfg")).expect("config.cfg");
	assert!(script.contains("cl_showthinking false"), "{script}");
	assert!(script.contains("sv_worktree_base /tmp/concurrent-worktrees"), "{script}");
}
