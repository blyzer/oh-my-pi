//! Asset-step proofs over a temporary home, v1 tree, project, and v2 roots.
//! Nothing here reads the process environment or the real `~/.omp` / `~/.o2`.

use std::{
	collections::BTreeMap,
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use omp_envd::{
	docserver::lsp_config::discover_native_lsp_sources,
	mcp::{McpConfigPaths, config_store::McpConfigStore},
	ssh::{AuthPolicy, HostConfig, HostPaths, HostStore},
};

use crate::{
	discovery::{
		prompts::PromptTemplates,
		rules::ActiveRules,
		skills::{SkillLevel, SkillPolicy, sources},
	},
	secrets::config::load_secret_rules,
	v1_import::{
		CredentialAccess, ImportEntry, ImportMode, ImportOutcome, ImportPair, ImportReport,
		ImportStep, OutcomeKind, ProfileSelection, SkipReason, V1Inputs, V1Item, V1Source, V2Roots,
		import_project_assets, plan, project_assets_marker, report::Attention, run,
	},
};

/// An Ed25519 host key and its `SHA256:` fingerprint, as `ssh-keygen -l`
/// prints it.
const HOST_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g";
const HOST_KEY_SHA256: &str = "SHA256:mKqU+0K8OhKmA8bBQi9Rz0Q5l7/g160hIP+rJYSTNj4";
const OTHER_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj9A";
const OTHER_KEY_SHA256: &str = "SHA256:yLcHRnrl/YjyiSIWUGY7IPRLbf9VEWEyVnQKRyzhxKw";

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

/// A scratch home, v1 root, project, and v2 roots.
struct Fixture {
	root:    tempfile::TempDir,
	home:    PathBuf,
	omp:     PathBuf,
	project: PathBuf,
	v2:      V2Roots,
}

impl Fixture {
	fn new() -> Self {
		let root = tempfile::tempdir().expect("scratch");
		let home = root.path().join("home");
		let project = root.path().join("work/repo");
		fs::create_dir_all(&project).expect("project");
		let v2 = V2Roots {
			config_dir:     root.path().join("o2"),
			data_dir:       root.path().join("share/omp"),
			state_dir:      root.path().join("state/omp"),
			cache_dir:      root.path().join("cache/omp"),
			active_profile: None,
		};
		Self { omp: home.join(".omp"), home, project, v2, root }
	}

	fn agent(&self) -> PathBuf {
		self.omp.join("agent")
	}

	fn config(&self) -> &Path {
		&self.v2.config_dir
	}

	fn source(&self) -> V1Source {
		V1Source::new(V1Inputs { home: self.home.clone(), ..V1Inputs::default() })
	}

	fn pairs(&self) -> Vec<ImportPair> {
		plan(&self.source(), &self.v2, &ProfileSelection::All).expect("plan")
	}

	/// Converts `project`'s v1-only `.omp/` files.
	fn project(&self, project: &Path) -> Vec<ImportEntry> {
		import_project_assets(project, &self.source(), &self.v2, ImportMode::Apply)
	}

	fn known_host(&self, pattern: &str, key: &str) {
		let path = self.home.join(".ssh/known_hosts");
		let mut text = fs::read_to_string(&path).unwrap_or_default();
		let _ = writeln!(text, "{pattern} ssh-ed25519 {key}");
		write(&path, &text);
	}
}

fn import(pairs: &[ImportPair], mode: ImportMode) -> ImportReport {
	run(pairs, mode, CredentialAccess::Offline(&omp_con::Ctx::new()))
}

fn apply(pairs: &[ImportPair]) -> ImportReport {
	import(pairs, ImportMode::Apply)
}

/// `(item, subject, kind)` for one step's entries.
fn kinds(report: &ImportReport, step: ImportStep) -> Vec<(V1Item, Option<&str>, OutcomeKind)> {
	report
		.entries()
		.filter(|entry| entry.step == step)
		.map(|entry| (entry.item, entry.subject.as_deref(), entry.outcome.kind()))
		.collect()
}

/// `(step, subject, kind)` for project entries.
fn listed(entries: &[ImportEntry]) -> Vec<(ImportStep, Option<&str>, OutcomeKind)> {
	entries
		.iter()
		.map(|entry| (entry.step, entry.subject.as_deref(), entry.outcome.kind()))
		.collect()
}

fn entry<'r>(report: &'r ImportReport, step: ImportStep, subject: &str) -> &'r ImportEntry {
	report
		.entries()
		.find(|entry| entry.step == step && entry.subject.as_deref() == Some(subject))
		.unwrap_or_else(|| panic!("no {step} entry for {subject}: {report:#?}"))
}

/// A v1 agent directory with one of every asset.
fn populate(fixture: &Fixture, agent: &Path, tag: &str) {
	write(
		&agent.join("skills/review/SKILL.md"),
		&format!("---\nname: review\ndescription: Review code ({tag})\n---\nReview.\n"),
	);
	write(&agent.join("skills/review/scripts/run.sh"), "#!/bin/sh\necho review\n");
	write(
		&agent.join("managed-skills/learned/SKILL.md"),
		"---\nname: learned\ndescription: Learned\n---\nLearned.\n",
	);
	write(&agent.join("rules/style.md"), "---\nalwaysApply: true\n---\nUse tabs.\n");
	write(&agent.join("RULES.md"), &format!("Sticky {tag}.\n"));
	write(&agent.join("AGENTS.md"), &format!("Guidance {tag}.\n"));
	write(
		&agent.join("prompts/explain.md"),
		"---\ndescription: Explain code\n---\nExplain $ARGUMENTS\n",
	);
	write(&agent.join("commands/ship.md"), "---\ndescription: Ship a change\n---\nShip $1 now.\n");
	write(&agent.join("themes/dusk.json"), "{\"name\": \"dusk\"}\n");
	write(&agent.join("SYSTEM.md"), &format!("System {tag}.\n"));
	write(&agent.join("APPEND_SYSTEM.md"), &format!("Append {tag}.\n"));
	write(&agent.join("TITLE_SYSTEM.md"), &format!("Title {tag}.\n"));
	write(&agent.join("lsp.json"), r#"{"servers":{"rust-analyzer":{"args":["--log"]}}}"#);
	write(
		&agent.join("dap.yml"),
		"adapters:\n  debugpy:\n    launchDefaults:\n      stopOnEntry: false\n",
	);
	write(&agent.join("secrets.yml"), "- type: plain\n  content: hunter2-secret\n");
	write(
		&agent.join("mcp.json"),
		r#"{"mcpServers":{"docs":{"command":"docs-mcp","args":["--stdio"]}}}"#,
	);
	write(
		&agent.join("ssh.json"),
		r#"{"hosts":{"build":{"host":"build.example","username":"ci"}}}"#,
	);
	fixture.known_host("build.example", HOST_KEY);
}

const ASSET_STEPS: [ImportStep; 10] = [
	ImportStep::Skills,
	ImportStep::Rules,
	ImportStep::Prompts,
	ImportStep::Commands,
	ImportStep::Themes,
	ImportStep::SystemPrompts,
	ImportStep::LspDap,
	ImportStep::Secrets,
	ImportStep::Mcp,
	ImportStep::SshHosts,
];

#[test]
fn a_dry_run_reports_every_asset_and_writes_nothing() {
	let fixture = Fixture::new();
	populate(&fixture, &fixture.agent(), "default");
	let before = snapshot(fixture.root.path());

	let report = import(&fixture.pairs(), ImportMode::DryRun);

	assert_eq!(snapshot(fixture.root.path()), before, "a dry run must not write anywhere");
	for step in ASSET_STEPS {
		assert!(
			kinds(&report, step)
				.iter()
				.any(|(_, _, kind)| *kind == OutcomeKind::WouldImport),
			"{step}: {:#?}",
			kinds(&report, step)
		);
	}
	// Every located asset has its importer in the inventory.
	let inventory = &report.pairs[0].inventory;
	for (item, _, steps) in inventory {
		if !matches!(item, V1Item::Models | V1Item::Settings) {
			assert!(!steps.is_empty(), "{item} has no importer");
		}
	}
	assert!(
		inventory
			.iter()
			.any(|(item, _, steps)| *item == V1Item::ManagedSkills && steps == &[ImportStep::Skills])
	);
}

#[test]
fn assets_land_where_v2_reads_them_once_per_profile() {
	let fixture = Fixture::new();
	populate(&fixture, &fixture.agent(), "default");
	populate(&fixture, &fixture.omp.join("profiles/work/agent"), "work");
	let v1_before = snapshot(&fixture.omp);
	let pairs = fixture.pairs();
	assert_eq!(pairs.len(), 2);

	let report = apply(&pairs);

	// Copy only: every v1 file and directory is exactly as it was.
	assert_eq!(snapshot(&fixture.omp), v1_before);
	for (profile, tag) in [(None, "default"), (Some("work"), "work")] {
		let target = fixture.v2.target(profile);
		let config = target.config_dir.as_path();
		let agent = config.join("agent");
		// Skills, as discovery's native user and managed roots.
		let roots = sources(&fixture.project, &fixture.home, config, &SkillPolicy::default());
		assert!(
			roots
				.iter()
				.any(|source| source.level == SkillLevel::User && source.root == agent.join("skills"))
		);
		assert!(read(&agent.join("skills/review/SKILL.md")).contains(tag));
		assert!(agent.join("skills/review/scripts/run.sh").is_file());
		assert!(
			crate::discovery::skills::managed_skills_root(config)
				.join("learned/SKILL.md")
				.is_file()
		);
		// Rules, the sticky RULES.md, and AGENTS.md.
		let rules = ActiveRules::discover(&fixture.project, &fixture.home, config);
		let names = rules
			.rules
			.iter()
			.map(|rule| rule.name.as_str())
			.collect::<Vec<_>>();
		assert!(names.contains(&"style") && names.contains(&"RULES"), "{names:?}");
		assert_eq!(read(&agent.join("AGENTS.md")), format!("Guidance {tag}.\n"));
		// Prompts and commands both become templates.
		let templates = PromptTemplates::discover(&fixture.project, config, &[], true);
		assert!(templates.warnings.is_empty(), "{:?}", templates.warnings);
		assert!(templates.get("explain").is_some());
		assert_eq!(
			templates.expand_line("/ship v2").as_deref(),
			Some("Ship v2 now."),
			"a v1 slash command runs as a v2 template"
		);
		assert!(agent.join("themes/dusk.json").is_file());
		for name in ["SYSTEM.md", "APPEND_SYSTEM.md", "TITLE_SYSTEM.md"] {
			assert!(read(&agent.join(name)).contains(tag), "{name}");
		}
		// LSP and DAP under `agent/`, where the v2 hosts read them.
		let lsp = discover_native_lsp_sources(Some(config), &fixture.project).expect("lsp");
		assert!(
			lsp.iter()
				.any(|source| source.provenance.source.ends_with("agent/lsp.json"))
		);
		assert!(agent.join("dap.yml").is_file());
		// `secrets.yml` sits at the config root, not under `agent/`.
		assert!(!agent.join("secrets.yml").exists());
		let secrets = load_secret_rules(&config.join("secrets.yml"), &fixture.project.join("x"))
			.expect("secrets");
		assert_eq!(secrets.len(), 1);
		// MCP and SSH, at the files v2's stores use.
		let mcp = McpConfigPaths::new(config, &fixture.project);
		assert!(
			McpConfigStore::new(mcp.user)
				.get("docs")
				.expect("mcp")
				.is_some()
		);
		let hosts =
			HostStore::load_layered(&HostPaths::new(config, &fixture.project)).expect("hosts");
		assert_eq!(hosts.get("build").expect("build").host_key.as_str(), HOST_KEY_SHA256);
		for step in ASSET_STEPS {
			assert!(step.marker(config).is_set(), "{step} marker for {profile:?}");
		}
	}
	for step in ASSET_STEPS {
		assert!(
			kinds(&report, step).iter().all(|(_, _, kind)| matches!(
				kind,
				OutcomeKind::Imported | OutcomeKind::NothingToImport
			)),
			"{step}: {:#?}",
			kinds(&report, step)
		);
	}

	// A second run is a no-op through the markers, even after v1 changes.
	write(&fixture.agent().join("skills/late/SKILL.md"), "late");
	let v2_before = snapshot(fixture.config());
	let again = apply(&pairs);
	assert_eq!(snapshot(fixture.config()), v2_before);
	for step in ASSET_STEPS {
		assert!(
			again
				.entries()
				.filter(|entry| entry.step == step)
				.all(|entry| matches!(
					entry.outcome,
					ImportOutcome::Skipped(SkipReason::MarkerPresent)
				)),
			"{step}"
		);
	}
}

#[test]
fn a_tree_copy_keeps_v2_files_and_reports_each_conflict() {
	let fixture = Fixture::new();
	let agent = fixture.agent();
	write(&agent.join("skills/a/SKILL.md"), "v1 a");
	write(&agent.join("skills/b/SKILL.md"), "same b");
	write(&agent.join("skills/c/SKILL.md"), "v1 c");
	let v2_skills = fixture.config().join("agent/skills");
	write(&v2_skills.join("a/SKILL.md"), "v2 a");
	write(&v2_skills.join("b/SKILL.md"), "same b");

	let report = apply(&fixture.pairs()[..1]);

	assert_eq!(read(&v2_skills.join("a/SKILL.md")), "v2 a", "v2's file wins");
	assert_eq!(read(&v2_skills.join("c/SKILL.md")), "v1 c");
	assert_eq!(kinds(&report, ImportStep::Skills), [
		(V1Item::Skills, Some("agent/skills (1 file)"), OutcomeKind::Imported),
		(V1Item::Skills, Some("agent/skills (1 file)"), OutcomeKind::Skipped),
		(V1Item::Skills, Some("agent/skills/a/SKILL.md"), OutcomeKind::NeedsAttention),
		(V1Item::ManagedSkills, None, OutcomeKind::NothingToImport),
	]);
	let conflict = entry(&report, ImportStep::Skills, "agent/skills/a/SKILL.md");
	assert!(matches!(conflict.outcome, ImportOutcome::NeedsAttention(Attention::Conflict)));
	assert_eq!(conflict.path.as_deref(), Some(agent.join("skills/a/SKILL.md").as_path()));
	let present = &report.pairs[0]
		.entries
		.iter()
		.filter(|entry| entry.step == ImportStep::Skills)
		.nth(1)
		.expect("present");
	assert!(matches!(present.outcome, ImportOutcome::Skipped(SkipReason::AlreadyPresent)));
}

#[test]
fn an_mcp_merge_keeps_the_v2_server_on_a_name_conflict() {
	let fixture = Fixture::new();
	let agent = fixture.agent();
	write(
		&fixture.config().join("mcp.json"),
		r#"{"mcpServers":{"shared":{"command":"v2-shared"},"own":{"command":"own"}}}"#,
	);
	write(
		&agent.join("mcp.json"),
		r#"{"mcpServers":{"shared":{"command":"v1-shared"},"fresh":{"command":"fresh"},
		   "same":{"url":"https://same.example/mcp"}},"disabledServers":["noisy"]}"#,
	);
	write(
		&agent.join(".mcp.json"),
		r#"{"mcpServers":{"hidden":{"command":"hidden"},"fresh":{"command":"other"},
		   "bad name":{"command":"x"}}}"#,
	);
	let v1_before = snapshot(&fixture.omp);

	let report = apply(&fixture.pairs()[..1]);

	let store = McpConfigStore::new(fixture.config().join("mcp.json"));
	let merged = store.read().expect("merged");
	let command = |name: &str| {
		merged.mcp_servers[name]
			.command
			.as_deref()
			.map(str::to_owned)
	};
	assert_eq!(command("shared").as_deref(), Some("v2-shared"), "the existing v2 server wins");
	assert_eq!(command("own").as_deref(), Some("own"));
	assert_eq!(command("fresh").as_deref(), Some("fresh"), "v1 `mcp.json` outranks `.mcp.json`");
	assert_eq!(command("hidden").as_deref(), Some("hidden"));
	assert!(merged.mcp_servers.contains_key("same"));
	assert!(!merged.mcp_servers.contains_key("bad name"));
	assert!(merged.disabled_servers.contains("noisy"));
	assert!(matches!(
		entry(&report, ImportStep::Mcp, "shared").outcome,
		ImportOutcome::NeedsAttention(Attention::Conflict)
	));
	assert!(matches!(
		entry(&report, ImportStep::Mcp, "bad name").outcome,
		ImportOutcome::NeedsAttention(Attention::Incompatible(_))
	));
	assert_eq!(
		report
			.entries()
			.filter(|entry| entry.subject.as_deref() == Some("fresh"))
			.map(|entry| entry.outcome.kind())
			.collect::<Vec<_>>(),
		[OutcomeKind::Imported, OutcomeKind::NeedsAttention]
	);
	// Copied, never moved: both v1 files are still there, byte for byte.
	assert_eq!(snapshot(&fixture.omp), v1_before);
}

#[test]
fn ssh_json_converts_to_hosts_toml() {
	let fixture = Fixture::new();
	let agent = fixture.agent();
	write(
		&agent.join("ssh.json"),
		r#"{"hosts":{
			"build":{"host":"build.example","username":"ci","port":"2222",
				"keyPath":"~/.ssh/id_build","description":"CI box","compat":"yes"},
			"plain":{"host":"plain.example","username":"me","port":22},
			"unpinned":{"host":"unknown.example","username":"me"},
			"anonymous":{"host":"plain.example"},
			"env":{"host":"${BUILD_HOST}","username":"me"},
			"kept":{"host":"kept.example","username":"me"}
		}}"#,
	);
	fixture.known_host("[build.example]:2222", HOST_KEY);
	fixture.known_host("plain.example", OTHER_KEY);
	fixture.known_host("kept.example", HOST_KEY);
	let hosts_toml = fixture.config().join("hosts.toml");
	write(
		&hosts_toml,
		"[hosts.kept]\naddress = \"kept.example\"\nuser = \"v2\"\nhost_key = \"SHA256:v2\"\nauth = \
		 { type = \"agent\" }\n",
	);

	let report = apply(&fixture.pairs()[..1]);

	let hosts = HostStore::load(&hosts_toml).expect("hosts.toml");
	let mut build = HostConfig::new(
		"build.example".into(),
		"ci".into(),
		HOST_KEY_SHA256.into(),
		AuthPolicy::Key { path: fixture.home.join(".ssh/id_build") },
	);
	build.port = 2222;
	assert_eq!(hosts.get("build").expect("build"), build);
	assert_eq!(
		hosts.get("plain").expect("plain"),
		HostConfig::new(
			"plain.example".into(),
			"me".into(),
			OTHER_KEY_SHA256.into(),
			AuthPolicy::Agent
		)
	);
	assert_eq!(hosts.get("kept").expect("kept").user.as_str(), "v2", "v2's host wins");
	assert_eq!(
		hosts
			.aliases()
			.iter()
			.map(|alias| alias.as_str())
			.collect::<Vec<_>>(),
		["build", "kept", "plain"]
	);
	assert_eq!(kinds(&report, ImportStep::SshHosts), [
		(V1Item::Ssh, Some("anonymous"), OutcomeKind::NeedsAttention),
		(V1Item::Ssh, Some("build description"), OutcomeKind::NotMigratable),
		(V1Item::Ssh, Some("build compat"), OutcomeKind::NotMigratable),
		(V1Item::Ssh, Some("build"), OutcomeKind::Imported),
		(V1Item::Ssh, Some("env"), OutcomeKind::NeedsAttention),
		(V1Item::Ssh, Some("kept"), OutcomeKind::NeedsAttention),
		(V1Item::Ssh, Some("plain"), OutcomeKind::Imported),
		(V1Item::Ssh, Some("unpinned"), OutcomeKind::NeedsAttention),
	]);
	let reason = |alias: &str| match &entry(&report, ImportStep::SshHosts, alias).outcome {
		ImportOutcome::NeedsAttention(Attention::Incompatible(
			crate::v1_import::ImportError::Assets(error),
		)) => format!("{error}"),
		other => format!("{other:?}"),
	};
	assert!(reason("unpinned").contains("known_hosts"), "{}", reason("unpinned"));
	assert!(reason("anonymous").contains("username"), "{}", reason("anonymous"));
	assert!(reason("env").contains("VAR"), "{}", reason("env"));
	assert!(matches!(
		entry(&report, ImportStep::SshHosts, "kept").outcome,
		ImportOutcome::NeedsAttention(Attention::Conflict)
	));
}

#[test]
fn v1_commands_become_prompt_templates() {
	let fixture = Fixture::new();
	let agent = fixture.agent();
	write(
		&agent.join("commands/review.md"),
		"---\ndescription: Review a change\n---\nReview $ARGUMENTS thoroughly.\n",
	);
	write(&agent.join("commands/plain.md"), "Summarize $1.\n");
	write(&agent.join("commands/broken.md"), "---\ndescription: [unclosed\n---\nBody.\n");
	write(&agent.join("commands/deploy/index.ts"), "export default {};\n");
	write(&agent.join("commands/notes.txt"), "not a command");
	write(&agent.join("commands/.hidden.md"), "hidden");
	// A v1 prompt template of the same name keeps its place.
	write(&agent.join("prompts/plain.md"), "Prompt $1.\n");

	let report = apply(&fixture.pairs()[..1]);

	let config = fixture.config();
	let templates = PromptTemplates::discover(&fixture.project, config, &[], true);
	let review = templates.get("review").expect("review");
	assert_eq!(review.description.as_str(), "Review a change (user)");
	assert_eq!(
		templates.expand_line("/review the diff").as_deref(),
		Some("Review the diff thoroughly.")
	);
	assert_eq!(templates.expand_line("/plain it").as_deref(), Some("Prompt it."));
	assert!(templates.get("broken").is_none());
	assert!(!config.join("agent/prompts/broken.md").exists());
	assert!(!config.join("agent/prompts/.hidden.md").exists());
	assert_eq!(kinds(&report, ImportStep::Commands), [
		(V1Item::Commands, Some("agent/prompts/broken.md"), OutcomeKind::NeedsAttention),
		(V1Item::Commands, Some("deploy"), OutcomeKind::NotMigratable),
		(V1Item::Commands, Some("agent/prompts/plain.md"), OutcomeKind::NeedsAttention),
		(V1Item::Commands, Some("agent/prompts/review.md"), OutcomeKind::Imported),
	]);
}

#[test]
fn lsp_dap_and_secrets_copy_only_what_v2_can_load() {
	let fixture = Fixture::new();
	let agent = fixture.agent();
	write(
		&agent.join("lsp.json"),
		r#"{"servers":{"mine":{"command":"mine-ls","fileTypes":[".x"],"rootMarkers":[".git"]}}}"#,
	);
	// v2's schema has no `env`, so this file would break every LSP server.
	write(&agent.join(".lsp.yaml"), "servers:\n  rust-analyzer:\n    env:\n      A: b\n");
	write(&agent.join("dap.json"), r#"{"adapters":{"acme":{"args":["--x"]}}}"#);
	write(&agent.join("secrets.yml"), "- type: shout\n  content: hunter2-secret\n");

	let report = apply(&fixture.pairs()[..1]);

	let config = fixture.config();
	assert!(config.join("agent/lsp.json").is_file());
	assert!(!config.join("agent/.lsp.yaml").exists());
	assert!(!config.join("agent/dap.json").exists(), "a new adapter needs a command");
	assert!(!config.join("secrets.yml").exists());
	let lsp = discover_native_lsp_sources(Some(config), &fixture.project).expect("lsp");
	omp_envd::docserver::lsp_config::load_lsp_config(&lsp).expect("the copied LSP file loads");
	assert_eq!(kinds(&report, ImportStep::LspDap), [
		(V1Item::Lsp, Some("agent/lsp.json"), OutcomeKind::Imported),
		(V1Item::Lsp, Some("agent/.lsp.yaml"), OutcomeKind::NeedsAttention),
		(V1Item::Dap, Some("agent/dap.json"), OutcomeKind::NeedsAttention),
	]);
	assert_eq!(kinds(&report, ImportStep::Secrets), [(
		V1Item::Secrets,
		Some("secrets.yml"),
		OutcomeKind::NeedsAttention
	)]);
}

#[test]
fn a_project_omp_converts_in_place_once() {
	let fixture = Fixture::new();
	let omp = fixture.project.join(".omp");
	write(
		&omp.join("ssh.json"),
		r#"{"hosts":{"stage":{"host":"stage.example","username":"deploy"}}}"#,
	);
	fixture.known_host("stage.example", HOST_KEY);
	write(&omp.join("mcp.json"), r#"{"mcpServers":{"repo":{"command":"repo-mcp"}}}"#);
	write(
		&omp.join(".mcp.json"),
		r#"{"mcpServers":{"repo":{"command":"other"},"lint":{"command":"lint-mcp"}}}"#,
	);
	write(&omp.join("commands/release.md"), "Release $1.\n");
	let project_before = snapshot(&omp);

	let report = fixture.project(&fixture.project);

	let config = fixture.config();
	let hosts = HostStore::load_layered(&HostPaths::new(config, &fixture.project)).expect("hosts");
	assert_eq!(hosts.get("stage").expect("stage").address.as_str(), "stage.example");
	assert!(omp.join("hosts.toml").is_file());
	assert!(!config.join("hosts.toml").exists(), "project hosts stay in the project");
	let mcp = McpConfigStore::new(McpConfigPaths::new(config, &fixture.project).project)
		.read()
		.expect("project mcp");
	assert_eq!(mcp.mcp_servers["repo"].command.as_deref(), Some("repo-mcp"));
	assert_eq!(mcp.mcp_servers["lint"].command.as_deref(), Some("lint-mcp"));
	let templates = PromptTemplates::discover(&fixture.project, config, &[], true);
	assert_eq!(templates.get("release").expect("release").source.as_str(), "(project)");
	assert_eq!(listed(&report), [
		(ImportStep::SshHosts, Some("stage"), OutcomeKind::Imported),
		(ImportStep::Mcp, Some("lint"), OutcomeKind::Imported),
		(ImportStep::Mcp, Some("repo"), OutcomeKind::NeedsAttention),
		(ImportStep::Commands, Some(".omp/prompts/release.md"), OutcomeKind::Imported),
	]);
	// The v1-only project files are untouched; `mcp.json`, which both
	// versions read, gained the merged servers.
	let after = snapshot(&omp);
	for (path, contents) in project_before
		.iter()
		.filter(|(path, _)| !path.ends_with("mcp.json"))
	{
		assert_eq!(after.get(path), Some(contents), "{}", path.display());
	}
	// The marker lives under the v2 state root, never in the repository.
	let marker = project_assets_marker(&fixture.project, &fixture.v2);
	assert!(marker.starts_with(&fixture.v2.state_dir) && marker.is_file());

	// The same project is converted once; another project still converts.
	let again = fixture.project(&fixture.project);
	assert!(matches!(again.as_slice(), [ImportEntry {
		outcome: ImportOutcome::Skipped(SkipReason::MarkerPresent),
		..
	}]));
	let other = fixture.root.path().join("work/other");
	write(&other.join(".omp/commands/hello.md"), "Hello.\n");
	assert_eq!(listed(&fixture.project(&other)), [
		(ImportStep::SshHosts, None, OutcomeKind::NothingToImport),
		(ImportStep::Mcp, None, OutcomeKind::NothingToImport),
		(ImportStep::Commands, Some(".omp/prompts/hello.md"), OutcomeKind::Imported),
	]);
	// A project without v1 files is neither converted nor marked.
	let plain = fixture.root.path().join("work/plain");
	fs::create_dir_all(plain.join(".omp")).expect("plain project");
	assert_eq!(listed(&fixture.project(&plain)), [(
		ImportStep::SshHosts,
		None,
		OutcomeKind::NothingToImport
	)]);
	assert!(!project_assets_marker(&plain, &fixture.v2).exists());
}

#[test]
fn a_project_that_is_the_v1_home_is_never_written() {
	let fixture = Fixture::new();
	write(&fixture.omp.join("commands/stray.md"), "Stray.\n");
	write(&fixture.agent().join("AGENTS.md"), "Guidance.\n");
	let v1_before = snapshot(&fixture.omp);
	// Running from `$HOME` makes `~/.omp` look like the project's `.omp/`.
	let report = fixture.project(&fixture.home);

	assert_eq!(snapshot(&fixture.omp), v1_before);
	assert!(matches!(report.as_slice(), [ImportEntry {
		outcome: ImportOutcome::Skipped(SkipReason::InsideV1Root),
		..
	}]));
	assert!(!project_assets_marker(&fixture.home, &fixture.v2).exists());
}
