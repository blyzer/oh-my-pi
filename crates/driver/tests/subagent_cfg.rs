//! ADR 0013 child seeding and cfg-order regression.

use std::fs;

use omp_driver::{cfg::CfgFiles, subagent::settings::child_ctx};

#[test]
fn child_uses_parent_live_then_user_and_project_spawn_cfgs() {
	let root = tempfile::tempdir().expect("scratch root");
	let user = root.path().join("user");
	let project = root.path().join("project/.omp");
	fs::create_dir_all(&user).expect("user cfg root");
	fs::create_dir_all(&project).expect("project cfg root");
	fs::write(user.join("config.cfg"), "ai_model stale\n").expect("stale main cfg");
	fs::write(user.join("subagent.cfg"), "ai_fastmode false\nai_thinking low\n")
		.expect("user subagent cfg");
	// The project overlay runs after the user subagent cfg, with project
	// authority: `ai_thinking` is project-scoped, `ai_fastmode` is not.
	fs::write(project.join("subagent.cfg"), "ai_thinking medium\nai_fastmode true\n")
		.expect("project subagent cfg");
	fs::write(user.join("sonic.cfg"), "ai_model sonic/model\n").expect("user class cfg");

	let parent = omp_con::Ctx::new();
	parent
		.run("ai_model live; ai_fastmode false; ai_thinking high")
		.expect("parent values");
	let files = CfgFiles::with_roots(user, Some(project));
	let child = child_ctx(&parent, &files, "sonic").expect("child context");

	assert_eq!(
		child
			.get_typed::<omp_core::Str>("ai_model")
			.expect("model")
			.as_str(),
		"sonic/model"
	);
	assert!(
		!child.get_typed::<bool>("ai_fastmode").expect("fast mode"),
		"a project overlay cannot set a convar that is not project-scoped"
	);
	assert_eq!(
		child
			.get_typed::<omp_core::Str>("ai_thinking")
			.expect("thinking")
			.as_str(),
		"medium"
	);
	assert_eq!(
		parent
			.get_typed::<omp_core::Str>("ai_model")
			.expect("parent model")
			.as_str(),
		"live"
	);
}

/// A parent under plan mode: Plan engaged in its session and its binds
/// derived into the console, exactly as the kernel derives them per request.
fn plan_parent(dir: &std::path::Path) -> (omp_con::Ctx, omp_session::Session) {
	use omp_agent::{DirectorRegistry, DirectorStack, directors::plan::Plan};

	let parent = omp_con::Ctx::new();
	let mut session = omp_session::Session::create(
		dir.join("parent.oms"),
		omp_session::ComponentRegistry::default(),
	)
	.expect("parent session");
	let registry = DirectorRegistry::standard();
	DirectorStack::from_dom(session.dom(), &registry)
		.engage(&mut session, Box::new(Plan::new("local://PLAN.md")))
		.expect("plan engages");
	DirectorStack::from_dom(session.dom(), &registry).apply_binds(session.dom(), &parent);
	(parent, session)
}

/// Children of a plan-mode parent are read-only (inheritance is a ceiling,
/// like the `task` recursion limit): the ceiling reaches every child and its
/// own children, and neither the child's cfg nor its console can lift it.
/// Children of any other parent are unaffected.
#[test]
fn children_of_a_plan_mode_parent_inherit_a_read_only_ceiling() {
	use omp_agent::{SV_TOOLS, SV_TOOLS_READ_ONLY, directors::plan::spawns_read_only};
	use omp_driver::subagent::spawn::configure_child;

	let root = tempfile::tempdir().expect("scratch root");
	let user = root.path().join("user");
	fs::create_dir_all(&user).expect("user cfg root");
	// The class cfg tries to widen the roster and lift the ceiling.
	fs::write(
		user.join("writer.cfg"),
		"sv_tools [write edit bash ast_edit read]\nsv_tools_read_only 0\n",
	)
	.expect("widening class cfg");
	let files = CfgFiles::with_roots(user, None);

	let (parent, _session) = plan_parent(root.path());
	assert!(spawns_read_only(&parent), "the plan Director owns the parent's sv_tools");
	let (child, _) = configure_child(&parent, &files, "writer", None).expect("child");
	assert!(SV_TOOLS_READ_ONLY.get(&child), "a cfg cannot lift the ceiling");
	assert_eq!(
		SV_TOOLS
			.get(&child)
			.iter()
			.map(|name| name.as_str())
			.collect::<Vec<_>>(),
		["write", "edit", "bash", "ast_edit", "read"],
		"the cfg's allowlist stands, capped by the ceiling at dispatch"
	);
	assert!(
		child.run("sv_tools_read_only 0").is_err(),
		"the child's console cannot lift the ceiling either"
	);
	assert!(SV_TOOLS_READ_ONLY.get(&child));

	let (grandchild, _) = configure_child(&child, &files, "writer", None).expect("grandchild");
	assert!(SV_TOOLS_READ_ONLY.get(&grandchild), "the ceiling is inherited downward");

	let ordinary = omp_con::Ctx::new();
	assert!(!spawns_read_only(&ordinary));
	let (free, _) = configure_child(&ordinary, &files, "writer", None).expect("ordinary child");
	assert!(!SV_TOOLS_READ_ONLY.get(&free), "a non-plan parent's children are unaffected");
}

/// A parent that set `sv_sandbox_mode off` and `sv_sandbox_network_mode
/// scoped` runs a network-only sandbox. Its children must too, although
/// `scoped` is also the shipped default: the seed carries the user's choice,
/// so the child does not read it as a defaulted network that leaves an
/// explicit `off` unsandboxed.
#[test]
fn children_keep_an_explicit_scoped_network_under_sandbox_mode_off() {
	use omp_driver::subagent::spawn::configure_child;
	use omp_envd::{
		admission::SandboxState,
		exec_settings::{NetworkConfinement, network_confinement},
	};

	let root = tempfile::tempdir().expect("scratch root");
	let plain_user = root.path().join("plain");
	fs::create_dir_all(&plain_user).expect("plain cfg root");
	let plain = CfgFiles::with_roots(plain_user, None);

	let parent = omp_con::Ctx::new();
	parent
		.run("sv_sandbox_mode off; sv_sandbox_network_mode scoped")
		.expect("parent sandbox settings");
	assert_eq!(network_confinement(&parent, SandboxState::Off), NetworkConfinement::Scoped);
	let (child, _) = configure_child(&parent, &plain, "writer", None).expect("child");
	assert_eq!(
		network_confinement(&child, SandboxState::Off),
		NetworkConfinement::Scoped,
		"the child keeps the parent's network-only sandbox"
	);
	let (grandchild, _) = configure_child(&child, &plain, "writer", None).expect("grandchild");
	assert_eq!(network_confinement(&grandchild, SandboxState::Off), NetworkConfinement::Scoped);

	// `subagent.cfg` turning the filesystem sandbox off keeps the parent's
	// explicit network as well.
	let off_user = root.path().join("off");
	fs::create_dir_all(&off_user).expect("off cfg root");
	fs::write(off_user.join("subagent.cfg"), "sv_sandbox_mode off\n").expect("subagent cfg");
	let off = CfgFiles::with_roots(off_user, None);
	let scoped_parent = omp_con::Ctx::new();
	scoped_parent
		.run("sv_sandbox_network_mode scoped")
		.expect("parent network");
	let (child, _) = configure_child(&scoped_parent, &off, "writer", None).expect("cfg-off child");
	assert_eq!(network_confinement(&child, SandboxState::Off), NetworkConfinement::Scoped);

	// A parent that left the network at its default spawns children whose
	// explicit `off` stays unsandboxed.
	let defaulted = omp_con::Ctx::new();
	defaulted.run("sv_sandbox_mode off").expect("parent mode");
	let (child, _) = configure_child(&defaulted, &plain, "writer", None).expect("defaulted child");
	assert_eq!(network_confinement(&child, SandboxState::Off), NetworkConfinement::Unconfined);
}
