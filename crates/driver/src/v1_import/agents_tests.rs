//! `agents` step proofs over temporary homes, v1 trees, v2 roots, and
//! projects. Nothing here reads the process environment or the real `~/.omp`
//! / `~/.o2`.

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use omp_core::Str;

use super::super::*;
use crate::{
	cfg::CfgFiles,
	discovery::rules::ActiveRules,
	subagent::{AgentName, MAIN_AGENT, settings::child_ctx},
};

/// Every frontmatter mapping at once: a folded description, a legacy role
/// reference with a thinking suffix, v1 tool aliases and `exec`, a tool v2
/// lacks, `task` withheld by `spawns: none`, and two keys without a home.
const REVIEWER: &str = concat!(
	"---\n",
	"name: reviewer\n",
	"description: Reviews diffs\n",
	"  for correctness\n",
	"model: pi/slow:high\n",
	"thinkingLevel: medium\n",
	"tools: read, search, exec, context_notes, Task\n",
	"spawns: none\n",
	"output:\n",
	"  type: object\n",
	"blocking: true\n",
	"---\n",
	"You review code.\n",
	"\n",
	"Be strict.\n",
);

const REVIEWER_CFG: &str = concat!(
	"// Reviews diffs for correctness\n",
	"ai_model @slow:high\n",
	"ai_thinking medium\n",
	"sv_tools [read grep eval bash yield hub]\n",
);

const REVIEWER_RULE: &str = concat!(
	"---\n",
	"alwaysApply: true\n",
	"agents:\n",
	"- reviewer\n",
	"---\n",
	"You review code.\n",
	"\n",
	"Be strict.\n",
);

/// v1 settings assigning the task role `@task` follows.
const V1_TASK_ROLE: &str = "modelRoles:\n  task: provider/task-model\n";

fn write(path: &Path, contents: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, contents).expect("write");
}

fn read(path: &Path) -> String {
	fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every file (with its bytes) and directory under `root`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
	let mut tree = BTreeMap::new();
	let mut pending = vec![root.to_owned()];
	while let Some(directory) = pending.pop() {
		let Ok(entries) = fs::read_dir(&directory) else {
			continue;
		};
		for entry in entries {
			let path = entry.expect("entry").path();
			if path.is_dir() {
				tree.insert(path.clone(), None);
				pending.push(path);
			} else {
				tree.insert(path.clone(), Some(fs::read(&path).expect("read")));
			}
		}
	}
	tree
}

/// A scratch home with a v1 install, v2 roots beside it, and a project.
struct Fixture {
	root: tempfile::TempDir,
}

impl Fixture {
	fn new() -> Self {
		Self { root: tempfile::tempdir().expect("scratch") }
	}

	fn home(&self) -> PathBuf {
		self.root.path().join("home")
	}

	fn omp(&self) -> PathBuf {
		self.home().join(".omp")
	}

	/// The default profile's v1 `agents/` directory.
	fn agents(&self) -> PathBuf {
		self.omp().join("agent/agents")
	}

	fn project(&self) -> PathBuf {
		self.root.path().join("project")
	}

	fn roots(&self) -> V2Roots {
		let root = self.root.path();
		V2Roots {
			config_dir:     root.join("o2"),
			data_dir:       root.join("share/omp"),
			state_dir:      root.join("state/omp"),
			cache_dir:      root.join("cache/omp"),
			active_profile: None,
		}
	}

	fn config(&self) -> PathBuf {
		self.roots().config_dir
	}

	fn source(&self) -> V1Source {
		V1Source::new(V1Inputs { home: self.home(), ..V1Inputs::default() })
	}

	fn run(&self, mode: ImportMode) -> ImportReport {
		let pairs = plan(&self.source(), &self.roots(), &ProfileSelection::All).expect("plan");
		run(&pairs, mode, CredentialAccess::Offline(&omp_con::Ctx::new()))
	}

	/// The rules a session in the project admits.
	fn rules(&self, config_root: &Path) -> ActiveRules {
		fs::create_dir_all(self.project()).expect("project");
		ActiveRules::discover(&self.project(), &self.home(), config_root, &Default::default())
	}
}

/// The agents step's entries: subject and outcome class.
fn agents<'a>(
	entries: impl IntoIterator<Item = &'a ImportEntry>,
) -> Vec<(Option<&'a str>, OutcomeKind)> {
	entries
		.into_iter()
		.filter(|entry| entry.step == ImportStep::Agents)
		.map(|entry| (entry.subject.as_deref(), entry.outcome.kind()))
		.collect()
}

fn report_agents(report: &ImportReport) -> Vec<(Option<&str>, OutcomeKind)> {
	agents(report.entries())
}

#[test]
fn a_dry_run_writes_nothing_and_an_import_runs_once_leaving_v1_byte_identical() {
	let fixture = Fixture::new();
	write(&fixture.agents().join("reviewer.md"), REVIEWER);
	let before = snapshot(fixture.root.path());

	let dry = fixture.run(ImportMode::DryRun);
	assert_eq!(snapshot(fixture.root.path()), before, "a dry run must not write anywhere");
	assert!(report_agents(&dry).contains(&(Some("reviewer"), OutcomeKind::WouldImport)));

	let v1_before = snapshot(&fixture.omp());
	let applied = fixture.run(ImportMode::Apply);
	assert!(report_agents(&applied).contains(&(Some("reviewer"), OutcomeKind::Imported)));
	assert_eq!(snapshot(&fixture.omp()), v1_before, "the v1 tree must stay byte-identical");
	assert!(ImportStep::Agents.marker(&fixture.config()).is_set());
	assert_eq!(read(&fixture.config().join("reviewer.cfg")), REVIEWER_CFG);

	let v2_after = snapshot(&fixture.config());
	let again = fixture.run(ImportMode::Apply);
	assert_eq!(report_agents(&again), [(None, OutcomeKind::Skipped)]);
	assert!(matches!(
		again
			.entries()
			.find(|entry| entry.step == ImportStep::Agents)
			.map(|entry| &entry.outcome),
		Some(ImportOutcome::Skipped(SkipReason::MarkerPresent))
	));
	assert_eq!(snapshot(&fixture.config()), v2_after, "a second run is a no-op");
	assert_eq!(snapshot(&fixture.omp()), v1_before);
}

#[test]
fn the_frontmatter_becomes_a_class_cfg_the_spawner_applies() {
	let fixture = Fixture::new();
	write(&fixture.agents().join("reviewer.md"), REVIEWER);
	write(
		&fixture.agents().join("scout.md"),
		concat!(
			"---\n",
			"name: scout\n",
			"description: Finds things\n",
			"model: \"@task\"\n",
			"thinkingLevel: HIGH\n",
			"tools: [read]\n",
			"spawns: \"*\"\n",
			"---\n",
			"Scout the tree.\n",
		),
	);
	write(&fixture.omp().join("agent/config.yml"), V1_TASK_ROLE);

	let report = fixture.run(ImportMode::Apply);

	assert_eq!(report_agents(&report), [
		(Some("reviewer"), OutcomeKind::Imported),
		(Some("reviewer: tool context_notes"), OutcomeKind::NotMigratable),
		(Some("reviewer: blocking"), OutcomeKind::NotMigratable),
		(Some("reviewer: output"), OutcomeKind::NotMigratable),
		(Some("scout"), OutcomeKind::Imported),
	]);
	assert_eq!(read(&fixture.config().join("reviewer.cfg")), REVIEWER_CFG);
	// `spawns: "*"` advertises `task`; with v1's task role assigned, `@task`
	// is v2's task role reference.
	assert_eq!(
		read(&fixture.config().join("scout.cfg")),
		concat!(
			"// Finds things\n",
			"ai_model @task\n",
			"ai_thinking high\n",
			"sv_tools [read task yield hub]\n",
		)
	);

	// The spawner's own loader runs the class cfg over the parent's values.
	let parent = omp_con::Ctx::new();
	parent
		.run("ai_model parent/model; ai_thinking low")
		.expect("parent values");
	let files = CfgFiles::with_roots(fixture.config(), None);
	let reviewer = child_ctx(&parent, &files, "reviewer").expect("reviewer child");
	assert_eq!(omp_agent::AI_MODEL.get(&reviewer).as_str(), "@slow:high");
	assert_eq!(omp_agent::AI_THINKING.get(&reviewer).as_str(), "medium");
	assert_eq!(omp_agent::SV_TOOLS.get(&reviewer), ["read", "grep", "eval", "bash", "yield", "hub"]);
	let scout = child_ctx(&parent, &files, "scout").expect("scout child");
	assert_eq!(omp_agent::AI_MODEL.get(&scout).as_str(), "@task");
	assert_eq!(omp_agent::SV_TOOLS.get(&scout), ["read", "task", "yield", "hub"]);
}

#[test]
fn an_imported_agent_model_outranks_the_task_model_as_in_v1() {
	let fixture = Fixture::new();
	write(&fixture.agents().join("reviewer.md"), REVIEWER);
	write(
		&fixture.agents().join("scout.md"),
		"---\nname: scout\ndescription: Finds things\nmodel: \"@task\"\n---\nScout the tree.\n",
	);
	write(&fixture.omp().join("agent/config.yml"), V1_TASK_ROLE);
	fixture.run(ImportMode::Apply);
	assert_eq!(read(&fixture.config().join("reviewer.cfg")), REVIEWER_CFG);

	// v1 resolved an agent's own `model` ahead of the task role; `@task`
	// named v1's task role, which v2 keeps as the `task` role reference.
	let parent = omp_con::Ctx::new();
	parent
		.run("ai_model parent/model; ai_task_model task/model")
		.expect("parent values");
	let files = CfgFiles::with_roots(fixture.config(), None);
	let spawned = |agent: &str| {
		let ctx = child_ctx(&parent, &files, agent).expect("child context");
		let settings = crate::subagent::settings::TaskSettings::from_con(&ctx);
		crate::subagent::spawn::configure_child_route(&ctx, &settings, agent, None)
			.expect("child route");
		omp_agent::AI_MODEL.get(&ctx)
	};
	assert_eq!(spawned("reviewer").as_str(), "@slow:high");
	assert_eq!(spawned("scout").as_str(), "@task");
}

/// v1 ran an agent without a `model` (or with `default`) on the session's
/// model, not the task role (`resolveAgentModelSelection`); only `@task`
/// followed the task role, and with no `modelRoles.task` assigned it too ran
/// on the session's model.
#[test]
fn an_agent_without_a_model_runs_on_the_session_model_not_the_task_model() {
	let fixture = Fixture::new();
	write(&fixture.agents().join("bare.md"), "---\nname: bare\ndescription: Bare\n---\nBody.\n");
	write(
		&fixture.agents().join("dflt.md"),
		"---\nname: dflt\ndescription: Default\nmodel: \"@default\"\n---\nBody.\n",
	);
	write(
		&fixture.agents().join("scout.md"),
		"---\nname: scout\ndescription: Finds things\nmodel: \"@task\"\n---\nScout.\n",
	);
	fixture.run(ImportMode::Apply);
	assert_eq!(read(&fixture.config().join("bare.cfg")), "// Bare\nai_model inherit\n");
	assert_eq!(read(&fixture.config().join("dflt.cfg")), "// Default\nai_model inherit\n");
	assert_eq!(read(&fixture.config().join("scout.cfg")), "// Finds things\nai_model inherit\n");

	let files = CfgFiles::with_roots(fixture.config(), None);
	let spawned = |parent: &omp_con::Ctx, agent: &str| {
		let ctx = child_ctx(parent, &files, agent).expect("child context");
		let settings = crate::subagent::settings::TaskSettings::from_con(&ctx);
		crate::subagent::spawn::configure_child_route(&ctx, &settings, agent, None)
			.expect("child route");
		omp_agent::AI_MODEL.get(&ctx)
	};
	let parent = omp_con::Ctx::new();
	parent
		.run("ai_model parent/model; ai_task_model task/model")
		.expect("parent values");
	assert_eq!(spawned(&parent, "bare").as_str(), "parent/model");
	assert_eq!(spawned(&parent, "dflt").as_str(), "parent/model");
	assert_eq!(spawned(&parent, "scout").as_str(), "parent/model");
}

/// v1's `@task` (`pi/task`) followed the task role, `modelRoles.task`, which
/// the settings step writes to `ai_model_roles.task`. When the v1 settings
/// assign it, the imported class's `ai_model @task` resolves through that
/// role in the child: the child's route is the role's model, not the
/// session's or `ai_task_model`.
#[test]
fn a_task_role_agent_follows_the_imported_task_role() {
	let fixture = Fixture::new();
	for (file, model) in [("scout.md", "\"@task\""), ("legacy.md", "pi/task")] {
		let name = file.trim_end_matches(".md");
		write(
			&fixture.agents().join(file),
			&format!("---\nname: {name}\ndescription: Finds things\nmodel: {model}\n---\nScout.\n"),
		);
	}
	write(&fixture.omp().join("agent/config.yml"), V1_TASK_ROLE);
	fixture.run(ImportMode::Apply);
	for name in ["scout", "legacy"] {
		assert_eq!(
			read(&fixture.config().join(format!("{name}.cfg"))),
			"// Finds things\nai_model @task\n"
		);
	}

	let catalog = omp_catalog::snapshot::Catalog::embedded();
	let mut models = catalog
		.models()
		.iter()
		.map(|model| model.key.as_str())
		.filter(|key| !key.contains([':', '@', ' ']));
	let (session, task) =
		(models.next().expect("a catalog model"), models.next().expect("a second catalog model"));
	let parent = omp_con::Ctx::new();
	parent
		.run(&format!("ai_model {session}; ai_task_model {session}; ai_model_roles {{task {task}}}"))
		.expect("parent values");
	let files = CfgFiles::with_roots(fixture.config(), None);
	for name in ["scout", "legacy"] {
		let child = child_ctx(&parent, &files, name).expect("child context");
		let settings = crate::subagent::settings::TaskSettings::from_con(&child);
		crate::subagent::spawn::configure_child_route(&child, &settings, name, None)
			.expect("child route");
		let selector = omp_agent::AI_MODEL.get(&child);
		assert_eq!(selector.as_str(), "@task");
		let selected = crate::discovery::roles::resolve_role_selector(
			catalog,
			&omp_catalog::settings::ModelSettings::from_con(&child),
			selector.as_str(),
		)
		.expect("the task role resolves in the child");
		assert_eq!(selected.model.as_str(), task, "{name} runs on the task role's model");
	}
}

/// Without `modelRoles.task` in the v1 settings, v1 had no task model for
/// `@task` to follow, so it ran on the session's model: the class gets
/// `ai_model inherit` (a `:level` suffix becomes its `ai_thinking`), and the
/// child runs on the spawning session's model — not `ai_task_model` and not
/// v2's catalog `@task`. The same holds for a project agent.
#[test]
fn a_task_role_agent_without_a_v1_task_role_inherits_the_session_model() {
	let fixture = Fixture::new();
	write(
		&fixture.agents().join("scout.md"),
		"---\nname: scout\ndescription: Finds things\nmodel: \"@task\"\n---\nScout.\n",
	);
	write(
		&fixture.agents().join("legacy.md"),
		"---\nname: legacy\ndescription: Finds things\nmodel: pi/task:low\n---\nScout.\n",
	);
	// Other roles do not count.
	write(&fixture.omp().join("agent/config.yml"), "modelRoles:\n  smol: provider/small\n");
	fixture.run(ImportMode::Apply);
	assert_eq!(read(&fixture.config().join("scout.cfg")), "// Finds things\nai_model inherit\n");
	assert_eq!(
		read(&fixture.config().join("legacy.cfg")),
		"// Finds things\nai_model inherit\nai_thinking low\n"
	);

	let project = fixture.project();
	write(
		&project.join(".omp/agents/probe.md"),
		"---\nname: probe\ndescription: Probes\nmodel: \"@task\"\n---\n",
	);
	import_project_agents(&project, &fixture.source(), &fixture.roots(), ImportMode::Apply);
	assert_eq!(read(&project.join(".omp/probe.cfg")), "// Probes\nai_model inherit\n");

	let parent = omp_con::Ctx::new();
	parent
		.run("ai_model parent/model; ai_task_model task/model")
		.expect("parent values");
	let files = CfgFiles::with_roots(fixture.config(), Some(project.join(".omp")));
	for name in ["scout", "legacy", "probe"] {
		let child = child_ctx(&parent, &files, name).expect("child context");
		assert_eq!(omp_agent::AI_MODEL.get(&child).as_str(), "parent/model", "{name}");
	}
}

/// v1 used a project agent in place of the user agent of its name. v2
/// layers the project class cfg over the user one, so the project cfg
/// resets what only the user agent sets, and shadows its body.
#[test]
fn a_project_agent_resets_what_only_the_user_agent_of_its_name_sets() {
	let fixture = Fixture::new();
	write(
		&fixture.agents().join("helper.md"),
		concat!(
			"---\n",
			"name: helper\n",
			"description: User helper\n",
			"model: anthropic/claude-opus-5\n",
			"thinkingLevel: low\n",
			"tools: [read, grep]\n",
			"---\n",
			"User body.\n",
		),
	);
	// A named profile's user agent of the same name counts too.
	write(
		&fixture.omp().join("profiles/work/agent/agents/lister.md"),
		"---\nname: lister\ndescription: Lists\ntools: [glob]\n---\nList things.\n",
	);
	let project = fixture.project();
	let omp = project.join(".omp");
	write(
		&omp.join("agents/helper.md"),
		"---\nname: helper\ndescription: Project helper\nmodel: \"@task\"\ntools: [grep]\n---\n",
	);
	// The project's own v1 settings assign the task role `@task` follows.
	write(&omp.join("config.yml"), V1_TASK_ROLE);
	write(
		&omp.join("agents/lister.md"),
		"---\nname: lister\ndescription: Project lister\nthinkingLevel: medium\n---\nOwn body.\n",
	);
	write(&omp.join("agents/solo.md"), "---\nname: solo\ndescription: Solo\n---\nSolo.\n");
	fixture.run(ImportMode::Apply);
	let roots = fixture.roots();
	let applied = import_project_agents(&project, &fixture.source(), &roots, ImportMode::Apply);
	assert_eq!(agents(&applied), [
		(Some("helper"), OutcomeKind::Imported),
		(Some("lister"), OutcomeKind::Imported),
		(Some("solo"), OutcomeKind::Imported),
	]);
	assert_eq!(
		read(&omp.join("helper.cfg")),
		concat!(
			"// Project helper\n",
			"ai_model @task\n",
			"sv_tools [grep yield hub]\n",
			"// v1 used this project agent in place of the user agent of its name\n",
			"reset ai_thinking\n",
		)
	);
	assert_eq!(
		read(&omp.join("lister.cfg")),
		concat!(
			"// Project lister\n",
			"ai_model inherit\n",
			"ai_thinking medium\n",
			"// v1 used this project agent in place of the user agent of its name\n",
			"reset sv_tools\n",
		)
	);
	assert_eq!(read(&omp.join("solo.cfg")), "// Solo\nai_model inherit\n");
	// The bodiless project agent still shadows the user body.
	assert_eq!(
		read(&omp.join("rules/agent-helper.md")),
		"---\nalwaysApply: false\nagents:\n- helper\n---\n"
	);

	// Layered, the user cfg then the project cfg equal the project agent
	// alone: the user agent's model, thinking level, and tools are gone.
	let parent = omp_con::Ctx::new();
	parent
		.run("ai_model parent/model; ai_task_model task/model")
		.expect("parent values");
	let layered = CfgFiles::with_roots(fixture.config(), Some(omp.clone()));
	let helper = child_ctx(&parent, &layered, "helper").expect("helper child");
	assert_eq!(omp_agent::AI_MODEL.get(&helper).as_str(), "@task");
	assert_eq!(
		omp_agent::AI_THINKING.get(&helper),
		omp_agent::AI_THINKING.get(&omp_con::Ctx::new()),
		"the declared default"
	);
	assert_eq!(omp_agent::SV_TOOLS.get(&helper), ["grep", "yield", "hub"]);
	let work = CfgFiles::with_roots(fixture.config().join("profiles/work"), Some(omp));
	let lister = child_ctx(&parent, &work, "lister").expect("lister child");
	assert!(omp_agent::SV_TOOLS.get(&lister).is_empty(), "every tool, as the project agent");
	assert_eq!(omp_agent::AI_MODEL.get(&lister).as_str(), "parent/model");

	let rules = fixture.rules(&fixture.config());
	// The user rule of the name is the one discovery skips.
	let skipped = rules
		.warnings
		.iter()
		.map(|warning| warning.path.as_path())
		.collect::<Vec<_>>();
	// Discovery reports canonical paths; the temp root may sit behind a
	// symlink (macOS `/var` → `/private/var`).
	let user_rule = std::fs::canonicalize(fixture.config().join("agent/rules/agent-helper.md"))
		.expect("canonical user rule path");
	assert_eq!(skipped, [user_rule]);
	let shadow = rules
		.get("agent-helper")
		.expect("the project rule wins the name");
	assert!(shadow.content.trim().is_empty(), "the user body is not carried");
	assert!(
		rules
			.prompt_facts(AgentName::from_ref("helper"))
			.always_apply
			.is_empty()
	);
}

#[test]
fn the_body_becomes_a_rule_admitted_only_for_its_class() {
	let fixture = Fixture::new();
	write(&fixture.agents().join("reviewer.md"), REVIEWER);

	fixture.run(ImportMode::Apply);

	let rule_path = fixture.config().join("agent/rules/agent-reviewer.md");
	assert_eq!(read(&rule_path), REVIEWER_RULE);
	let rules = fixture.rules(&fixture.config());
	assert!(rules.warnings.is_empty(), "{:?}", rules.warnings);
	let rule = rules.get("agent-reviewer").expect("rule admitted");
	assert!(rule.always_apply);
	assert_eq!(rule.agents, [Str::new_static("reviewer")]);
	assert_eq!(rule.content.as_str(), "You review code.\n\nBe strict.\n");
	let names = |agent: &str| {
		rules
			.for_agent(AgentName::from_ref(agent))
			.map(|rule| rule.name.as_str().to_owned())
			.collect::<Vec<_>>()
	};
	assert_eq!(names("reviewer"), ["agent-reviewer"]);
	assert!(
		names(MAIN_AGENT.as_str()).is_empty(),
		"the main session must not carry the agent's body"
	);
	assert_eq!(
		rules
			.prompt_facts(AgentName::from_ref("reviewer"))
			.always_apply
			.len(),
		1
	);
	assert!(rules.prompt_facts(MAIN_AGENT).always_apply.is_empty());
}

#[test]
fn unknown_tools_unmappable_models_and_keys_are_reported() {
	let fixture = Fixture::new();
	write(
		&fixture.agents().join("odd.md"),
		concat!(
			"---\n",
			"name: odd\n",
			"description: Odd one\n",
			"model: [\"@custom\", \"anthropic/claude-opus-5\"]\n",
			"thinkingLevel: auto\n",
			"tools: [read, mcp__github_search, new_context]\n",
			"spawns: scout, reviewer\n",
			"autoloadSkills: [rust]\n",
			"prewalk: true\n",
			"---\n",
			"Be odd.\n",
		),
	);
	write(
		&fixture.agents().join("plain.md"),
		concat!(
			"---\n",
			"name: plain\n",
			"description: Plain\n",
			"spawns: none\n",
			"model: \"pi/unknown-role\"\n",
			"---\n",
		),
	);
	write(&fixture.agents().join("broken.md"), "no frontmatter at all\n");

	let report = fixture.run(ImportMode::Apply);

	assert_eq!(report_agents(&report), [
		(Some("broken.md: no readable v1 agent frontmatter"), OutcomeKind::NotMigratable),
		(Some("odd"), OutcomeKind::Imported),
		(Some("odd: model @custom"), OutcomeKind::NotMigratable),
		(Some("odd: model fallback anthropic/claude-opus-5"), OutcomeKind::NotMigratable),
		(Some("odd: thinkingLevel auto"), OutcomeKind::NotMigratable),
		(Some("odd: spawns scout,reviewer"), OutcomeKind::NotMigratable),
		(Some("odd: tool mcp__github_search"), OutcomeKind::NotMigratable),
		(Some("odd: tool new_context"), OutcomeKind::NotMigratable),
		(Some("odd: autoloadSkills"), OutcomeKind::NotMigratable),
		(Some("odd: prewalk"), OutcomeKind::NotMigratable),
		(Some("plain"), OutcomeKind::Imported),
		(Some("plain: model pi/unknown-role"), OutcomeKind::NotMigratable),
		(Some("plain: spawns none without a tools list"), OutcomeKind::NotMigratable),
	]);
	// A spawn allowlist still lets the class delegate.
	assert_eq!(
		read(&fixture.config().join("odd.cfg")),
		"// Odd one\nsv_tools [read task yield hub]\n"
	);
	assert_eq!(read(&fixture.config().join("plain.cfg")), "// Plain\n");
	assert!(
		!fixture.config().join("agent/rules/agent-plain.md").exists(),
		"an empty body writes no rule"
	);
	assert!(ImportStep::Agents.marker(&fixture.config()).is_set());
}

#[test]
fn a_different_v2_file_or_a_built_in_class_is_a_conflict_that_keeps_v2() {
	let fixture = Fixture::new();
	write(&fixture.agents().join("reviewer.md"), REVIEWER);
	write(
		&fixture.agents().join("task.md"),
		"---\nname: task\ndescription: Overrides the default\n---\nBody.\n",
	);
	write(
		&fixture.agents().join("twin.md"),
		"---\nname: twin\ndescription: Twin\nthinkingLevel: low\n---\nTwin body.\n",
	);
	let mine = "// my own reviewer\nai_model @smol\n";
	write(&fixture.config().join("reviewer.cfg"), mine);
	// Identical content counts as already present; the missing rule lands.
	write(&fixture.config().join("twin.cfg"), "// Twin\nai_model inherit\nai_thinking low\n");

	let report = fixture.run(ImportMode::Apply);

	let reviewer_cfg = fixture.config().join("reviewer.cfg");
	let differs = format!("reviewer: {}", reviewer_cfg.display());
	assert_eq!(report_agents(&report), [
		(Some(differs.as_str()), OutcomeKind::NeedsAttention),
		(Some("task: built-in v2 agent class"), OutcomeKind::NeedsAttention),
		(Some("twin"), OutcomeKind::Imported),
	]);
	assert!(
		report
			.entries()
			.filter(|entry| entry.step == ImportStep::Agents)
			.take(2)
			.all(|entry| matches!(entry.outcome, ImportOutcome::NeedsAttention(Attention::Conflict)))
	);
	assert_eq!(read(&reviewer_cfg), mine, "v2's own class cfg is kept");
	let rules = fixture.config().join("agent/rules");
	assert!(!rules.join("agent-reviewer.md").exists(), "a colliding agent writes nothing");
	assert!(!fixture.config().join("task.cfg").exists());
	assert!(!rules.join("agent-task.md").exists());
	assert!(rules.join("agent-twin.md").is_file());
}

#[test]
fn project_agents_import_into_the_project_once() {
	let fixture = Fixture::new();
	let project = fixture.project();
	let omp = project.join(".omp");
	write(
		&omp.join("agents/helper.md"),
		"---\nname: helper\ndescription: Project helper\ntools: grep\n---\nHelp here.\n",
	);
	let roots = fixture.roots();
	let before = snapshot(fixture.root.path());

	let dry = import_project_agents(&project, &fixture.source(), &roots, ImportMode::DryRun);
	assert_eq!(agents(&dry), [(Some("helper"), OutcomeKind::WouldImport)]);
	assert_eq!(snapshot(fixture.root.path()), before, "a dry run must not write anywhere");

	let v1_before = snapshot(&omp.join("agents"));
	let applied = import_project_agents(&project, &fixture.source(), &roots, ImportMode::Apply);
	assert_eq!(agents(&applied), [(Some("helper"), OutcomeKind::Imported)]);
	assert_eq!(
		read(&omp.join("helper.cfg")),
		"// Project helper\nai_model inherit\nsv_tools [grep yield hub]\n"
	);
	assert_eq!(snapshot(&omp.join("agents")), v1_before);
	let marker = project_agents_marker(&project, &roots);
	assert!(marker.starts_with(&roots.state_dir), "the marker stays out of the repository");
	assert!(marker.is_file());

	// The project rule is admitted for the class from the project's own
	// `.omp/rules`, and the spawner reads the project class cfg.
	let rules = fixture.rules(&fixture.config());
	let names = |agent: &str| {
		rules
			.for_agent(AgentName::from_ref(agent))
			.map(|rule| rule.name.as_str().to_owned())
			.collect::<Vec<_>>()
	};
	assert_eq!(names("helper"), ["agent-helper"]);
	assert!(names(MAIN_AGENT.as_str()).is_empty());
	let files = CfgFiles::with_roots(fixture.config(), Some(omp));
	let child = child_ctx(&omp_con::Ctx::new(), &files, "helper").expect("helper child");
	assert_eq!(omp_agent::SV_TOOLS.get(&child), ["grep", "yield", "hub"]);

	let project_after = snapshot(&project);
	let again = import_project_agents(&project, &fixture.source(), &roots, ImportMode::Apply);
	assert_eq!(agents(&again), [(None, OutcomeKind::Skipped)]);
	assert_eq!(snapshot(&project), project_after, "a second run is a no-op");

	let empty = fixture.root.path().join("empty-project");
	fs::create_dir_all(&empty).expect("empty project");
	let nothing = import_project_agents(&empty, &fixture.source(), &roots, ImportMode::Apply);
	assert_eq!(agents(&nothing), [(None, OutcomeKind::NothingToImport)]);
	assert!(!project_agents_marker(&empty, &roots).exists());

	// Run from `$HOME`, the project's `.omp/` is the v1 install: never written.
	write(
		&fixture.omp().join("agents/stray.md"),
		"---\nname: stray\ndescription: Stray\n---\nBody.\n",
	);
	let v1_root = snapshot(&fixture.omp());
	let inside =
		import_project_agents(&fixture.home(), &fixture.source(), &roots, ImportMode::Apply);
	assert!(matches!(inside.as_slice(), [ImportEntry {
		outcome: ImportOutcome::Skipped(SkipReason::InsideV1Root),
		..
	}]));
	assert_eq!(snapshot(&fixture.omp()), v1_root);
}

#[test]
fn a_named_profile_imports_into_its_v2_namesake() {
	let fixture = Fixture::new();
	write(
		&fixture.omp().join("profiles/work/agent/agents/lead.md"),
		"---\nname: lead\ndescription: Work lead\nthinkingLevel: xhigh\n---\nLead the work.\n",
	);
	fs::create_dir_all(fixture.omp().join("agent")).expect("default profile");

	let report = fixture.run(ImportMode::Apply);

	assert_eq!(agents(&report.pairs[0].entries), [(None, OutcomeKind::NothingToImport)]);
	assert_eq!(agents(&report.pairs[1].entries), [(Some("lead"), OutcomeKind::Imported)]);
	let work = fixture.config().join("profiles/work");
	assert_eq!(read(&work.join("lead.cfg")), "// Work lead\nai_model inherit\nai_thinking xhigh\n");
	assert!(work.join("agent/rules/agent-lead.md").is_file());
	assert!(!fixture.config().join("lead.cfg").exists());
	assert!(ImportStep::Agents.marker(&fixture.config()).is_set());
	assert!(ImportStep::Agents.marker(&work).is_set());
	let rules = fixture.rules(&work);
	assert_eq!(
		rules
			.for_agent(AgentName::from_ref("lead"))
			.map(|rule| rule.name.as_str())
			.collect::<Vec<_>>(),
		["agent-lead"]
	);
}

#[test]
fn model_patterns_follow_v2_selector_rules() {
	use super::{ModelMapping, TaskRole, map_model};
	let selector = |text: &'static str| ModelMapping::Selector {
		selector: Str::new_static(text),
		thinking: None,
	};
	let mapped = |pattern: &str| map_model(pattern, TaskRole::Defined);
	assert_eq!(mapped("@smol"), selector("@smol"));
	assert_eq!(mapped("pi/slow"), selector("@slow"));
	assert_eq!(mapped("*:high"), selector("@default:high"));
	assert_eq!(mapped("anthropic/claude-opus-5:xhigh"), selector("anthropic/claude-opus-5:xhigh"));
	assert_eq!(mapped("opus:inherit"), selector("opus"));
	assert_eq!(mapped("opus:off"), ModelMapping::Selector {
		selector: Str::new_static("opus"),
		thinking: Some("off"),
	});
	for inherited in ["*", "default", "@default", "pi/default"] {
		assert_eq!(mapped(inherited), ModelMapping::Session { thinking: None }, "{inherited}");
	}
	for task in ["@task", "pi/task"] {
		assert_eq!(mapped(task), selector("@task"), "{task}");
		assert_eq!(
			map_model(task, TaskRole::Undefined),
			ModelMapping::Session { thinking: None },
			"{task} without a v1 task role"
		);
	}
	assert_eq!(map_model("@task:high", TaskRole::Undefined), ModelMapping::Session {
		thinking: Some("high"),
	});
	assert_eq!(map_model("@task:off", TaskRole::Undefined), ModelMapping::Session {
		thinking: Some("off"),
	});
	assert_eq!(
		map_model("@smol", TaskRole::Undefined),
		selector("@smol"),
		"only the task role depends on it"
	);
	assert_eq!(mapped("@my-role"), ModelMapping::Unmappable);
	assert_eq!(mapped("pi/nope"), ModelMapping::Unmappable);
	assert_eq!(mapped("@smol:bogus"), ModelMapping::Unmappable);
	assert_eq!(mapped("@plan:auto"), selector("@plan:auto"));
	assert_eq!(mapped("bad::selector"), ModelMapping::Unmappable);
}
