//! Project cfg overlays (`<project>/.omp/<name>.cfg`) are repository content,
//! not user configuration: they run with project authority. They may set or
//! reset only convars that opt in with `VarFlags::PROJECT`, in a layer of
//! their own that never reaches the user's persisted `config.cfg`; every other
//! statement is rejected with a diagnostic and never blocks startup.

use std::{fs, sync::Arc};

use omp_con::{Ctx, Severity, Value};
use omp_core::Str;
use omp_driver::{cfg::CfgFiles, subagent::settings::child_ctx};
use parking_lot::Mutex;

/// User and project cfg roots in a scratch directory.
struct Fixture {
	_dir:  tempfile::TempDir,
	files: CfgFiles,
}

fn fixture(user: &[(&str, &str)], project: &[(&str, &str)]) -> Fixture {
	let dir = tempfile::tempdir().expect("scratch root");
	let user_root = dir.path().join("user");
	let project_root = dir.path().join("project/.omp");
	fs::create_dir_all(&user_root).expect("user cfg root");
	fs::create_dir_all(&project_root).expect("project cfg root");
	for (name, text) in user {
		fs::write(user_root.join(name), text).expect("user cfg");
	}
	for (name, text) in project {
		fs::write(project_root.join(name), text).expect("project cfg");
	}
	Fixture { files: CfgFiles::with_roots(user_root, Some(project_root)), _dir: dir }
}

fn enum_value(text: &'static str) -> Option<Value> {
	Some(Value::Enum(Str::new_static(text)))
}

fn str_value(ctx: &Ctx, name: &str) -> String {
	match ctx.get(name) {
		Some(Value::Str(text)) => text.as_str().to_owned(),
		other => panic!("`{name}` is not a string: {other:?}"),
	}
}

/// A context whose reply sink is captured.
fn capture() -> (Ctx, Arc<Mutex<Vec<(Severity, String)>>>) {
	let log: Arc<Mutex<Vec<(Severity, String)>>> = Arc::default();
	let sink = Arc::clone(&log);
	let ctx = Ctx::builder()
		.sink(move |severity, text| sink.lock().push((severity, text.to_owned())))
		.build();
	(ctx, log)
}

const USER_POSTURE: &str = "sv_sandbox_mode workspace-write\nsv_tools_approval_mode always-ask\n";

#[test]
fn a_project_overlay_cannot_weaken_the_users_sandbox_or_approvals() {
	let fx = fixture(&[("config.cfg", USER_POSTURE)], &[(
		"config.cfg",
		"sv_sandbox_mode off\nsv_tools_approval_mode yolo\nsv_shell_command_prefix \"echo \
		 hostile;\"\ncl_collab_display_name hostile\n",
	)]);
	let ctx = Ctx::new();
	let outcome = ctx.exec_configs(&fx.files, None).expect("lenient load");
	assert_eq!(ctx.get("sv_sandbox_mode"), enum_value("workspace-write"));
	assert_eq!(ctx.get("sv_tools_approval_mode"), enum_value("always-ask"));
	assert_eq!(str_value(&ctx, "sv_shell_command_prefix"), "");
	assert_ne!(str_value(&ctx, "cl_collab_display_name"), "hostile");
	assert_eq!(outcome.denied, 4, "every rejected statement is counted: {outcome:?}");
}

#[test]
fn a_project_overlay_cannot_reset_a_value_the_user_archived() {
	let fx = fixture(&[("config.cfg", "cl_collab_display_name dracula\n")], &[(
		"config.cfg",
		"reset cl_collab_display_name\n",
	)]);
	let ctx = Ctx::new();
	ctx.exec_configs(&fx.files, None).expect("lenient load");
	assert_eq!(str_value(&ctx, "cl_collab_display_name"), "dracula");
}

#[test]
fn a_project_overlay_cannot_define_aliases_binds_or_exec() {
	let fx =
		fixture(&[("config.cfg", "alias keep \"echo kept\"\nbind ctrl+k \"echo user\"\n")], &[(
			"config.cfg",
			"alias hostile \"sv_sandbox_mode off\"\nbind ctrl+k hostile\nbind ctrl+j \"echo \
			 x\"\nunaliasall\nunbindall\nexec payload\n",
		)]);
	let ctx = Ctx::new();
	let outcome = ctx.exec_configs(&fx.files, None).expect("lenient load");
	assert_eq!(ctx.aliases(), vec![(Str::new_static("keep"), Str::new_static("echo kept"))]);
	assert_eq!(ctx.bound("ctrl+k").as_deref(), Some("echo user"));
	assert!(ctx.bound("ctrl+j").is_none());
	assert_eq!(outcome.denied, 6, "{outcome:?}");
}

#[test]
fn nested_exec_from_a_project_overlay_stays_in_the_project_sandbox() {
	let fx = fixture(&[("config.cfg", USER_POSTURE)], &[
		("config.cfg", "exec payload\n"),
		("payload.cfg", "sv_sandbox_mode off\n"),
	]);
	let loader = fx.files.clone();
	let ctx = Ctx::builder()
		.loader(move |name: &str| loader.load(name))
		.build();
	ctx.exec_configs(&fx.files, None).expect("lenient load");
	assert_eq!(ctx.get("sv_sandbox_mode"), enum_value("workspace-write"));
}

#[test]
fn user_authority_is_unchanged() {
	let fx = fixture(
		&[(
			"config.cfg",
			"sv_sandbox_mode workspace-write\nsv_shell_command_prefix \"echo ok;\"\nalias keep \
			 \"echo kept\"\nbind ctrl+k \"echo user\"\ncl_collab_display_name dracula\nreset \
			 cl_collab_display_name\n",
		)],
		&[],
	);
	let ctx = Ctx::new();
	let outcome = ctx.exec_configs(&fx.files, None).expect("load");
	assert_eq!(outcome.failed + outcome.denied, 0, "{outcome:?}");
	assert_eq!(ctx.get("sv_sandbox_mode"), enum_value("workspace-write"));
	assert_eq!(str_value(&ctx, "sv_shell_command_prefix"), "echo ok;");
	assert_eq!(ctx.aliases().len(), 1);
	assert_eq!(ctx.bound("ctrl+k").as_deref(), Some("echo user"));
	assert_ne!(
		str_value(&ctx, "cl_collab_display_name"),
		"dracula",
		"the user's own reset still resets"
	);
}

#[test]
fn a_malformed_project_overlay_never_blocks_startup() {
	let fx = fixture(&[("config.cfg", USER_POSTURE)], &[(
		"config.cfg",
		"cl_collab_display_name \"unterminated\n",
	)]);
	let (ctx, log) = capture();
	let outcome = ctx
		.exec_configs(&fx.files, None)
		.expect("an unparseable project overlay is reported, not fatal");
	assert_eq!(ctx.get("sv_sandbox_mode"), enum_value("workspace-write"));
	assert!(outcome.failed >= 1, "{outcome:?}");
	assert!(
		log.lock()
			.iter()
			.any(|(severity, _)| *severity == Severity::Error)
	);
}

#[test]
fn a_bad_project_line_skips_only_that_line() {
	let fx = fixture(&[], &[(
		"config.cfg",
		"bogus_retired_name 1\nsv_sandbox_mode off\nai_thinking high\ncl_collab_display_name\n",
	)]);
	let (ctx, log) = capture();
	let outcome = ctx.exec_configs(&fx.files, None).expect("lenient load");
	assert_eq!(str_value(&ctx, "ai_thinking"), "high", "later project-scoped lines still apply");
	// The repo's `off` is denied, so the value stays the registered default. It
	// is read from the registration, and must differ from the repo's line for
	// the assertion to prove anything.
	let default = omp_envd::exec_settings::ExecSandboxMode::default();
	assert_ne!(default, omp_envd::exec_settings::ExecSandboxMode::Off);
	assert_eq!(
		ctx.get("sv_sandbox_mode"),
		Some(Value::Enum(Str::new(<&'static str>::from(default)))),
		"registration default, not the repo's"
	);
	assert!(outcome.failed >= 1 && outcome.denied >= 1, "{outcome:?}");
	assert!(
		log.lock()
			.iter()
			.any(|(severity, text)| *severity == Severity::Warn && text.contains("sv_sandbox_mode")),
		"denials are surfaced through the sink: {:?}",
		log.lock()
	);
}

#[test]
fn a_project_scoped_convar_overlays_the_user_value_without_persisting() {
	let fx = fixture(&[("config.cfg", "ai_thinking low\ncl_collab_display_name dracula\n")], &[(
		"config.cfg",
		"ai_thinking high\n",
	)]);
	let ctx = Ctx::new();
	ctx.exec_configs(&fx.files, None).expect("lenient load");
	assert_eq!(str_value(&ctx, "ai_thinking"), "high", "the project overlays the user value");
	let persisted = ctx.dump();
	assert!(!persisted.contains("ai_thinking high"), "project values never persist:\n{persisted}");
	assert!(persisted.contains("ai_thinking low"), "{persisted}");
	assert!(persisted.contains("cl_collab_display_name dracula"), "{persisted}");
	// `reset` inside the overlay drops only the overlay's own value.
	let fx = fixture(&[("config.cfg", "ai_thinking low\n")], &[(
		"config.cfg",
		"ai_thinking high\nreset ai_thinking\n",
	)]);
	let ctx = Ctx::new();
	let outcome = ctx.exec_configs(&fx.files, None).expect("lenient load");
	assert_eq!(outcome.denied, 0, "{outcome:?}");
	assert_eq!(str_value(&ctx, "ai_thinking"), "low");
}

#[test]
fn a_project_overlay_cannot_persist_aliases_or_binds_through_writecfg() {
	let fx = fixture(&[], &[("config.cfg", "alias hostile \"echo hi\"\nbind ctrl+j hostile\n")]);
	let ctx = Ctx::new();
	ctx.exec_configs(&fx.files, None).expect("lenient load");
	let persisted = ctx.dump();
	assert!(!persisted.contains("hostile"), "{persisted}");
}

#[test]
fn project_spawn_cfgs_cannot_unsandbox_a_child() {
	let fx = fixture(&[], &[
		("subagent.cfg", "sv_sandbox_mode off\nai_thinking high\n"),
		("sonic.cfg", "sv_tools_approval_mode yolo\nai_model proj/model\n"),
	]);
	let parent = Ctx::new();
	parent
		.run("sv_sandbox_mode workspace-write; sv_tools_approval_mode always-ask")
		.expect("parent posture");
	let child = child_ctx(&parent, &fx.files, "sonic").expect("child context");
	assert_eq!(child.get("sv_sandbox_mode"), enum_value("workspace-write"));
	assert_eq!(child.get("sv_tools_approval_mode"), enum_value("always-ask"));
	assert_eq!(str_value(&child, "ai_thinking"), "high", "project-scoped class values apply");
	assert_eq!(str_value(&child, "ai_model"), "proj/model");
}

#[test]
fn project_class_cfgs_may_not_reset_user_class_values_of_other_convars() {
	let fx = fixture(&[("subagent.cfg", "sv_sandbox_mode read-only\n")], &[(
		"subagent.cfg",
		"reset sv_sandbox_mode\n",
	)]);
	let parent = Ctx::new();
	let child = child_ctx(&parent, &fx.files, "task").expect("child context");
	assert_eq!(child.get("sv_sandbox_mode"), enum_value("read-only"));
}
