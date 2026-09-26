//! The `settings` step over temporary homes, v1 trees, projects, and v2
//! roots. Nothing here reads the process environment or the real `~/.omp` /
//! `~/.o2`.

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use super::{super::*, *};

const V1_CONFIG_YML: &str = concat!(
	"retry:\n",
	"  maxRetries: 5\n",
	"  baseDelayMs: 1500\n",
	"compaction:\n",
	"  thresholdPercent: 70\n",
	"modelRoles:\n",
	"  default: anthropic/claude-opus\n",
	"  smol: [openai/gpt-mini, google/flash]\n",
	"task:\n",
	"  agentIdleTtlMs: 0\n",
	"  maxRuntimeMs: 600000\n",
	"async:\n",
	"  maxJobs: 100\n",
	"memory:\n",
	"  backend: local\n",
	"hindsight:\n",
	"  apiUrl: https://hindsight.example\n",
	"  apiToken: hs-secret-token\n",
	"  recallMaxTokens: 900\n",
	"tier:\n",
	"  subagent: priority\n",
	"setupVersion: 3\n",
);

fn write(path: &Path, contents: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, contents).expect("write");
}

fn inputs(home: &Path) -> V1Inputs {
	V1Inputs { home: home.to_owned(), ..V1Inputs::default() }
}

fn roots(root: &Path) -> V2Roots {
	V2Roots {
		config_dir:     root.join("o2"),
		data_dir:       root.join("share/omp"),
		state_dir:      root.join("state/omp"),
		cache_dir:      root.join("cache/omp"),
		active_profile: None,
	}
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

fn offline() -> CredentialAccess<'static> {
	static CTX: std::sync::LazyLock<omp_con::Ctx> = std::sync::LazyLock::new(omp_con::Ctx::new);
	CredentialAccess::Offline(&CTX)
}

/// `(subject, kind)` of the settings entries.
fn settings(entries: &[ImportEntry]) -> Vec<(String, OutcomeKind)> {
	entries
		.iter()
		.filter(|entry| entry.step == ImportStep::Settings)
		.map(|entry| (entry.subject.as_deref().unwrap_or_default().to_owned(), entry.outcome.kind()))
		.collect()
}

fn kind_of(entries: &[(String, OutcomeKind)], subject: &str) -> OutcomeKind {
	entries
		.iter()
		.find(|(candidate, _)| candidate == subject)
		.unwrap_or_else(|| panic!("no entry for {subject}: {entries:#?}"))
		.1
}

#[test]
fn user_settings_import_once_into_config_cfg() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let omp = home.join(".omp");
	write(&omp.join("agent/config.yml"), V1_CONFIG_YML);
	let v2 = roots(root.path());
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");
	let before = snapshot(root.path());

	// A dry run reports everything and writes nothing anywhere.
	let dry = run(&pairs, ImportMode::DryRun, offline());
	assert_eq!(snapshot(root.path()), before, "a dry run must not write");
	let planned = settings(&dry.pairs[0].entries);
	assert_eq!(
		kind_of(&planned, "retry.baseDelayMs -> ai_retry_base_delay_ms"),
		OutcomeKind::WouldImport
	);

	let report = run(&pairs, ImportMode::Apply, offline());
	let entries = settings(&report.pairs[0].entries);
	for subject in [
		"retry.maxRetries -> ai_retry_max_retries",
		"retry.baseDelayMs -> ai_retry_base_delay_ms",
		"compaction.thresholdPercent -> ai_compact_threshold",
		"modelRoles -> ai_model_roles",
		"task.agentIdleTtlMs -> sv_task_agent_idle_ttl",
		"task.maxRuntimeMs -> sv_task_max_runtime",
		"memory.backend -> ai_memory_backend",
		"tier.subagent -> subagent.cfg",
	] {
		assert_eq!(kind_of(&entries, subject), OutcomeKind::Imported, "{subject}");
	}
	// Only what the user set, and only what differs from the v2 default.
	assert_eq!(kind_of(&entries, "async.maxJobs -> sv_async_max_jobs"), OutcomeKind::Skipped);
	for unmapped in
		["hindsight.apiUrl", "hindsight.apiToken", "hindsight.recallMaxTokens", "setupVersion"]
	{
		assert_eq!(kind_of(&entries, unmapped), OutcomeKind::NotMigratable, "{unmapped}");
	}

	let config = fs::read_to_string(v2.config_dir.join("config.cfg")).expect("config.cfg");
	let lines = config.lines().collect::<Vec<_>>();
	for line in [
		"ai_retry_max_retries 5",
		"ai_retry_base_delay_ms 1500",
		"ai_compact_threshold 0.7",
		"ai_model_roles {default anthropic/claude-opus smol openai/gpt-mini,google/flash}",
		"sv_task_agent_idle_ttl never",
		"sv_task_max_runtime 600000ms",
		"ai_memory_backend mnemopi",
		"// hindsight.apiUrl = \"https://hindsight.example\"",
		"// hindsight.apiToken = <redacted>",
		"// hindsight.recallMaxTokens = 900",
		"// setupVersion = 3",
	] {
		assert!(lines.contains(&line), "missing `{line}` in:\n{config}");
	}
	assert!(!config.contains("hs-secret-token"), "secrets are never copied:\n{config}");
	assert!(!config.contains("sv_async_max_jobs"), "defaults are not written:\n{config}");
	assert!(config.starts_with("// omp v1 settings imported from "), "{config}");
	assert!(config.contains("// v1 settings with no v2 equivalent (imported from "), "{config}");
	// The generated file replays through the console.
	let ctx = omp_con::Ctx::new();
	ctx.exec(&config, omp_con::Source::Config(Str::new_static("config.cfg")))
		.expect("the imported cfg executes");
	assert_eq!(ctx.value("ai_memory_backend").expect("var").to_string(), "mnemopi");
	// `tier.subagent` belongs to children (ADR 0013), clamped per family.
	let subagent = fs::read_to_string(v2.config_dir.join("subagent.cfg")).expect("subagent.cfg");
	for line in ["ai_tier_openai priority", "ai_tier_anthropic priority", "ai_tier_google priority"]
	{
		assert!(subagent.lines().any(|candidate| candidate == line), "{subagent}");
	}
	assert!(ImportStep::Settings.marker(&v2.config_dir).is_set());

	// Copy only: the v1 tree is exactly as it was.
	let v1_before = before
		.iter()
		.filter(|(path, _)| path.starts_with(&omp) && **path != omp)
		.map(|(path, bytes)| (path.clone(), bytes.clone()))
		.collect::<BTreeMap<_, _>>();
	assert_eq!(snapshot(&omp), v1_before);

	// A second run is a no-op through the marker, even after v1 changes.
	write(&omp.join("agent/config.yml"), "retry:\n  maxRetries: 9\n");
	let v2_before = snapshot(&v2.config_dir);
	let again = run(&pairs, ImportMode::Apply, offline());
	assert!(again.entries().any(|entry| {
		entry.step == ImportStep::Settings
			&& matches!(entry.outcome, ImportOutcome::Skipped(SkipReason::MarkerPresent))
	}));
	assert_eq!(snapshot(&v2.config_dir), v2_before);
}

#[test]
fn an_existing_config_cfg_keeps_its_lines_and_values() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	write(
		&home.join(".omp/agent/config.yml"),
		"retry:\n  maxRetries: 5\n  baseDelayMs: 1500\ntier:\n  subagent: flex\n",
	);
	let v2 = roots(root.path());
	let existing = "// mine\nai_retry_base_delay_ms 900\nunbindall\n";
	write(&v2.config_dir.join("config.cfg"), existing);
	write(&v2.config_dir.join("subagent.cfg"), "ai_tier_openai none\n");
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");

	let report = run(&pairs, ImportMode::Apply, offline());

	let entries = settings(&report.pairs[0].entries);
	assert_eq!(
		kind_of(&entries, "retry.baseDelayMs -> ai_retry_base_delay_ms"),
		OutcomeKind::Skipped
	);
	assert!(report.pairs[0].entries.iter().any(|entry| {
		entry.subject.as_deref() == Some("retry.baseDelayMs -> ai_retry_base_delay_ms")
			&& matches!(entry.outcome, ImportOutcome::Skipped(SkipReason::TargetExists))
	}));
	let config = fs::read_to_string(v2.config_dir.join("config.cfg")).expect("config.cfg");
	// Existing bytes are untouched, the import is appended after them.
	assert!(config.starts_with(existing), "{config}");
	assert!(config.contains("\nai_retry_max_retries 5\n"), "{config}");
	assert!(!config.contains("ai_retry_base_delay_ms 1500"), "the existing value wins");
	// Only families the user has not pinned in subagent.cfg are appended.
	let subagent = fs::read_to_string(v2.config_dir.join("subagent.cfg")).expect("subagent.cfg");
	assert!(subagent.starts_with("ai_tier_openai none\n"), "{subagent}");
	assert!(subagent.contains("ai_tier_google flex"), "{subagent}");
	assert!(subagent.contains("ai_tier_anthropic none"), "anthropic cannot realize flex");
	assert!(!subagent.contains("ai_tier_openai flex"), "{subagent}");
}

#[test]
fn a_named_profile_imports_into_its_namesake() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	// Only the `work` profile has settings, spelled `config.yaml`.
	write(&home.join(".omp/profiles/work/agent/config.yaml"), "retry:\n  maxRetries: 7\n");
	let v2 = roots(root.path());
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");

	let report = run(&pairs, ImportMode::Apply, offline());

	assert_eq!(settings(&report.pairs[0].entries), [(String::new(), OutcomeKind::NothingToImport)]);
	assert!(!v2.config_dir.join("config.cfg").exists(), "the default profile gets nothing");
	assert!(ImportStep::Settings.marker(&v2.config_dir).is_set(), "nothing to import still marks");
	let work = v2.config_dir.join("profiles/work");
	let config = fs::read_to_string(work.join("config.cfg")).expect("work config.cfg");
	assert!(config.contains("\nai_retry_max_retries 7\n"), "{config}");
	assert!(ImportStep::Settings.marker(&work).is_set());
}

#[test]
fn project_settings_import_once_per_project() {
	let root = tempfile::tempdir().expect("scratch");
	let v2 = roots(root.path());
	let project = root.path().join("repo");
	let v1_file = project.join(".omp/config.yml");
	write(&v1_file, "compaction:\n  thresholdPercent: 55\nsharpshooter:\n  model: x\n");
	let before = snapshot(root.path());

	let dry = import_project_settings(&project, &v2, ImportMode::DryRun);
	assert_eq!(snapshot(root.path()), before, "a dry run must not write");
	assert_eq!(
		kind_of(&settings(&dry), "compaction.thresholdPercent -> ai_compact_threshold"),
		OutcomeKind::WouldImport
	);

	let applied = import_project_settings(&project, &v2, ImportMode::Apply);
	assert_eq!(
		kind_of(&settings(&applied), "compaction.thresholdPercent -> ai_compact_threshold"),
		OutcomeKind::Imported
	);
	assert_eq!(kind_of(&settings(&applied), "sharpshooter.model"), OutcomeKind::NotMigratable);
	let config = fs::read_to_string(project.join(".omp/config.cfg")).expect("project cfg");
	assert!(config.contains("\nai_compact_threshold 0.55\n"), "{config}");
	assert!(config.contains("\n// sharpshooter.model = \"x\"\n"), "{config}");
	assert_eq!(fs::read(&v1_file).expect("v1 file"), before[&v1_file].clone().expect("bytes"));
	// Nothing lands in the user roots, and the marker lives outside the repo.
	assert!(!v2.config_dir.exists());
	let marker = project_marker(&project, &v2);
	assert!(marker.starts_with(&v2.state_dir) && marker.is_file());

	// Once per project: a second run leaves everything as it is.
	let after = snapshot(root.path());
	let again = import_project_settings(&project, &v2, ImportMode::Apply);
	assert!(matches!(again[0].outcome, ImportOutcome::Skipped(SkipReason::MarkerPresent)));
	assert_eq!(snapshot(root.path()), after);

	// Another project imports on its own, and one without v1 settings gets
	// no marker.
	let other = root.path().join("other");
	write(&other.join(".omp/config.yml"), "compaction:\n  thresholdPercent: 60\n");
	let imported = import_project_settings(&other, &v2, ImportMode::Apply);
	assert_eq!(imported[0].outcome.kind(), OutcomeKind::Imported);
	let bare = root.path().join("bare");
	fs::create_dir_all(&bare).expect("bare project");
	let nothing = import_project_settings(&bare, &v2, ImportMode::Apply);
	assert_eq!(nothing[0].outcome.kind(), OutcomeKind::NothingToImport);
	assert!(!project_marker(&bare, &v2).exists());
}

#[test]
fn a_dropped_memory_backend_warns_and_keeps_its_settings_as_comments() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	write(
		&home.join(".omp/agent/config.yml"),
		"memory:\n  backend: hindsight\nhindsight:\n  bankId: team\n",
	);
	let v2 = roots(root.path());
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");

	let report = run(&pairs, ImportMode::Apply, offline());

	assert!(report.pairs[0].entries.iter().any(|entry| {
		entry.subject.as_deref() == Some("memory.backend = hindsight")
			&& matches!(entry.outcome, ImportOutcome::NeedsAttention(Attention::MemoryBackendDropped))
	}));
	let entries = settings(&report.pairs[0].entries);
	assert_eq!(kind_of(&entries, "hindsight.bankId"), OutcomeKind::NotMigratable);
	let config = fs::read_to_string(v2.config_dir.join("config.cfg")).expect("config.cfg");
	assert!(!config.contains("ai_memory_backend"), "{config}");
	assert!(config.contains("// memory.backend = \"hindsight\"\n"), "{config}");
	assert!(config.contains("// hindsight.bankId = \"team\"\n"), "{config}");
}

#[test]
fn a_rejected_value_is_reported_and_commented() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	write(
		&home.join(".omp/agent/config.yml"),
		"retry:\n  baseDelayMs: soon\nimages:\n  urls:\n    credentials:\n      host: {password: \
		 hunter2}\n",
	);
	let v2 = roots(root.path());
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");

	let report = run(&pairs, ImportMode::Apply, offline());

	assert!(report.pairs[0].entries.iter().any(|entry| {
		entry.subject.as_deref() == Some("retry.baseDelayMs -> ai_retry_base_delay_ms")
			&& matches!(entry.outcome, ImportOutcome::NotMigratable(NotMigratable::ValueRejected))
	}));
	// A record is one key; its secrets are redacted.
	let entries = settings(&report.pairs[0].entries);
	assert_eq!(kind_of(&entries, "images.urls.credentials"), OutcomeKind::NotMigratable);
	let config = fs::read_to_string(v2.config_dir.join("config.cfg")).expect("config.cfg");
	assert!(config.contains("// retry.baseDelayMs = \"soon\"\n"), "{config}");
	assert!(config.contains("// images.urls.credentials = <redacted>\n"), "{config}");
	assert!(!config.contains("hunter2"), "{config}");
}

#[test]
fn secret_keys_are_recognized_by_word() {
	for key in ["apiToken", "llmApiKey", "basicPassword", "credentials", "token", "URLToken"] {
		assert!(is_secret(key), "{key}");
	}
	for key in ["recallMaxTokens", "keepRecentTokens", "bankId", "apiUrl", "keybindings"] {
		assert!(!is_secret(key), "{key}");
	}
	let mut rendered = String::new();
	let value = legacy_settings::read_yaml_document(Path::new("/nonexistent")).expect("empty");
	assert!(value.is_empty());
	render(
		&mut rendered,
		&LegacyValue::Map(
			serde_yaml::from_str::<LegacyMap>("url: \"a\\nb\"\napiKey: sk\n").expect("yaml"),
		),
		false,
	);
	assert_eq!(rendered, r#"{"url": "a\nb", "apiKey": <redacted>}"#);
}
