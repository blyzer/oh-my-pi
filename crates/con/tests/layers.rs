//! Engagement-layer precedence contracts.

use omp_con::{ConError, Ctx, DumpOptions, Origin, Source, Value};
use omp_core::Str;

omp_con::var! {
	/// Layer precedence target.
	pub static LAYERED = test_layered: i32 {
		default: 1,
		flags: archive | session,
	};
}

#[test]
fn test_engagement_bind_shadows_and_pops_by_derivation() {
	let ctx = Ctx::new();
	ctx.set("test_layered", Value::Int(2), Origin::Archive)
		.unwrap();
	let layer =
		ctx.push_layer(Str::new_static("plan"), &[(Str::new_static("test_layered"), Value::Int(9))]);
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(9)));
	ctx.pop_layer(layer);
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(2)));
}

#[test]
fn test_shadowed_user_write_commits_and_surfaces_on_exit() {
	let ctx = Ctx::new();
	let layer =
		ctx.push_layer(Str::new_static("goal"), &[(Str::new_static("test_layered"), Value::Int(8))]);
	let report = ctx
		.set("test_layered", Value::Int(4), Origin::Script(Str::new_static("console")))
		.unwrap();
	assert_eq!(report.committed_to, Origin::Session);
	assert_eq!(report.shadowed_by, Some((layer, Str::new_static("goal"))));
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(8)));
	ctx.pop_layer(layer);
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(4)));
}

#[test]
fn test_layers_stack_innermost_last_and_pop_independently() {
	let ctx = Ctx::new();
	let outer =
		ctx.push_layer(Str::new_static("outer"), &[(Str::new_static("test_layered"), Value::Int(5))]);
	let inner =
		ctx.push_layer(Str::new_static("inner"), &[(Str::new_static("test_layered"), Value::Int(6))]);
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(6)));
	ctx.pop_layer(outer);
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(6)));
	ctx.pop_layer(inner);
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(1)));
}

#[test]
fn test_unshadowed_write_reports_nothing() {
	let ctx = Ctx::new();
	let report = ctx
		.set("test_layered", Value::Int(3), Origin::Session)
		.unwrap();
	assert_eq!(report.committed_to, Origin::Session);
	assert_eq!(report.shadowed_by, None);
	assert_eq!(ctx.get("test_layered"), Some(Value::Int(3)));
	assert_eq!(ctx.seed_child().get("test_layered"), Some(&Value::Int(3)));
}

omp_con::var! {
	/// Archived, non-session scope target.
	pub static SCOPED = test_scoped: i32 {
		default: 10,
		flags: archive,
	};
}

fn value(ctx: &Ctx) -> Value {
	ctx.get("test_layered").expect("registered")
}

/// The main session inherits from the user cfg: a console `reset` removes the
/// session override instead of writing the archived value back, so a later
/// user-cfg change still flows through.
#[test]
fn console_reset_drops_the_session_override_so_later_archive_changes_flow() {
	let ctx = Ctx::new();
	ctx.set("test_layered", Value::Int(2), Origin::Archive)
		.unwrap();
	ctx.run("test_layered 4").unwrap();
	assert_eq!(value(&ctx), Value::Int(4));
	ctx.run("reset test_layered").unwrap();
	assert_eq!(value(&ctx), Value::Int(2), "back to the user cfg's value");
	ctx.set("test_layered", Value::Int(7), Origin::Archive)
		.unwrap();
	assert_eq!(value(&ctx), Value::Int(7), "no explicit override pins the old value");
	assert!(!ctx.session_writes().any(|(name, _)| name == "test_layered"));
}

/// A child: parent seed (inherited) ← class cfg ← its own console writes.
/// Each `reset` peels exactly the layer its statement commits to.
#[test]
fn child_reset_peels_console_then_class_down_to_the_inherited_value() {
	let child = Ctx::new();
	child
		.set("test_layered", Value::Int(5), Origin::Inherited)
		.unwrap();
	child
		.exec("test_layered 6", Source::Agent(Str::new_static("sonic")))
		.unwrap();
	assert_eq!(value(&child), Value::Int(6), "the class cfg outranks the parent's value");
	child.run("test_layered 8").unwrap();
	assert_eq!(value(&child), Value::Int(8));
	child.run("reset test_layered").unwrap();
	assert_eq!(value(&child), Value::Int(6), "console reset returns to the class value");
	child
		.exec("reset test_layered", Source::Agent(Str::new_static("sonic")))
		.unwrap();
	assert_eq!(value(&child), Value::Int(5), "a class cfg reset returns to the parent's");
	// Nothing further to peel in the session layer: a repeat is a no-op.
	child.run("reset test_layered").unwrap();
	assert_eq!(value(&child), Value::Int(5));
}

/// Inside `config.cfg` the scope is the archive itself, whose parent is the
/// registration default.
#[test]
fn config_cfg_reset_clears_the_archived_value() {
	let ctx = Ctx::new();
	ctx.exec("test_layered 3", Source::Config(Str::new_static("config.cfg")))
		.unwrap();
	assert_eq!(value(&ctx), Value::Int(3));
	ctx.exec("reset test_layered", Source::Config(Str::new_static("config.cfg")))
		.unwrap();
	assert_eq!(value(&ctx), Value::Int(1));
}

/// A removed `SESSION` override is published as `None`, so the journal drops
/// it rather than recording the inherited value as a new write.
#[test]
fn reset_publishes_a_removal_not_the_inherited_value() {
	let ctx = Ctx::new();
	ctx.set("test_layered", Value::Int(2), Origin::Inherited)
		.unwrap();
	let writes = ctx.subscribe_session_writes();
	ctx.run("test_layered 9").unwrap();
	assert_eq!(writes.try_recv().unwrap(), (Str::new("test_layered"), Some(Value::Int(9))));
	ctx.run("reset test_layered").unwrap();
	assert_eq!(writes.try_recv().unwrap(), (Str::new("test_layered"), None));
	assert_eq!(value(&ctx), Value::Int(2));
	ctx.run("reset test_layered").unwrap();
	assert!(writes.try_recv().is_err(), "nothing removed, nothing published");
	// Unknown and non-variable names are refused.
	assert!(matches!(ctx.run("reset test_nonexistent"), Err(ConError::Unknown { .. })));
	assert!(matches!(ctx.run("reset echo"), Err(ConError::NotAVar { .. })));
}

/// Persistence records the scope's own values only: an inherited or class
/// value is never written, and a reset override drops out of the script
/// instead of being saved as an explicit default.
#[test]
fn persisted_cfg_drops_a_reset_line_and_never_records_inherited_values() {
	let options = DumpOptions {
		include_archived_defaults: true,
		include_session_defaults: true,
		..DumpOptions::default()
	};
	let ctx = Ctx::new();
	ctx.run("test_scoped 10").unwrap();
	assert!(
		ctx.dump_with_options(options)
			.as_str()
			.contains("test_scoped 10"),
		"an explicit default is recorded"
	);
	ctx.run("reset test_scoped").unwrap();
	assert!(
		!ctx
			.dump_with_options(options)
			.as_str()
			.contains("test_scoped")
	);

	let child = Ctx::new();
	child
		.set("test_scoped", Value::Int(11), Origin::Inherited)
		.unwrap();
	child.exec("test_layered 6", Source::Subagent).unwrap();
	let dump = child.dump_with_options(options);
	assert!(!dump.as_str().contains("test_scoped"), "{dump}");
	assert!(!dump.as_str().contains("test_layered"), "{dump}");
	child.run("test_scoped 12").unwrap();
	assert!(child.dump().as_str().contains("test_scoped 12"));
	child.run("reset test_scoped").unwrap();
	assert_eq!(child.get("test_scoped"), Some(Value::Int(11)));
	assert!(
		!child
			.dump_with_options(options)
			.as_str()
			.contains("test_scoped")
	);
}

/// A context presents another scope's inherited and class layers beneath its
/// own session writes (the main chat resuming a child), and drops them again.
#[test]
fn adopted_scope_sits_beneath_session_writes_and_drops_cleanly() {
	let child = Ctx::new();
	child
		.set("test_layered", Value::Int(5), Origin::Inherited)
		.unwrap();
	child
		.set("test_scoped", Value::Int(20), Origin::Inherited)
		.unwrap();
	child.exec("test_layered 6", Source::Subagent).unwrap();

	let main = Ctx::new();
	main
		.set("test_layered", Value::Int(2), Origin::Archive)
		.unwrap();
	let writes = main.subscribe_session_writes();
	main.adopt_scope(&child);
	assert_eq!(value(&main), Value::Int(6));
	assert_eq!(main.get("test_scoped"), Some(Value::Int(20)));
	assert!(writes.try_recv().is_err(), "an adopted scope is never journaled");
	main.run("test_layered 9").unwrap();
	assert_eq!(value(&main), Value::Int(9));
	main.run("reset test_layered").unwrap();
	assert_eq!(value(&main), Value::Int(6), "reset returns to the adopted class value");
	main.drop_scope();
	assert_eq!(value(&main), Value::Int(2));
	assert_eq!(main.get("test_scoped"), Some(Value::Int(10)));
}

/// The main scope's session writes — journaled or not — are parked while a
/// child scope is adopted: they never outrank the child's class, reach the
/// child only through the seed it inherits, and come back with `drop_scope`.
#[test]
fn adopted_scope_parks_the_main_session_layer_beneath_its_class() {
	let main = Ctx::new();
	main
		.set("test_layered", Value::Int(2), Origin::Archive)
		.unwrap();
	main.run("test_layered 3").unwrap();
	main
		.set("test_scoped", Value::Int(30), Origin::Host)
		.unwrap();
	let seed = main.scope_seed();
	assert_eq!(seed.get("test_layered"), Some(&Value::Int(3)), "session over archive");
	assert_eq!(seed.get("test_scoped"), Some(&Value::Int(30)), "host writes seed too");

	// The spawn path builds the child over that seed; its class sets only
	// `test_layered`.
	let child = Ctx::new();
	for (name, value) in seed.iter() {
		child
			.set(name.as_str(), value.clone(), Origin::Inherited)
			.unwrap();
	}
	child.exec("test_layered 6", Source::Subagent).unwrap();

	let writes = main.subscribe_session_writes();
	main.adopt_scope(&child);
	assert_eq!(value(&main), Value::Int(6), "the class outranks the main session's write");
	assert_eq!(main.get("test_scoped"), Some(Value::Int(30)), "inherited beneath the class");
	assert_eq!(main.session_writes().count(), 0, "the child's session layer starts empty");
	assert!(writes.try_recv().is_err(), "parking journals nothing");

	// The child's own write outranks its class and survives re-adoption (a
	// resync at every command boundary); the seed still describes the main
	// scope, never the child's writes.
	main.run("test_layered 9").unwrap();
	main.run("test_scoped 40").unwrap();
	assert_eq!(main.scope_seed().get("test_layered"), Some(&Value::Int(3)));
	main.adopt_scope(&child);
	assert_eq!(value(&main), Value::Int(9));
	assert_eq!(main.get("test_scoped"), Some(Value::Int(40)));
	main.run("reset test_scoped").unwrap();
	assert_eq!(main.get("test_scoped"), Some(Value::Int(30)), "reset falls to the inherited seed");

	// Switching back restores the main scope's layer, not the child's.
	while writes.try_recv().is_ok() {}
	main.drop_scope();
	assert_eq!(value(&main), Value::Int(3));
	assert_eq!(main.get("test_scoped"), Some(Value::Int(30)));
	assert!(writes.try_recv().is_err(), "restoring journals nothing");
	main.drop_scope();
	assert_eq!(value(&main), Value::Int(3), "a second drop is a no-op");
}

/// `reset` completes variable names and documents its inheritance.
#[test]
fn reset_completes_variables_and_explains_itself() {
	let replies = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
	let sink = std::sync::Arc::clone(&replies);
	let ctx = Ctx::builder()
		.sink(move |_, text| sink.lock().push(text.to_owned()))
		.build();
	let names: Vec<_> = ctx
		.complete("reset test_lay", 14)
		.into_iter()
		.map(|suggestion| suggestion.text.to_string())
		.collect();
	assert_eq!(names, ["test_layered"]);
	ctx.run("help reset").unwrap();
	let help = replies.lock().join("\n");
	assert!(help.contains("reset <var>") && help.contains("inherits"), "{help}");
}

#[test]
fn user_set_reads_the_user_layers_and_not_default_project_or_engagement() {
	let ctx = Ctx::new();
	assert!(!ctx.is_user_set("test_layered"), "the registration default is not a user choice");
	assert!(!ctx.is_user_set("test_no_such_var"), "an unknown name is not set");

	let layer =
		ctx.push_layer(Str::new_static("plan"), &[(Str::new_static("test_layered"), Value::Int(9))]);
	assert!(!ctx.is_user_set("test_layered"), "a director bind is not a user choice");
	ctx.pop_layer(layer);

	for origin in [Origin::Archive, Origin::Session] {
		ctx.set("test_layered", Value::Int(2), origin).unwrap();
		assert!(ctx.is_user_set("test_layered"));
		ctx.set("test_layered", Value::Int(1), Origin::Default)
			.unwrap();
		assert!(!ctx.is_user_set("test_layered"), "writing the default back clears the user layers");
	}
}
