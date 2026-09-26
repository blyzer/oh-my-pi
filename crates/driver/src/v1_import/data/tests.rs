//! Data and memory step proofs over temporary homes, v1 trees, projects, and
//! v2 roots. Nothing here reads the process environment or the real `~/.omp`
//! / `~/.o2`.

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use omp_memory::{
	BankId, BankScope, BankScopeInput, MemoryBackend, MnemopiSettings, bank::database_path,
	config::BankScoping, recall::RecallBounds, store::BankStore,
};
use rusqlite::Connection;

use crate::v1_import::{
	Attention, CredentialAccess, ImportEntry, ImportMode, ImportOutcome, ImportReport, ImportStep,
	OutcomeKind, ProfileSelection, SkipReason, V1Inputs, V1Source, V2Roots, plan, run,
};

/// The steps this module registers, in run order.
const DATA_STEPS: [ImportStep; 5] = [
	ImportStep::History,
	ImportStep::InstallId,
	ImportStep::Mnemopi,
	ImportStep::LearnedLessons,
	ImportStep::Marketplace,
];

/// v1's `history-storage.ts` schema.
const V1_HISTORY_DDL: &str = "
CREATE TABLE history (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 prompt TEXT NOT NULL UNIQUE,
 created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER)),
 cwd TEXT,
 session_id TEXT
);
CREATE INDEX idx_history_created_at ON history(created_at DESC);
CREATE VIRTUAL TABLE history_fts USING fts5(prompt, content='history', content_rowid='id');
CREATE TRIGGER history_ai AFTER INSERT ON history BEGIN
 INSERT INTO history_fts(rowid, prompt) VALUES (new.id, new.prompt);
END;
PRAGMA user_version = 1;
";

/// `crates/chat/src/history.rs`'s schema, as v2 leaves it after an open.
const V2_HISTORY_DDL: &str = "
CREATE TABLE history (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 prompt TEXT NOT NULL UNIQUE,
 created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER)),
 cwd TEXT,
 session_id TEXT
);
CREATE INDEX idx_history_created_at ON history(created_at DESC, id DESC);
CREATE VIRTUAL TABLE history_fts USING fts5(prompt, content='history', content_rowid='id');
CREATE TRIGGER history_ai AFTER INSERT ON history BEGIN
 INSERT INTO history_fts(rowid, prompt) VALUES (new.id, new.prompt);
END;
CREATE TRIGGER history_ad AFTER DELETE ON history BEGIN
 INSERT INTO history_fts(history_fts, rowid, prompt) VALUES ('delete', old.id, old.prompt);
END;
CREATE TRIGGER history_au AFTER UPDATE ON history BEGIN
 INSERT INTO history_fts(history_fts, rowid, prompt) VALUES ('delete', old.id, old.prompt);
 INSERT INTO history_fts(rowid, prompt) VALUES (new.id, new.prompt);
END;
PRAGMA user_version = 2;
";

/// The tables v1 Mnemopi's `initBeam` creates (`packages/mnemopi`), with v1's
/// column set and index tables.
const V1_MNEMOPI_DDL: &str = "
CREATE TABLE working_memory (
 id TEXT PRIMARY KEY, content TEXT NOT NULL, embed_text TEXT DEFAULT NULL, source TEXT,
 timestamp TEXT, session_id TEXT DEFAULT 'default', importance REAL DEFAULT 0.5,
 metadata_json TEXT, veracity TEXT DEFAULT 'unknown', memory_type TEXT DEFAULT 'unknown',
 consolidated_at TEXT, recall_count INTEGER DEFAULT 0, last_recalled TIMESTAMP DEFAULT NULL,
 valid_until TIMESTAMP DEFAULT NULL, superseded_by TEXT DEFAULT NULL,
 scope TEXT DEFAULT 'global', author_id TEXT DEFAULT NULL, author_type TEXT DEFAULT NULL,
 channel_id TEXT DEFAULT NULL, trust_tier TEXT DEFAULT 'STATED', validator TEXT DEFAULT NULL,
 validated_at TIMESTAMP DEFAULT NULL, validation_count INTEGER DEFAULT 0,
 event_date TEXT DEFAULT NULL, event_date_precision TEXT DEFAULT 'unknown',
 temporal_tags TEXT DEFAULT '[]', corrected_by INTEGER DEFAULT NULL,
 created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE episodic_memory (
 rowid INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, content TEXT NOT NULL,
 source TEXT, timestamp TEXT, session_id TEXT DEFAULT 'default', importance REAL DEFAULT 0.5,
 metadata_json TEXT, summary_of TEXT DEFAULT '', veracity TEXT DEFAULT 'unknown',
 tier INTEGER DEFAULT 1, degraded_at TEXT, memory_type TEXT DEFAULT 'unknown',
 binary_vector BLOB, recall_count INTEGER DEFAULT 0, last_recalled TIMESTAMP DEFAULT NULL,
 valid_until TIMESTAMP DEFAULT NULL, superseded_by TEXT DEFAULT NULL,
 scope TEXT DEFAULT 'global', author_id TEXT DEFAULT NULL, author_type TEXT DEFAULT NULL,
 channel_id TEXT DEFAULT NULL, trust_tier TEXT DEFAULT 'STATED', validator TEXT DEFAULT NULL,
 validated_at TIMESTAMP DEFAULT NULL, validation_count INTEGER DEFAULT 0,
 event_date TEXT DEFAULT NULL, event_date_precision TEXT DEFAULT 'unknown',
 temporal_tags TEXT DEFAULT '[]', corrected_by INTEGER DEFAULT NULL,
 created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE scratchpad (
 id TEXT PRIMARY KEY, content TEXT NOT NULL, session_id TEXT DEFAULT 'default',
 created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE VIRTUAL TABLE fts_episodes USING fts5(content, content='episodic_memory', \
                              content_rowid='rowid');
CREATE VIRTUAL TABLE fts_working USING fts5(id UNINDEXED, content);
CREATE TRIGGER em_ai AFTER INSERT ON episodic_memory BEGIN
 INSERT INTO fts_episodes(rowid, content) VALUES (new.rowid, new.content);
END;
";

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

/// Every v1 profile under `source` into its v2 namesake.
fn import(v2: &V2Roots, source: V1Inputs, mode: ImportMode) -> ImportReport {
	let pairs = plan(&V1Source::new(source), v2, &ProfileSelection::All).expect("plan");
	run(&pairs, mode, CredentialAccess::Offline(&omp_con::Ctx::new()))
}

fn step_entries(report: &ImportReport, step: ImportStep) -> Vec<&ImportEntry> {
	report
		.entries()
		.filter(|entry| entry.step == step)
		.collect()
}

/// `(kind, subject)` of every entry `step` produced.
fn summary(report: &ImportReport, step: ImportStep) -> Vec<(OutcomeKind, Option<String>)> {
	step_entries(report, step)
		.into_iter()
		.map(|entry| (entry.outcome.kind(), entry.subject.as_deref().map(str::to_owned)))
		.collect()
}

#[expect(clippy::unnecessary_wraps, reason = "builds an expected `Option` report subject")]
fn owned(text: &str) -> Option<String> {
	Some(text.to_owned())
}

#[expect(clippy::unnecessary_wraps, reason = "builds an expected `Option` report subject")]
fn shown(path: &Path) -> Option<String> {
	Some(path.display().to_string())
}

/// A canonical project directory under the scratch root.
fn project(root: &Path, name: &str) -> PathBuf {
	let path = root.join("projects").join(name);
	fs::create_dir_all(&path).expect("project");
	fs::canonicalize(path).expect("canonical project")
}

/// v1's `encodeProjectPath`, without the `--` delimiters.
fn encode_body(project: &Path) -> String {
	let text = project.to_str().expect("utf-8 path");
	let text = text.strip_prefix(['/', '\\']).unwrap_or(text);
	text.replace(['/', '\\', ':'], "-")
}

/// v1's `encodeProjectPath`.
fn encode(project: &Path) -> String {
	format!("--{}--", encode_body(project))
}

fn v1_history(path: &Path, prompts: &[(&str, i64, &str)]) -> Connection {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	let connection = Connection::open(path).expect("v1 history");
	connection.execute_batch(V1_HISTORY_DDL).expect("v1 schema");
	for (prompt, created_at, cwd) in prompts {
		connection
			.execute(
				"INSERT INTO history(prompt, created_at, cwd, session_id) VALUES (?1, ?2, ?3, 's1')",
				(prompt, created_at, cwd),
			)
			.expect("v1 row");
	}
	connection
}

fn fts_matches(connection: &Connection, query: &str) -> Vec<String> {
	let mut statement = connection
		.prepare(
			"SELECT h.prompt FROM history_fts f JOIN history h ON h.id = f.rowid
			 WHERE history_fts MATCH ?1 ORDER BY h.prompt",
		)
		.expect("fts query");
	statement
		.query_map([query], |row| row.get::<_, String>(0))
		.expect("fts rows")
		.collect::<Result<_, _>>()
		.expect("fts")
}

#[test]
fn history_merges_into_v2_and_stays_searchable() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	let v1_db = home.join(".omp/agent/history.db");
	// A running v1 still holds its last prompt in the WAL: it must be
	// imported without SQLite ever writing beside the v1 database.
	let live =
		v1_history(&v1_db, &[("git commit the fix", 10, "/v1"), ("shared prompt", 20, "/v1")]);
	live
		.pragma_update(None, "journal_mode", "WAL")
		.expect("wal");
	live
		.pragma_update(None, "wal_autocheckpoint", 0)
		.expect("no checkpoint");
	live
		.execute(
			"INSERT INTO history(prompt, created_at, cwd) VALUES ('deploy staging', 30, '/v1')",
			[],
		)
		.expect("wal row");
	assert!(
		fs::metadata(home.join(".omp/agent/history.db-wal"))
			.expect("wal")
			.len() > 0
	);
	let v2_db = v2.data_dir.join("history.db");
	fs::create_dir_all(&v2.data_dir).expect("v2 data");
	let v2_connection = Connection::open(&v2_db).expect("v2 history");
	v2_connection
		.execute_batch(V2_HISTORY_DDL)
		.expect("v2 schema");
	v2_connection
		.execute_batch(
			"INSERT INTO history(prompt, created_at, cwd) VALUES ('shared prompt', 99, '/v2');
			 INSERT INTO history(prompt, created_at, cwd) VALUES ('v2 only', 98, '/v2');",
		)
		.expect("v2 rows");
	drop(v2_connection);
	let v1_before = snapshot(&home.join(".omp"));

	let dry = import(&v2, inputs(&home), ImportMode::DryRun);
	assert_eq!(summary(&dry, ImportStep::History), [
		(OutcomeKind::WouldImport, owned("2 prompts")),
		(OutcomeKind::Skipped, owned("1 prompt already in v2")),
	]);
	let report = import(&v2, inputs(&home), ImportMode::Apply);

	assert_eq!(summary(&report, ImportStep::History), [
		(OutcomeKind::Imported, owned("2 prompts")),
		(OutcomeKind::Skipped, owned("1 prompt already in v2")),
	]);
	assert_eq!(snapshot(&home.join(".omp")), v1_before, "the v1 database and its WAL are untouched");
	let merged = Connection::open(&v2_db).expect("merged");
	let rows = merged
		.prepare("SELECT prompt, created_at, cwd FROM history ORDER BY prompt")
		.expect("rows")
		.query_map([], |row| {
			Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
		})
		.expect("rows")
		.collect::<Result<Vec<_>, _>>()
		.expect("rows");
	let row = |prompt: &str, created: i64, cwd: &str| (prompt.to_owned(), created, cwd.to_owned());
	assert_eq!(rows, [
		row("deploy staging", 30, "/v1"),
		row("git commit the fix", 10, "/v1"),
		// v2's copy of a shared prompt, and its provenance, win.
		row("shared prompt", 99, "/v2"),
		row("v2 only", 98, "/v2"),
	]);
	assert_eq!(fts_matches(&merged, "deploy*"), ["deploy staging"]);
	assert_eq!(fts_matches(&merged, "commit"), ["git commit the fix"]);
	assert_eq!(fts_matches(&merged, "prompt"), ["shared prompt"]);
	merged
		.execute("INSERT INTO history_fts(history_fts) VALUES('integrity-check')", [])
		.expect("the index matches the table");
	drop(live);
}

#[test]
fn history_without_a_v2_database_is_copied_whole() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	drop(v1_history(&home.join(".omp/agent/history.db"), &[
		("first", 1, "/a"),
		("second", 2, "/b"),
	]));

	let report = import(&v2, inputs(&home), ImportMode::Apply);

	assert_eq!(summary(&report, ImportStep::History), [(OutcomeKind::Imported, owned("2 prompts"))]);
	let copy = Connection::open(v2.data_dir.join("history.db")).expect("copy");
	// The v1 `user_version` survives, so v2's open runs its rebuild migration.
	let version: i64 = copy
		.pragma_query_value(None, "user_version", |row| row.get(0))
		.expect("version");
	assert_eq!(version, 1);
	assert_eq!(fts_matches(&copy, "sec*"), ["second"]);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_history_db_shared_through_xdg_is_skipped() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	fs::create_dir_all(home.join(".omp")).expect("v1 root");
	// v1 relocated to `$XDG_DATA_HOME/omp`, which is v2's data directory.
	drop(v1_history(&v2.data_dir.join("history.db"), &[("only", 1, "/a")]));
	let source = V1Inputs { xdg_data_home: Some(root.path().join("share")), ..inputs(&home) };
	let before = snapshot(&v2.data_dir);

	let report = import(&v2, source, ImportMode::Apply);

	let history = step_entries(&report, ImportStep::History);
	assert_eq!(history.len(), 1);
	assert!(matches!(history[0].outcome, ImportOutcome::Skipped(SkipReason::SharedWithV2)));
	assert_eq!(snapshot(&v2.data_dir), before);
	assert!(ImportStep::History.marker(&v2.config_dir).is_set());
}

#[test]
fn install_id_is_copied_only_when_v2_has_none() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	write(&home.join(".omp/install-id"), "6f1c2a4e-0000-4000-8000-000000000001\n");
	fs::create_dir_all(home.join(".omp/profiles/work/agent")).expect("work profile");
	write(&v2.data_dir.join("profiles/work/install-id"), "v2-own-id");

	let report = import(&v2, inputs(&home), ImportMode::Apply);

	assert_eq!(summary(&report, ImportStep::InstallId), [
		(OutcomeKind::Imported, None),
		(OutcomeKind::Skipped, owned("v2 keeps its own, different id")),
	]);
	assert_eq!(
		fs::read_to_string(v2.data_dir.join("install-id")).expect("id"),
		"6f1c2a4e-0000-4000-8000-000000000001"
	);
	assert_eq!(
		fs::read_to_string(v2.data_dir.join("profiles/work/install-id")).expect("id"),
		"v2-own-id"
	);
}

fn v1_bank(path: &Path, rows: &[(&str, &Path)]) {
	fs::create_dir_all(path.parent().expect("parent")).expect("bank dir");
	let connection = Connection::open(path).expect("v1 bank");
	connection.execute_batch(V1_MNEMOPI_DDL).expect("v1 schema");
	for (index, (content, cwd)) in rows.iter().enumerate() {
		let metadata = serde_json::json!({ "cwd": cwd, "operation": "memory.save" }).to_string();
		connection
			.execute(
				"INSERT INTO working_memory(id, content, source, timestamp, session_id, metadata_json)
				 VALUES (?1, ?2, 'coding-agent-learn', '2026-01-01T00:00:00Z', 'v1', ?3)",
				(format!("wm-{index}"), content, metadata),
			)
			.expect("v1 row");
	}
}

/// The directory a default-settings v2 session in `project` keeps banks in.
fn bank_dir(v2: &V2Roots, project: &Path) -> PathBuf {
	let state = omp_env::project_state::directory(&v2.data_dir, project).expect("state");
	omp_memory::runtime::database_dir(
		&omp_envd::memory::memory_data_dir(&state),
		&MnemopiSettings::default(),
	)
}

/// A Mnemopi runtime composed as a default-settings session in `project`.
fn session_memory(
	v2: &V2Roots,
	project: &Path,
	session: &str,
) -> omp_envd::memory::RegisteredMemoryRuntime {
	let state = omp_env::project_state::directory(&v2.data_dir, project).expect("state");
	let snapshot = omp_envd::vcs::RepositorySnapshot {
		availability:  omp_envd::vcs::RepositoryAvailability::NotRepository,
		worktree_root: None,
		primary_root:  None,
		head:          None,
		branch:        None,
		status_counts: Default::default(),
	};
	omp_envd::memory::start(
		MemoryBackend::Mnemopi,
		&MnemopiSettings::default(),
		&state,
		session,
		project.to_owned(),
		Some(&snapshot),
	)
	.expect("memory runtime")
}

fn recalled(runtime: &omp_memory::MemoryRuntime, query: &str) -> Vec<String> {
	let bounds = RecallBounds { limit: 50, token_budget: 32 * 1024, voice_limit: 100 };
	let mut contents = runtime
		.search(query, None, bounds)
		.expect("search")
		.items
		.into_iter()
		.map(|item| item.memory.content.as_str().to_owned())
		.collect::<Vec<_>>();
	contents.sort();
	contents
}

#[test]
fn a_v1_mnemopi_store_is_copied_where_v2_recalls_it() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	let app = project(root.path(), "app");
	let other = project(root.path(), "other");
	let gone = root.path().join("projects/gone");
	let mnemopi = home.join(".omp/agent/memories/mnemopi");
	v1_bank(&mnemopi.join("mnemopi.db"), &[("shared fact", Path::new("/anywhere"))]);
	v1_bank(&mnemopi.join("banks/app-1x2y/mnemopi.db"), &[
		("the app builds with bazel", &app),
		("the app deploys on fridays", &app),
	]);
	v1_bank(&mnemopi.join("banks/mixed-9z/mnemopi.db"), &[("fact one", &app), ("fact two", &other)]);
	v1_bank(&mnemopi.join("banks/gone-7q/mnemopi.db"), &[("lost", &gone)]);
	let v1_before = snapshot(&home);

	let report = import(&v2, inputs(&home), ImportMode::Apply);

	let unscoped = v2.data_dir.join("mnemopi");
	let app_bank = bank_dir(&v2, &app).join("banks/app-1x2y/mnemopi.db");
	assert_eq!(summary(&report, ImportStep::Mnemopi), [
		(OutcomeKind::Imported, shown(&unscoped.join("mnemopi.db"))),
		(OutcomeKind::NeedsAttention, shown(&unscoped.join("mnemopi.db"))),
		(OutcomeKind::Imported, shown(&app_bank)),
		(OutcomeKind::Skipped, shown(&gone)),
		(OutcomeKind::Imported, shown(&unscoped.join("banks/mixed-9z/mnemopi.db"))),
		(OutcomeKind::NeedsAttention, shown(&unscoped.join("banks/mixed-9z/mnemopi.db"))),
	]);
	let entries = step_entries(&report, ImportStep::Mnemopi);
	assert!(matches!(
		entries[1].outcome,
		ImportOutcome::NeedsAttention(Attention::MnemopiStoreUnscoped)
	));
	assert!(matches!(entries[3].outcome, ImportOutcome::Skipped(SkipReason::ProjectMissing)));
	assert_eq!(snapshot(&home), v1_before);

	// v2's store opens the copied v1-shaped bank, migrating its schema.
	let bank = BankId::configured("app-1x2y").expect("bank");
	let store = BankStore::open(&app_bank, bank, &app).expect("v2 opens the v1 bank");
	let mut contents = store
		.list(10)
		.expect("list")
		.into_iter()
		.map(|record| record.content.as_str().to_owned())
		.collect::<Vec<_>>();
	contents.sort();
	assert_eq!(contents, ["the app builds with bazel", "the app deploys on fridays"]);
	drop(store);
	// A session in that project recalls it through legacy-bank adoption.
	let session = session_memory(&v2, &app, "after-import");
	assert_eq!(recalled(session.runtime(), "bazel"), ["the app builds with bazel"]);
	drop(session);

	// Without the marker, every store v2 now has is kept, never replaced.
	fs::remove_file(ImportStep::Mnemopi.marker(&v2.config_dir).path()).expect("marker");
	let again = import(&v2, inputs(&home), ImportMode::DryRun);
	assert_eq!(
		step_entries(&again, ImportStep::Mnemopi)
			.iter()
			.map(|entry| match entry.outcome {
				ImportOutcome::Skipped(reason) => reason,
				_ => panic!("{entry:?} is not a skip"),
			})
			.collect::<Vec<_>>(),
		[
			SkipReason::TargetExists,
			SkipReason::TargetExists,
			SkipReason::ProjectMissing,
			SkipReason::TargetExists,
		]
	);
}

#[test]
fn learned_lessons_land_in_the_project_bank_through_the_learn_path() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	let app = project(root.path(), "my-app");
	let memories = home.join(".omp/agent/memories");
	write(
		&memories.join(encode(&app)).join("learned.md"),
		"# Learned\n- Run `just fmt` before committing _(context: CI rejected a push)_\n- Prefer \
		 nextest over cargo test\n- The API rate-limits at 10 rps\n- Prefer nextest over cargo \
		 test\n",
	);
	let missing = root.path().join("projects/deleted-app");
	write(&memories.join(encode(&missing)).join("learned.md"), "- orphaned lesson\n");
	// v2 already learned one of them in an earlier session.
	let earlier = session_memory(&v2, &app, "earlier-session");
	omp_tools::learn::retain_lesson(earlier.runtime(), "The API rate-limits at 10 rps", None)
		.expect("earlier lesson");
	drop(earlier);
	let guess = format!("/{}", encode_body(&missing).replace('-', "/"));
	let app_label = app.display().to_string();

	let dry = import(&v2, inputs(&home), ImportMode::DryRun);
	assert_eq!(summary(&dry, ImportStep::LearnedLessons), [
		(OutcomeKind::Skipped, Some(guess.clone())),
		(OutcomeKind::WouldImport, Some(format!("{app_label}: 4 lessons"))),
		(OutcomeKind::NeedsAttention, owned("4 imported lessons")),
	]);
	let report = import(&v2, inputs(&home), ImportMode::Apply);

	assert_eq!(summary(&report, ImportStep::LearnedLessons), [
		(OutcomeKind::Skipped, Some(guess)),
		(OutcomeKind::Imported, Some(format!("{app_label}: 2 lessons"))),
		(OutcomeKind::Skipped, Some(format!("{app_label}: 2 lessons already stored"))),
		(OutcomeKind::NeedsAttention, owned("2 imported lessons")),
	]);
	let entries = step_entries(&report, ImportStep::LearnedLessons);
	assert!(matches!(entries[0].outcome, ImportOutcome::Skipped(SkipReason::ProjectMissing)));
	assert!(matches!(entries[3].outcome, ImportOutcome::NeedsAttention(Attention::EnableMnemopi)));

	// They are `learn` records in the bank a session in that project writes:
	// `BankScope::resolve` over its canonical path, per project.
	let scope = BankScope::resolve(BankScopeInput {
		canonical_primary_root: None,
		workspace_root:         &app,
		configured_bank:        None,
		scoping:                BankScoping::PerProject,
	})
	.expect("scope");
	let bank_path = database_path(&bank_dir(&v2, &app), &scope.global, &scope.retain);
	let store = BankStore::open(&bank_path, scope.retain, &app).expect("project bank");
	let mut records = store
		.list(50)
		.expect("records")
		.into_iter()
		.map(|record| {
			(
				record.content.as_str().to_owned(),
				record.source.as_deref().map(str::to_owned),
				record.metadata["context"].as_str().map(str::to_owned),
			)
		})
		.collect::<Vec<_>>();
	records.sort();
	let learn = owned("coding-agent-learn");
	assert_eq!(records, [
		("Prefer nextest over cargo test".to_owned(), learn.clone(), None),
		("Run `just fmt` before committing".to_owned(), learn.clone(), owned("CI rejected a push")),
		("The API rate-limits at 10 rps".to_owned(), learn, None),
	]);
}

#[test]
fn lessons_need_no_hint_when_the_profile_runs_mnemopi() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	let app = project(root.path(), "svc");
	write(
		&home
			.join(".omp/agent/memories")
			.join(encode(&app))
			.join("learned.md"),
		"- one\n",
	);
	write(&v2.config_dir.join("config.cfg"), "ai_memory_backend mnemopi\n");

	let report = import(&v2, inputs(&home), ImportMode::Apply);

	assert_eq!(summary(&report, ImportStep::LearnedLessons), [(
		OutcomeKind::Imported,
		Some(format!("{}: 1 lesson", app.display()))
	)]);
}

/// Owner decision: v1's `local` backend wrote the lessons, so the settings
/// step (which runs first) turns Mnemopi on and the lessons need no hint.
#[test]
fn a_v1_local_backend_turns_mnemopi_on_for_the_imported_lessons() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	let app = project(root.path(), "svc");
	write(
		&home
			.join(".omp/agent/memories")
			.join(encode(&app))
			.join("learned.md"),
		"- one\n- two\n",
	);
	write(&home.join(".omp/agent/config.yml"), "memory:\n  backend: local\n");
	let steps = ImportStep::registered().collect::<Vec<_>>();
	let position = |step| steps.iter().position(|registered| *registered == step);
	assert!(position(ImportStep::Settings) < position(ImportStep::LearnedLessons));

	let report = import(&v2, inputs(&home), ImportMode::Apply);

	let config = fs::read_to_string(v2.config_dir.join("config.cfg")).expect("config.cfg");
	assert!(
		config
			.lines()
			.any(|line| line.split_whitespace().eq(["ai_memory_backend", "mnemopi"])),
		"{config}"
	);
	assert_eq!(summary(&report, ImportStep::LearnedLessons), [(
		OutcomeKind::Imported,
		Some(format!("{}: 2 lessons", app.display()))
	)]);
}

#[test]
fn the_marketplace_registry_and_cache_merge_into_v2() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path());
	let omp = home.join(".omp");
	let v1_cache = omp.join("plugins/cache");
	let v2_plugins = v2.data_dir.join("plugins");
	write(
		&omp.join("marketplaces.json"),
		&serde_json::json!({ "version": 1, "marketplaces": [
			{ "name": "acme", "sourceType": "github", "sourceUri": "acme/plugins",
			  "catalogPath": v1_cache.join("marketplaces/acme/.claude-plugin/marketplace.json"),
			  "addedAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z" },
			{ "name": "shared", "sourceType": "url", "sourceUri": "https://v1.example/m.json",
			  "catalogPath": v1_cache.join("marketplaces/shared/marketplace.json"),
			  "addedAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z" },
		]})
		.to_string(),
	);
	write(
		&omp.join("plugins/installed_plugins.json"),
		&serde_json::json!({ "version": 2, "plugins": {
			"lint@acme": [{ "scope": "user",
			  "installPath": v1_cache.join("plugins/acme___lint___1.0.0"),
			  "version": "1.0.0", "installedAt": "2026-01-03T00:00:00Z",
			  "lastUpdated": "2026-01-03T00:00:00Z", "gitCommitSha": "abc123" }],
			"fmt@shared": [{ "scope": "user",
			  "installPath": v1_cache.join("plugins/shared___fmt___2.0.0"),
			  "version": "2.0.0", "installedAt": "2026-01-03T00:00:00Z",
			  "lastUpdated": "2026-01-03T00:00:00Z", "enabled": false }],
		}})
		.to_string(),
	);
	write(&v1_cache.join("marketplaces/acme/.claude-plugin/marketplace.json"), "{}");
	write(&v1_cache.join("plugins/acme___lint___1.0.0/skills/lint/SKILL.md"), "# lint");
	write(&v1_cache.join("plugins/shared___fmt___2.0.0/v1.txt"), "v1");
	write(&v1_cache.join("plugins/.tmp-01J/partial"), "x");
	write(&omp.join("plugins/package.json"), "{}");
	write(&omp.join("stats.db"), "stats");
	fs::create_dir_all(omp.join("agent")).expect("agent dir");
	let agent_db = Connection::open(omp.join("agent/agent.db")).expect("agent.db");
	agent_db
		.execute_batch(
			"CREATE TABLE model_usage (model TEXT, count INTEGER);
			 INSERT INTO model_usage VALUES ('m', 3);
			 CREATE TABLE command_usage (command TEXT, count INTEGER);",
		)
		.expect("usage tables");
	drop(agent_db);
	// v2 already has the `shared` marketplace, `fmt@shared`, and its cache.
	write(
		&v2.data_dir.join("marketplaces.json"),
		&serde_json::json!({ "version": 1, "marketplaces": [
			{ "name": "shared", "sourceType": "url", "sourceUri": "https://v2.example/m.json",
			  "catalogPath": v2_plugins.join("cache/marketplaces/shared/marketplace.json"),
			  "addedAt": "2026-02-01T00:00:00Z", "updatedAt": "2026-02-01T00:00:00Z" },
		]})
		.to_string(),
	);
	write(
		&v2_plugins.join("installed_plugins.json"),
		&serde_json::json!({ "version": 2, "plugins": {
			"fmt@shared": [{ "scope": "user",
			  "installPath": v2_plugins.join("cache/plugins/shared___fmt___3.0.0"),
			  "version": "3.0.0", "installedAt": "2026-02-01T00:00:00Z",
			  "lastUpdated": "2026-02-01T00:00:00Z", "enabled": true }],
		}})
		.to_string(),
	);
	write(&v2_plugins.join("cache/plugins/shared___fmt___2.0.0/v2.txt"), "v2");
	let v1_before = snapshot(&omp);

	let report = import(&v2, inputs(&home), ImportMode::Apply);

	assert_eq!(summary(&report, ImportStep::Marketplace), [
		(OutcomeKind::Imported, owned("1 marketplace")),
		(OutcomeKind::Skipped, owned("1 marketplace v2 already had")),
		(OutcomeKind::Imported, owned("1 installed plugin")),
		(OutcomeKind::Skipped, owned("1 installed plugin v2 already had")),
		(
			OutcomeKind::NotMigratable,
			owned(
				"plugin skills, MCP servers and hooks (copied for `omp ext`; v2 does not load them \
				 automatically yet)"
			)
		),
		(OutcomeKind::Imported, owned("1 cached marketplace")),
		(OutcomeKind::Skipped, owned("cache/plugins/shared___fmt___2.0.0")),
		(OutcomeKind::Imported, owned("1 cached plugin")),
		(OutcomeKind::NotMigratable, owned("npm plugins (v2 runs no JavaScript plugins)")),
		(OutcomeKind::NotMigratable, None),
		(OutcomeKind::NotMigratable, owned("usage tables: model_usage")),
	]);
	assert_eq!(snapshot(&omp), v1_before);

	let marketplaces: serde_json::Value =
		serde_json::from_slice(&fs::read(v2.data_dir.join("marketplaces.json")).expect("registry"))
			.expect("json");
	assert_eq!(marketplaces["marketplaces"][0]["name"], "acme");
	assert_eq!(
		marketplaces["marketplaces"][0]["catalogPath"],
		v2_plugins
			.join("cache/marketplaces/acme/.claude-plugin/marketplace.json")
			.to_str()
			.expect("utf-8")
	);
	assert_eq!(marketplaces["marketplaces"][1]["sourceUri"], "https://v2.example/m.json");
	let installed: serde_json::Value = serde_json::from_slice(
		&fs::read(v2_plugins.join("installed_plugins.json")).expect("installed"),
	)
	.expect("json");
	let lint = &installed["plugins"]["lint@acme"][0];
	assert_eq!(
		lint["installPath"],
		v2_plugins
			.join("cache/plugins/acme___lint___1.0.0")
			.to_str()
			.expect("utf-8")
	);
	assert_eq!(lint["gitCommitSha"], "abc123");
	assert_eq!(lint["enabled"], true);
	assert_eq!(installed["plugins"]["fmt@shared"][0]["version"], "3.0.0");
	assert_eq!(
		fs::read_to_string(v2_plugins.join("cache/plugins/acme___lint___1.0.0/skills/lint/SKILL.md"))
			.expect("cached skill"),
		"# lint"
	);
	assert!(
		v2_plugins
			.join("cache/marketplaces/acme/.claude-plugin/marketplace.json")
			.is_file()
	);
	assert!(
		!v2_plugins
			.join("cache/plugins/shared___fmt___2.0.0/v1.txt")
			.exists()
	);
	assert!(!v2_plugins.join("cache/plugins/.tmp-01J").exists());
	assert!(!v2.data_dir.join("stats.db").exists());
}

/// One v1 install with every data item, in the default and a named profile.
fn full_fixture(root: &Path) -> (PathBuf, V2Roots) {
	let home = root.join("home");
	let v2 = roots(root);
	let app = project(root, "app");
	let omp = home.join(".omp");
	write(&omp.join("install-id"), "11111111-2222-4333-8444-555555555555\n");
	for agent in [omp.join("agent"), omp.join("profiles/work/agent")] {
		drop(v1_history(&agent.join("history.db"), &[("hello", 1, "/a")]));
		v1_bank(&agent.join("memories/mnemopi/banks/app-1/mnemopi.db"), &[("a fact", &app)]);
		write(&agent.join("memories").join(encode(&app)).join("learned.md"), "- a lesson\n");
	}
	write(&omp.join("marketplaces.json"), r#"{"version":1,"marketplaces":[]}"#);
	write(&omp.join("plugins/cache/plugins/m___p___1/x"), "x");
	write(&omp.join("profiles/work/plugins/cache/plugins/m___q___1/y"), "y");
	(home, v2)
}

#[test]
fn data_steps_hold_the_import_invariants() {
	let root = tempfile::tempdir().expect("scratch");
	let (home, v2) = full_fixture(root.path());
	let omp = home.join(".omp");

	// A dry run writes nothing anywhere.
	let before = snapshot(root.path());
	let dry = import(&v2, inputs(&home), ImportMode::DryRun);
	assert_eq!(snapshot(root.path()), before, "a dry run must not write");
	for step in DATA_STEPS {
		let entries = step_entries(&dry, step);
		assert!(!entries.is_empty(), "{step} reported nothing");
		assert!(
			entries
				.iter()
				.all(|entry| entry.outcome.kind() != OutcomeKind::Imported),
			"{step} imported during a dry run"
		);
	}

	// An apply covers both profiles and leaves v1 byte-identical.
	let v1_before = snapshot(&omp);
	let report = import(&v2, inputs(&home), ImportMode::Apply);
	assert_eq!(snapshot(&omp), v1_before, "the v1 tree must be byte-identical");
	assert_eq!(report.pairs.len(), 2);
	for (pair, target) in report
		.pairs
		.iter()
		.zip([v2.target(None), v2.target(Some("work"))])
	{
		for step in DATA_STEPS {
			assert!(step.marker(&target.config_dir).is_set(), "{step} marker");
			assert!(pair.entries.iter().any(|entry| entry.step == step), "{step} reported nothing");
		}
		assert!(
			pair.entries.iter().all(|entry| !matches!(
				entry.outcome,
				ImportOutcome::NeedsAttention(Attention::Failed(_))
			)),
			"{:?}",
			pair.entries
		);
		assert!(target.data_dir.join("history.db").is_file());
		assert!(target.data_dir.join("install-id").is_file());
	}
	assert!(
		v2.data_dir
			.join("plugins/cache/plugins/m___p___1/x")
			.is_file()
	);
	assert!(
		v2.data_dir
			.join("profiles/work/plugins/cache/plugins/m___q___1/y")
			.is_file()
	);

	// A second run is a no-op through the markers, even after v1 changes.
	write(&omp.join("install-id"), "changed\n");
	let v2_before = snapshot(root.path());
	let again = import(&v2, inputs(&home), ImportMode::Apply);
	for step in DATA_STEPS {
		assert!(
			step_entries(&again, step).iter().all(|entry| matches!(
				entry.outcome,
				ImportOutcome::Skipped(SkipReason::MarkerPresent)
			)),
			"{step} ran again"
		);
	}
	assert_eq!(snapshot(root.path()), v2_before);
}
