//! Project cfg overlays run with project authority: only `set` and `reset` of
//! convars flagged `PROJECT`, into a layer that is never persisted.

use std::sync::Arc;

use omp_con::{
	CfgLoader, ConError, ConResult, Ctx, DynamicVarSpec, Origin, Severity, Source, TypeSpec, Value,
	VarFlags,
};
use omp_core::Str;
use parking_lot::Mutex;

omp_con::var! {
	/// Opted in: a project may set it.
	pub static SCOPED = test_project_scoped: i32 {
		default: 1,
		flags: archive | project,
	};
	/// Not opted in: only the user may set it.
	pub static USER_ONLY = test_project_user_only: i32 {
		default: 1,
		flags: archive,
	};
	/// Opted in but script-read-only: the gates still apply to a project.
	pub static LOCKED = test_project_locked: i32 {
		default: 1,
		flags: archive | project | readonly,
	};
}

fn project(ctx: &Ctx, script: &str) -> omp_con::ConResult<Vec<omp_con::Output>> {
	ctx.exec(script, Source::Project(Str::new_static("config.cfg")))
}

fn captured() -> (Ctx, Arc<Mutex<Vec<(Severity, String)>>>) {
	let log: Arc<Mutex<Vec<(Severity, String)>>> = Arc::default();
	let sink = Arc::clone(&log);
	let ctx = Ctx::builder()
		.sink(move |severity, text| sink.lock().push((severity, text.to_owned())))
		.build();
	(ctx, log)
}

/// User text and project overlay per cfg name.
struct Files {
	user:    Vec<(&'static str, &'static str)>,
	project: Vec<(&'static str, &'static str)>,
}

impl CfgLoader for Files {
	fn load(&self, name: &str) -> ConResult<Option<Str>> {
		Ok(self
			.user
			.iter()
			.find(|(file, _)| *file == name)
			.map(|(_, text)| Str::new_static(text)))
	}

	fn load_project(&self, name: &str) -> ConResult<Option<Str>> {
		Ok(self
			.project
			.iter()
			.find(|(file, _)| *file == name)
			.map(|(_, text)| Str::new_static(text)))
	}
}

#[test]
fn a_project_may_set_only_convars_that_opt_in() {
	let ctx = Ctx::new();
	project(&ctx, "test_project_scoped 7").unwrap();
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(7)));
	let err = project(&ctx, "test_project_user_only 7").unwrap_err();
	assert!(matches!(err, ConError::ProjectVarDenied { .. }), "{err:?}");
	assert_eq!(ctx.get("test_project_user_only"), Some(Value::Int(1)));
}

#[test]
fn a_project_value_is_not_a_user_choice() {
	let ctx = Ctx::new();
	project(&ctx, "test_project_scoped 7").unwrap();
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(7)));
	assert!(!ctx.is_user_set("test_project_scoped"));
	ctx.set("test_project_scoped", Value::Int(3), Origin::Archive)
		.unwrap();
	assert!(ctx.is_user_set("test_project_scoped"));
}

#[test]
fn every_statement_other_than_set_and_reset_of_a_scoped_convar_is_denied() {
	let ctx = Ctx::new();
	ctx.run("alias keep \"test_project_user_only 5\"").unwrap();
	for statement in [
		"alias x \"echo hi\"",
		"unalias keep",
		"unaliasall",
		"bind ctrl+k \"echo hi\"",
		"unbind ctrl+k",
		"unbindall",
		"exec config",
		"writecfg",
		"echo hi",
		"toggle test_project_scoped",
		"keep",
		"+attack",
		"help test_project_scoped",
		"reset test_project_user_only",
	] {
		let err = project(&ctx, statement).unwrap_err();
		assert!(err.is_project_denial(), "`{statement}` should be denied, got {err:?}");
	}
	assert_eq!(ctx.aliases().len(), 1, "the user's alias survives");
}

#[test]
fn unknown_names_stay_ordinary_failures_not_denials() {
	let ctx = Ctx::new();
	let err = project(&ctx, "test_project_retired_name 1").unwrap_err();
	assert!(matches!(err, ConError::Unknown { .. }), "{err:?}");
}

#[test]
fn script_gates_still_apply_to_a_project() {
	let ctx = Ctx::new();
	let err = project(&ctx, "test_project_locked 9").unwrap_err();
	assert!(matches!(err, ConError::ReadOnly { .. }), "{err:?}");
}

#[test]
fn project_values_overlay_the_archive_without_replacing_it() {
	let ctx = Ctx::new();
	ctx.exec("test_project_scoped 3", Source::Config(Str::new_static("config.cfg")))
		.unwrap();
	project(&ctx, "test_project_scoped 9").unwrap();
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(9)));
	// `reset` in the overlay removes only the overlay's own value.
	project(&ctx, "reset test_project_scoped").unwrap();
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(3)));
	// The user's console still outranks the project.
	project(&ctx, "test_project_scoped 9").unwrap();
	ctx.run("test_project_scoped 4").unwrap();
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(4)));
}

#[test]
fn project_values_are_never_persisted_or_dumped_as_the_users() {
	let ctx = Ctx::new();
	ctx.exec("test_project_scoped 3", Source::Config(Str::new_static("config.cfg")))
		.unwrap();
	project(&ctx, "test_project_scoped 9").unwrap();
	let persisted = ctx.dump();
	assert!(persisted.contains("test_project_scoped 3"), "{persisted}");
	assert!(!persisted.contains("test_project_scoped 9"), "{persisted}");
	let overlay = ctx.dump_project();
	assert!(overlay.contains("test_project_scoped 9"), "{overlay}");
	assert!(!overlay.contains("alias") && !overlay.contains("bind"), "{overlay}");
	// The overlay replays into a fresh context's project layer.
	let replay = Ctx::new();
	project(&replay, overlay.as_str()).unwrap();
	assert_eq!(replay.get("test_project_scoped"), Some(Value::Int(9)));
}

#[test]
fn children_inherit_the_project_value_in_their_seed() {
	let ctx = Ctx::new();
	project(&ctx, "test_project_scoped 9").unwrap();
	assert_eq!(ctx.seed_child().get("test_project_scoped"), Some(&Value::Int(9)));
	assert_eq!(ctx.scope_seed().get("test_project_scoped"), Some(&Value::Int(9)));
}

#[test]
fn exec_configs_runs_the_overlay_after_the_user_text_and_counts_denials() {
	let (ctx, log) = captured();
	let files = Files {
		user:    vec![(
			"config.cfg",
			"test_project_scoped 3\ntest_project_user_only 5\nalias keep \"echo kept\"\n",
		)],
		project: vec![(
			"config.cfg",
			"test_project_scoped 9\ntest_project_user_only 99\nunaliasall\nbogus_name 1\n",
		)],
	};
	let outcome = ctx.exec_configs(&files, None).unwrap();
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(9)));
	assert_eq!(ctx.get("test_project_user_only"), Some(Value::Int(5)));
	assert_eq!(ctx.aliases().len(), 1);
	assert_eq!((outcome.ran, outcome.failed, outcome.denied), (4, 1, 2), "{outcome:?}");
	let log = log.lock();
	assert!(
		log.iter()
			.any(|(severity, text)| *severity == Severity::Warn
				&& text.starts_with("project cfg line 2:")
				&& text.contains("test_project_user_only")),
		"{log:?}"
	);
}

#[test]
fn spawn_overlays_commit_to_the_class_layer_under_project_authority() {
	let ctx = Ctx::new();
	ctx.set("test_project_scoped", Value::Int(5), Origin::Inherited)
		.unwrap();
	let files = Files {
		user:    vec![("subagent.cfg", "test_project_user_only 4\n")],
		project: vec![
			("subagent.cfg", "test_project_scoped 6\ntest_project_user_only 8\n"),
			("scout.cfg", "reset test_project_scoped\nbind ctrl+k \"echo hi\"\n"),
		],
	};
	let outcome = ctx.exec_spawn_configs(&files, "scout").unwrap();
	assert_eq!(ctx.get("test_project_user_only"), Some(Value::Int(4)));
	// The agent overlay reset the class value the subagent overlay set.
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(5)));
	assert_eq!(outcome.denied, 2, "{outcome:?}");
	assert!(ctx.bound("ctrl+k").is_none());
}

/// A loader whose project overlay cannot be read.
struct Unreadable;

impl CfgLoader for Unreadable {
	fn load(&self, name: &str) -> ConResult<Option<Str>> {
		Ok((name == "config.cfg").then(|| Str::new_static("test_project_user_only 5")))
	}

	fn load_project(&self, _name: &str) -> ConResult<Option<Str>> {
		Err(ConError::MissingCfg { name: Str::new_static("config.cfg") })
	}
}

#[test]
fn an_unreadable_project_overlay_never_blocks_the_load() {
	let (ctx, log) = captured();
	let outcome = ctx.exec_configs(&Unreadable, None).unwrap();
	assert_eq!(ctx.get("test_project_user_only"), Some(Value::Int(5)));
	assert_eq!((outcome.ran, outcome.failed), (1, 1));
	assert!(log.lock().iter().any(|(severity, text)| {
		*severity == Severity::Error && text.starts_with("project cfg `config.cfg` skipped")
	}));
}

#[test]
fn a_malformed_project_overlay_is_skipped_whole() {
	let (ctx, _log) = captured();
	let files =
		Files { user: vec![], project: vec![("config.cfg", "test_project_scoped \"open\n")] };
	let outcome = ctx.exec_configs(&files, None).unwrap();
	assert_eq!((outcome.ran, outcome.failed), (0, 1));
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(1)));
}

#[test]
fn exec_resolves_the_project_half_with_project_authority() {
	let files = Files {
		user:    vec![("focus", "test_project_user_only 6\n")],
		project: vec![("focus", "test_project_scoped 8\ntest_project_user_only 66\n")],
	};
	let ctx = Ctx::builder().loader(files).build();
	ctx.run("exec focus").unwrap();
	assert_eq!(ctx.get("test_project_user_only"), Some(Value::Int(6)));
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(8)));
	// A cfg with only a project half still runs; one with neither is missing.
	let only_project = Files { user: vec![], project: vec![("solo", "test_project_scoped 2\n")] };
	let ctx = Ctx::builder().loader(only_project).build();
	ctx.run("exec solo").unwrap();
	assert_eq!(ctx.get("test_project_scoped"), Some(Value::Int(2)));
	assert!(matches!(ctx.run("exec nothing").unwrap_err(), ConError::MissingCfg { .. }));
}

#[test]
fn dynamic_variables_opt_in_through_their_spec_flags() {
	let ctx = Ctx::new();
	for (name, flags) in [
		("product::project_scoped", VarFlags::ARCHIVE.with(VarFlags::PROJECT)),
		("product::user_only", VarFlags::ARCHIVE),
	] {
		ctx.register_dynamic_var(DynamicVarSpec {
			name: name.into(),
			desc: "dynamic".into(),
			ty: TypeSpec::BOOL,
			flags,
			default: Value::Bool(false),
			meta: Arc::from([]),
		})
		.unwrap();
	}
	project(&ctx, "product::project_scoped true").unwrap();
	assert_eq!(ctx.get("product::project_scoped"), Some(Value::Bool(true)));
	let err = project(&ctx, "product::user_only true").unwrap_err();
	assert!(matches!(err, ConError::ProjectVarDenied { .. }), "{err:?}");
}

#[test]
fn user_authority_runs_everything_a_project_cannot() {
	let ctx = Ctx::new();
	let outcome = ctx.exec_lenient(
		"alias keep \"echo kept\"; bind ctrl+k \"echo hi\"; test_project_user_only 5; reset \
		 test_project_user_only",
	);
	assert_eq!((outcome.failed, outcome.denied), (0, 0), "{outcome:?}");
}
