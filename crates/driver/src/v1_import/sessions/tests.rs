//! Sessions-step proofs over temporary homes, v1 trees, and v2 roots, with a
//! recording converter standing in for the app's `ForeignFormat::Omp1`
//! importer (whose conversion `omp-app`'s `session_import` tests prove).

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
	time::{Duration, SystemTime},
};

use omp_core::{Hash32, Str};
use omp_dom::{Op, PropKey, Txn, Value};
use omp_journal::blob::{BlobRef, BlobStore};
use omp_session::import::{
	IMPORT_FORMAT, IMPORT_SOURCE, IMPORT_SOURCE_BLOB, IMPORT_SOURCE_ID, OMP1_FORMAT, STAMP_SETTLE,
	SourceStamp,
};
use parking_lot::Mutex;
use serde_json::json;

use super::{super::*, ConvertError, PriorImport, import_selected, list, scan_imports};

/// One call the converter saw.
#[derive(Clone, Debug)]
struct Call {
	source:    PathBuf,
	id:        String,
	source_id: String,
	blobs:     Option<PathBuf>,
	artifacts: Vec<V1Artifact>,
	children:  Vec<V1ChildJob>,
}

/// Writes a one-message journal per transcript, with the provenance the
/// app's importer records first (the transcript's path, its digest unless
/// `unrecorded`, and its settled stamp unless `unstamped`), and records every
/// call; the transcript named `fail` fails after writing a partial
/// destination.
#[derive(Default)]
struct Recorder {
	calls:      Mutex<Vec<Call>>,
	fail:       Option<PathBuf>,
	unrecorded: bool,
	unstamped:  bool,
}

impl V1SessionConverter for Recorder {
	fn convert(&self, conversion: &V1Conversion<'_>) -> Result<usize, ConvertError> {
		self.calls.lock().push(Call {
			source:    conversion.source.to_owned(),
			id:        conversion.id.to_owned(),
			source_id: conversion.source_id.to_owned(),
			blobs:     conversion.blobs.map(Path::to_owned),
			artifacts: conversion.artifacts.to_vec(),
			children:  conversion.children.to_vec(),
		});
		if self.fail.as_deref() == Some(conversion.source) {
			fs::write(conversion.destination, "partial")?;
			return Err("conversion failed".into());
		}
		let mut session = omp_session::Session::create(
			conversion.destination,
			omp_session::ComponentRegistry::standard(),
		)?;
		let meta = session.dom().meta();
		let cause = session.head().ok_or("no genesis")?;
		let set = |prop: &'static str, value: &str| Op::Set {
			h:     meta,
			prop:  PropKey::Custom(Str::new_static(prop)),
			value: Value::Str(Str::new(value)),
		};
		let mut ops = vec![
			set(IMPORT_FORMAT, OMP1_FORMAT),
			set(IMPORT_SOURCE_ID, conversion.source_id),
			set(IMPORT_SOURCE, &conversion.source.to_string_lossy()),
		];
		let taken = SystemTime::now();
		let stat = fs::metadata(conversion.source)?;
		let bytes = fs::read(conversion.source)?;
		if !self.unrecorded {
			let digest = Hash32::sum(&bytes);
			ops.push(set(IMPORT_SOURCE_BLOB, &format!("artifact://sha256/{}", digest.to_hex())));
		}
		if !self.unstamped
			&& let Some(stamp) =
				SourceStamp::of(&stat).and_then(|stamp| stamp.recordable(bytes.len() as u64, taken))
		{
			ops.extend(stamp.ops(meta));
		}
		session.patch(Txn { cause, label: None, ops })?;
		session.begin_turn()?;
		session.user("imported", Vec::new())?;
		Ok(1)
	}
}

impl Recorder {
	fn sources(&self) -> Vec<PathBuf> {
		self
			.calls
			.lock()
			.iter()
			.map(|call| call.source.clone())
			.collect()
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

fn write(path: &Path, contents: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, contents).expect("write");
}

/// Journals (`*.oms`, hidden staging files included) directly in `directory`.
fn journals(directory: &Path) -> Vec<PathBuf> {
	let mut found = fs::read_dir(directory)
		.map(|entries| {
			entries
				.map(|entry| entry.expect("entry").path())
				.filter(|path| path.extension().and_then(|value| value.to_str()) == Some("oms"))
				.collect::<Vec<_>>()
		})
		.unwrap_or_default();
	found.sort();
	found
}

struct Fixture {
	root:    tempfile::TempDir,
	home:    PathBuf,
	/// An existing project directory sessions recorded as their `cwd`.
	project: PathBuf,
	v2:      V2Roots,
}

impl Fixture {
	fn new() -> Self {
		let root = tempfile::tempdir().expect("scratch");
		let home = root.path().join("home");
		let project = root.path().join("project");
		fs::create_dir_all(&project).expect("project");
		let v2 = V2Roots {
			config_dir:     root.path().join("o2"),
			data_dir:       root.path().join("share/omp"),
			state_dir:      root.path().join("state/omp"),
			cache_dir:      root.path().join("cache/omp"),
			active_profile: None,
		};
		Self { root, home, project, v2 }
	}

	fn agent(&self, profile: Option<&str>) -> PathBuf {
		match profile {
			Some(profile) => self.home.join(".omp/profiles").join(profile).join("agent"),
			None => self.home.join(".omp/agent"),
		}
	}

	/// Writes a v3 transcript `<agent>/sessions/-project/<ts>_<id>.jsonl`
	/// with a title slot and one user message.
	fn session(&self, profile: Option<&str>, id: &str, cwd: &Path) -> PathBuf {
		let lines = [
			json!({"type": "title", "v": 1, "title": format!("{id} title"), "updatedAt": "2026-01-02T03:04:07.000Z", "pad": "  "}),
			json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-01-02T03:04:05.000Z", "cwd": cwd}),
			json!({"type": "message", "id": "m1", "parentId": null, "timestamp": "2026-01-02T03:04:06.000Z", "message": {"role": "user", "content": format!("hello {id}"), "timestamp": 1}}),
			json!({"type": "message", "id": "m2", "parentId": "m1", "timestamp": "2026-01-02T03:04:07.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "hi"}], "timestamp": 2}}),
		];
		let path = self
			.agent(profile)
			.join("sessions/-project")
			.join(format!("2026-01-02T03-04-05-000Z_{id}.jsonl"));
		write(&path, &lines.map(|line| line.to_string() + "\n").concat());
		path
	}

	/// A subagent transcript in `parent`'s artifact directory.
	fn child(&self, parent: &Path, agent_id: &str, agent: &str) -> PathBuf {
		let lines = [
			json!({"type": "session", "version": 3, "id": format!("{agent_id}-session"), "timestamp": "2026-01-02T03:05:00.000Z", "cwd": self.project}),
			json!({"type": "session_init", "id": "i1", "parentId": null, "timestamp": "2026-01-02T03:05:00.000Z", "systemPrompt": "", "task": "look", "tools": [], "agent": agent}),
		];
		let path = parent.with_extension("").join(format!("{agent_id}.jsonl"));
		write(&path, &lines.map(|line| line.to_string() + "\n").concat());
		path
	}

	fn pairs(&self, selection: &ProfileSelection) -> Vec<ImportPair> {
		plan(
			&V1Source::new(V1Inputs { home: self.home.clone(), ..V1Inputs::default() }),
			&self.v2,
			selection,
		)
		.expect("plan")
	}

	/// The v2 sessions directory of `bucket` for the default profile.
	fn sessions_dir(&self, bucket: &ProjectBucket) -> PathBuf {
		bucket
			.state_dir(&self.v2.data_dir)
			.expect("state dir")
			.join("sessions")
	}
}

fn session_entries(report: &ImportReport) -> Vec<&ImportEntry> {
	report
		.entries()
		.filter(|entry| entry.step == ImportStep::Sessions)
		.collect()
}

fn outcomes(report: &ImportReport) -> Vec<(Option<&str>, OutcomeKind)> {
	session_entries(report)
		.into_iter()
		.map(|entry| (entry.subject.as_deref(), entry.outcome.kind()))
		.collect()
}

#[test]
fn without_bulk_the_step_points_at_the_picker_and_converts_nothing() {
	let fixture = Fixture::new();
	fixture.session(None, "alpha", &fixture.project);
	let pairs = fixture.pairs(&ProfileSelection::All);

	let report = run(&pairs, ImportMode::Apply, CredentialAccess::Offline(&omp_con::Ctx::new()));

	let entries = session_entries(&report);
	assert_eq!(entries.len(), 1);
	assert!(matches!(entries[0].outcome, ImportOutcome::Skipped(SkipReason::OnDemand)));
	assert_eq!(entries[0].path.as_deref(), Some(fixture.agent(None).join("sessions").as_path()));
	assert!(!ImportStep::Sessions.marker(&fixture.v2.config_dir).is_set(), "on demand stays open");
	assert!(!fixture.v2.data_dir.exists(), "nothing is converted or recorded");
}

#[test]
fn a_bulk_dry_run_lists_every_session_and_writes_nothing() {
	let fixture = Fixture::new();
	fixture.session(None, "alpha", &fixture.project);
	fixture.session(None, "gone", &fixture.root.path().join("vanished"));
	let pairs = fixture.pairs(&ProfileSelection::All);
	let before = snapshot(fixture.root.path());
	let recorder = Recorder::default();

	let report = run_with(
		&pairs,
		ImportMode::DryRun,
		CredentialAccess::Offline(&omp_con::Ctx::new()),
		SessionImport::Bulk(&recorder),
	);

	assert_eq!(snapshot(fixture.root.path()), before, "a dry run must not write anywhere");
	assert!(recorder.sources().is_empty());
	assert_eq!(outcomes(&report), [
		(Some("alpha"), OutcomeKind::WouldImport),
		(Some("gone (project directory gone: no-directory)"), OutcomeKind::WouldImport),
	]);
}

#[test]
fn a_bulk_import_places_links_pins_and_runs_once() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	let gone = fixture.session(None, "gone", &fixture.root.path().join("vanished"));
	let scout = fixture.child(&alpha, "0-Scout", "scout");
	write(&alpha.with_extension("").join("0.bash.log"), "spilled output");
	write(&fixture.agent(None).join("sessions/-project/notes.jsonl"), "{\"not\":\"a session\"}\n");
	write(&fixture.agent(None).join("session-pins.json"), "[\"alpha\",\"stale\"]");
	let blobs = fixture.agent(None).join("blobs");
	fs::create_dir_all(&blobs).expect("blobs");
	let v1_before = snapshot(&fixture.home);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();
	let bulk = SessionImport::Bulk(&recorder);
	let offline = omp_con::Ctx::new();

	let report = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);

	assert_eq!(outcomes(&report), [
		(Some("alpha"), OutcomeKind::Imported),
		(Some("gone (project directory gone: no-directory)"), OutcomeKind::Imported),
		(None, OutcomeKind::NothingToImport),
	]);
	// The subagent converted first, then its parent, which links it.
	assert_eq!(recorder.sources(), [scout, alpha, gone]);
	let calls = recorder.calls.lock().clone();
	let project = fixture.sessions_dir(&ProjectBucket::for_cwd(Some(&fixture.project)));
	let project_journals = journals(&project);
	assert_eq!(project_journals.len(), 2, "the parent and its child journal: {project_journals:?}");
	assert!(
		fs::read_dir(&project).expect("sessions").all(|entry| !entry
			.expect("entry")
			.file_name()
			.to_string_lossy()
			.contains("importing")),
		"no staging file or its lock is left behind"
	);
	let parent = project.join(format!("{}.oms", calls[1].id));
	let child = project.join(format!("{}.oms", calls[0].id));
	assert!(parent.is_file() && child.is_file());
	assert_eq!(calls[1].children, [V1ChildJob {
		id:         Str::new(&calls[0].id),
		agent:      Str::new_static("scout"),
		source_id:  Str::new_static("0-Scout"),
		started_ms: 1_767_323_100_000,
	}]);
	// The session keeps its whole artifact directory, copied into the
	// bucket's project blob store; the subagent references none of it.
	let spilled = BlobRef { hash: Hash32::sum(b"spilled output"), size: 14 };
	assert_eq!(calls[1].artifacts, [V1Artifact {
		name: Str::new_static("0.bash.log"),
		id:   Some(0),
		blob: spilled,
	}]);
	let store =
		BlobStore::open(omp_env::project_state::blob_store(project.parent().expect("state dir")))
			.expect("store");
	assert_eq!(store.get(&spilled).expect("copied").as_ref(), b"spilled output");
	assert!(calls[0].artifacts.is_empty());
	assert_eq!(calls[1].blobs.as_deref(), Some(blobs.as_path()));
	assert_eq!(calls[1].source_id, "alpha");
	assert_eq!(calls[0].source_id, "0-Scout-session");
	assert!(calls[2].artifacts.is_empty());
	// A vanished working directory lands in the no-directory bucket.
	let orphans = fixture.sessions_dir(&ProjectBucket::NoDirectory);
	assert_eq!(orphans, fixture.v2.data_dir.join("projects/no-directory/sessions"));
	assert_eq!(journals(&orphans), [orphans.join(format!("{}.oms", calls[2].id))]);
	// The v1 pin follows its session to the new ULID; a stale pin is dropped.
	let pins = fs::read_to_string(project.with_file_name("session-pins.json")).expect("pins");
	assert_eq!(pins, format!("[\"{}\"]", calls[1].id));
	assert!(!orphans.with_file_name("session-pins.json").exists());
	assert!(ImportStep::Sessions.marker(&fixture.v2.config_dir).is_set());
	// Copy only.
	assert_eq!(snapshot(&fixture.home), v1_before);

	// The marker does not gate a second bulk run; the journals skip every
	// session, and nothing is written.
	let v2_before = snapshot(&fixture.v2.data_dir);
	let again = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&again), [
		(Some("alpha"), OutcomeKind::Skipped),
		(Some("gone (project directory gone: no-directory)"), OutcomeKind::Skipped),
		(None, OutcomeKind::NothingToImport),
	]);
	assert!(matches!(
		session_entries(&again)[0].outcome,
		ImportOutcome::Skipped(SkipReason::SessionImported)
	));
	assert_eq!(recorder.sources().len(), 3, "nothing converts twice");
	assert_eq!(snapshot(&fixture.v2.data_dir), v2_before);
	// On demand, the marker reports the bulk run.
	let on_demand = run(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline));
	let entries = session_entries(&on_demand);
	assert_eq!(entries.len(), 1);
	assert!(matches!(entries[0].outcome, ImportOutcome::Skipped(SkipReason::MarkerPresent)));
}

#[test]
fn the_picker_lists_without_converting_and_converts_on_pick() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	fixture.child(&alpha, "0-Scout", "scout");
	fixture.session(None, "gone", &fixture.root.path().join("vanished"));
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();

	let rows = list(&pairs[0]).expect("list");
	assert!(!fixture.v2.data_dir.exists(), "listing converts nothing");
	assert!(recorder.sources().is_empty());
	let alpha_row = rows
		.iter()
		.find(|row| row.id == "alpha")
		.expect("alpha row");
	assert_eq!(rows.len(), 2, "subagent transcripts are not listed: {rows:?}");
	assert_eq!(alpha_row.title.as_deref(), Some("alpha title"));
	assert_eq!(alpha_row.first_message.as_deref(), Some("hello alpha"));
	assert_eq!(alpha_row.messages, 2);
	assert_eq!(alpha_row.cwd.as_deref(), Some(fixture.project.as_path()));
	assert_eq!(alpha_row.created_ms, 1_767_323_045_000);
	assert_eq!(alpha_row.imported, None);

	let picked = import_selected(&pairs[0], &alpha, &recorder).expect("pick");
	assert!(picked.converted);
	assert_eq!(picked.children, 1);
	assert!(picked.journal.is_file());
	assert_eq!(picked.bucket, ProjectBucket::for_cwd(Some(&fixture.project)));
	// Picking it again reopens the same journal.
	let again = import_selected(&pairs[0], &alpha, &recorder).expect("pick again");
	assert!(!again.converted);
	assert_eq!(again.journal, picked.journal);
	assert_eq!(recorder.sources().len(), 2);
	// So does a later bulk run.
	let report = run_with(
		&pairs,
		ImportMode::Apply,
		CredentialAccess::Offline(&omp_con::Ctx::new()),
		SessionImport::Bulk(&recorder),
	);
	assert_eq!(outcomes(&report)[0], (Some("alpha"), OutcomeKind::Skipped));
	// Only v1 transcripts can be picked.
	let outside = fixture.root.path().join("elsewhere.jsonl");
	fs::copy(&alpha, &outside).expect("copy");
	assert!(matches!(
		import_selected(&pairs[0], &outside, &recorder),
		Err(SessionImportError::OutsideSessions { .. })
	));
}

#[test]
fn a_named_profile_imports_into_its_namesake() {
	let fixture = Fixture::new();
	fixture.session(Some("work"), "w1", &fixture.project);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();

	let report = run_with(
		&pairs,
		ImportMode::Apply,
		CredentialAccess::Offline(&omp_con::Ctx::new()),
		SessionImport::Bulk(&recorder),
	);

	assert_eq!(outcomes(&report), [
		(None, OutcomeKind::NothingToImport),
		(Some("w1"), OutcomeKind::Imported),
	]);
	let work = fixture.v2.target(Some("work"));
	let sessions = ProjectBucket::for_cwd(Some(&fixture.project))
		.state_dir(&work.data_dir)
		.expect("state")
		.join("sessions");
	assert_eq!(journals(&sessions).len(), 1);
	let imported = scan_imports(&work).expect("index");
	assert_eq!(imported.journal("w1"), journals(&sessions).first().map(PathBuf::as_path));
	assert!(
		scan_imports(&fixture.v2.target(None))
			.expect("index")
			.is_empty()
	);
	assert!(!fixture.v2.data_dir.join("projects").exists(), "the default profile is untouched");
	assert!(ImportStep::Sessions.marker(&work.config_dir).is_set());
	assert!(ImportStep::Sessions.marker(&fixture.v2.config_dir).is_set());
}

#[test]
fn a_failed_session_is_reported_discarded_and_retried() {
	let fixture = Fixture::new();
	fixture.session(None, "alpha", &fixture.project);
	let gone = fixture.session(None, "gone", &fixture.root.path().join("vanished"));
	let pairs = fixture.pairs(&ProfileSelection::All);
	let failing = Recorder { fail: Some(gone.clone()), ..Recorder::default() };
	let offline = omp_con::Ctx::new();

	let report = run_with(
		&pairs,
		ImportMode::Apply,
		CredentialAccess::Offline(&offline),
		SessionImport::Bulk(&failing),
	);

	let entries = session_entries(&report);
	assert!(matches!(entries[0].outcome, ImportOutcome::Imported));
	assert!(matches!(
		entries[1].outcome,
		ImportOutcome::NeedsAttention(Attention::Failed(ImportError::Session(
			SessionImportError::Convert { .. }
		)))
	));
	let orphans = fixture.sessions_dir(&ProjectBucket::NoDirectory);
	assert!(journals(&orphans).is_empty(), "the partial journal is discarded");
	assert!(
		!ImportStep::Sessions.marker(&fixture.v2.config_dir).is_set(),
		"a failure stays retryable"
	);

	let retry = Recorder::default();
	let report = run_with(
		&pairs,
		ImportMode::Apply,
		CredentialAccess::Offline(&offline),
		SessionImport::Bulk(&retry),
	);
	assert_eq!(outcomes(&report), [
		(Some("alpha"), OutcomeKind::Skipped),
		(Some("gone (project directory gone: no-directory)"), OutcomeKind::Imported),
	]);
	assert_eq!(retry.sources(), [gone]);
	assert!(ImportStep::Sessions.marker(&fixture.v2.config_dir).is_set());
}

#[test]
fn the_picker_marks_imported_sessions_and_a_deleted_journal_imports_again() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	fixture.session(None, "gone", &fixture.root.path().join("vanished"));
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();

	let picked = import_selected(&pairs[0], &alpha, &recorder).expect("pick");

	let rows = list(&pairs[0]).expect("list");
	let imported = |id: &str| {
		rows
			.iter()
			.find(|row| row.id == id)
			.expect("row")
			.imported
			.clone()
	};
	assert_eq!(
		imported("alpha"),
		Some(PriorImport::Current(picked.journal.clone())),
		"the journal is the record"
	);
	assert_eq!(imported("gone"), None);
	// The journal is the only record: without it the session imports again.
	fs::remove_file(&picked.journal).expect("delete the imported journal");
	assert!(
		list(&pairs[0])
			.expect("list")
			.iter()
			.all(|row| row.imported.is_none())
	);
	let again = import_selected(&pairs[0], &alpha, &recorder).expect("pick again");
	assert!(again.converted);
	assert_ne!(again.journal, picked.journal);
	assert!(!fixture.v2.data_dir.join("v1-sessions").exists(), "no side records");
}

#[test]
fn artifact_references_parse_only_v1_ids() {
	let text = br#"{"text":"see artifact://0 and artifact://12:1-30, artifact://3.\n[raw output: artifact://12]"}
{"text":"artifact://sha256/abc artifact://4x artifact:// artifact://5_ artifact://99999999999999999999999"}"#;
	assert_eq!(super::artifact_references(text), [0, 3, 12]);
	assert_eq!(super::artifact_id("3.bash.log"), Some(3));
	assert_eq!(super::artifact_id("0-Scout.md"), None);
	assert_eq!(super::artifact_id(".log"), None);
	assert_eq!(super::artifact_id("12"), None);
}

#[test]
fn v1_artifacts_are_copied_mapped_and_missing_ones_reported() {
	let fixture = Fixture::new();
	let lines = [
		json!({"type": "session", "version": 3, "id": "art", "timestamp": "2026-01-02T03:04:05.000Z", "cwd": fixture.project}),
		json!({"type": "message", "id": "m1", "parentId": null, "timestamp": "2026-01-02T03:04:06.000Z", "message": {"role": "user", "content": "run it", "timestamp": 1}}),
		json!({"type": "message", "id": "m2", "parentId": "m1", "timestamp": "2026-01-02T03:04:07.000Z", "message": {"role": "toolResult", "toolCallId": "c1", "toolName": "bash", "content": [{"type": "text", "text": "head\n[raw output: artifact://0]"}], "isError": false, "timestamp": 2}}),
		json!({"type": "message", "id": "m3", "parentId": "m2", "timestamp": "2026-01-02T03:04:08.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "also artifact://9"}], "timestamp": 3}}),
	];
	let transcript = fixture
		.agent(None)
		.join("sessions/-project/2026-01-02T03-04-05-000Z_art.jsonl");
	write(&transcript, &lines.map(|line| line.to_string() + "\n").concat());
	let directory = transcript.with_extension("");
	write(&directory.join("0.bash.log"), "zero output");
	write(&directory.join("5.read.log"), "five output");
	write(&directory.join("notes.md"), "notes");
	let child = directory.join("0-Task.jsonl");
	write(
		&child,
		&[
			json!({"type": "session", "version": 3, "id": "task-session", "timestamp": "2026-01-02T03:05:00.000Z", "cwd": fixture.project}),
			json!({"type": "message", "id": "c1", "parentId": null, "timestamp": "2026-01-02T03:05:01.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "read artifact://5 and artifact://7"}], "timestamp": 1}}),
		]
		.map(|line| line.to_string() + "\n")
		.concat(),
	);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();
	let offline = omp_con::Ctx::new();
	let missing = |report: &ImportReport| {
		session_entries(report)
			.into_iter()
			.skip(1)
			.map(|entry| {
				assert!(matches!(
					entry.outcome,
					ImportOutcome::NeedsAttention(Attention::ArtifactMissing)
				));
				(entry.subject.clone(), entry.path.clone())
			})
			.collect::<Vec<_>>()
	};

	// A dry run reports the missing artifacts a real run reports, and writes
	// nothing.
	let before = snapshot(fixture.root.path());
	let dry = run_with(
		&pairs,
		ImportMode::DryRun,
		CredentialAccess::Offline(&offline),
		SessionImport::Bulk(&recorder),
	);
	assert_eq!(snapshot(fixture.root.path()), before, "a dry run must not write anywhere");
	assert!(recorder.sources().is_empty());
	assert_eq!(outcomes(&dry), [
		(Some("art"), OutcomeKind::WouldImport),
		(Some("art artifact://7"), OutcomeKind::NeedsAttention),
		(Some("art artifact://9"), OutcomeKind::NeedsAttention),
	]);

	let report = run_with(
		&pairs,
		ImportMode::Apply,
		CredentialAccess::Offline(&offline),
		SessionImport::Bulk(&recorder),
	);

	assert_eq!(missing(&dry), missing(&report), "the dry run reports what the real run did");
	let entries = session_entries(&report);
	assert_eq!(outcomes(&report), [
		(Some("art"), OutcomeKind::Imported),
		(Some("art artifact://7"), OutcomeKind::NeedsAttention),
		(Some("art artifact://9"), OutcomeKind::NeedsAttention),
	]);
	assert!(matches!(entries[1].outcome, ImportOutcome::NeedsAttention(Attention::ArtifactMissing)));
	assert_eq!(entries[1].path.as_deref(), Some(child.as_path()), "the subagent referenced it");
	assert_eq!(entries[2].path.as_deref(), Some(transcript.as_path()));
	assert!(
		ImportStep::Sessions.marker(&fixture.v2.config_dir).is_set(),
		"a missing artifact is not a failure"
	);
	let blob = |bytes: &[u8]| BlobRef { hash: Hash32::sum(bytes), size: bytes.len() as u64 };
	let artifact = |name: &'static str, id: Option<u64>, bytes: &[u8]| V1Artifact {
		name: Str::new_static(name),
		id,
		blob: blob(bytes),
	};
	let calls = recorder.calls.lock().clone();
	assert_eq!(calls[0].source, child);
	assert_eq!(calls[0].artifacts, [artifact("5.read.log", Some(5), b"five output")]);
	assert_eq!(calls[1].artifacts, [
		artifact("0.bash.log", Some(0), b"zero output"),
		artifact("5.read.log", Some(5), b"five output"),
		artifact("notes.md", None, b"notes"),
	]);
	let state = ProjectBucket::for_cwd(Some(&fixture.project))
		.state_dir(&fixture.v2.data_dir)
		.expect("state");
	let store =
		BlobStore::open(omp_env::project_state::blob_store(&state)).expect("project blob store");
	for bytes in [b"zero output".as_slice(), b"five output", b"notes"] {
		assert_eq!(store.get(&blob(bytes)).expect("copied").as_ref(), bytes);
	}
}

/// Appends a user message to a v1 transcript, as v1 did when the session
/// went on after its import.
fn grow(transcript: &Path, text: &str) {
	let line = json!({"type": "message", "id": "m9", "parentId": "m2", "timestamp": "2026-01-03T00:00:00.000Z", "message": {"role": "user", "content": text, "timestamp": 9}});
	let mut bytes = fs::read(transcript).expect("transcript");
	bytes.extend_from_slice(line.to_string().as_bytes());
	bytes.push(b'\n');
	fs::write(transcript, bytes).expect("grow");
}

#[test]
fn a_session_changed_since_import_is_marked_and_imports_again_beside_the_earlier_one() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	fixture.session(None, "beta", &fixture.project);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();
	let offline = omp_con::Ctx::new();
	let bulk = SessionImport::Bulk(&recorder);
	run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	let first = scan_imports(&pairs[0].target)
		.expect("index")
		.journal("alpha")
		.expect("imported")
		.to_owned();
	let first_bytes = fs::read(&first).expect("journal");

	grow(&alpha, "one more thing");

	// The picker marks it changed, naming the earlier journal.
	let rows = list(&pairs[0]).expect("list");
	let prior = |rows: &[V1SessionInfo], id: &str| {
		rows
			.iter()
			.find(|row| row.id == id)
			.expect("row")
			.imported
			.clone()
	};
	assert_eq!(prior(&rows, "alpha"), Some(PriorImport::Changed(first.clone())));
	assert!(matches!(prior(&rows, "beta"), Some(PriorImport::Current(_))));
	// A dry run says it would import it again, and writes nothing.
	let before = snapshot(fixture.root.path());
	let dry = run_with(&pairs, ImportMode::DryRun, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(snapshot(fixture.root.path()), before);
	assert_eq!(outcomes(&dry), [
		(Some("alpha"), OutcomeKind::WouldReimport),
		(Some("beta"), OutcomeKind::Skipped),
	]);

	// A bulk rerun imports it again into a fresh journal; the earlier one
	// stays, untouched.
	let again = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&again), [
		(Some("alpha"), OutcomeKind::Reimported),
		(Some("beta"), OutcomeKind::Skipped),
	]);
	assert_eq!(recorder.sources(), [
		alpha.clone(),
		fixture
			.agent(None)
			.join("sessions/-project/2026-01-02T03-04-05-000Z_beta.jsonl"),
		alpha.clone()
	]);
	assert_eq!(fs::read(&first).expect("earlier journal"), first_bytes, "journals are append-only");
	let project = fixture.sessions_dir(&ProjectBucket::for_cwd(Some(&fixture.project)));
	assert_eq!(journals(&project).len(), 3, "both imports of alpha, and beta");
	let second = project.join(format!("{}.oms", recorder.calls.lock()[2].id));
	let rows = list(&pairs[0]).expect("list");
	assert_eq!(prior(&rows, "alpha"), Some(PriorImport::Current(second.clone())));
	let index = scan_imports(&pairs[0].target).expect("index");
	assert_eq!(index.len(), 2);
	// Nothing changed since: the next run skips it.
	let settled = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&settled)[0], (Some("alpha"), OutcomeKind::Skipped));
	assert_eq!(recorder.sources().len(), 3);

	// The picker imports a changed session again too, and reopens it after.
	grow(&alpha, "and another");
	let picked = import_selected(&pairs[0], &alpha, &recorder).expect("pick");
	assert!(picked.converted);
	assert!(
		picked
			.previous
			.as_ref()
			.is_some_and(|previous| *previous == first || *previous == second),
		"{picked:?}"
	);
	assert!(picked.journal != first && picked.journal != second);
	let reopened = import_selected(&pairs[0], &alpha, &recorder).expect("pick again");
	assert!(!reopened.converted);
	assert_eq!(reopened.journal, picked.journal);
	assert_eq!(journals(&project).len(), 4);
}

#[test]
fn a_journal_without_a_recorded_digest_counts_as_current() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder { unrecorded: true, ..Recorder::default() };
	let picked = import_selected(&pairs[0], &alpha, &recorder).expect("pick");

	grow(&alpha, "later");

	let rows = list(&pairs[0]).expect("list");
	assert_eq!(rows[0].imported, Some(PriorImport::Current(picked.journal)));
	let again = import_selected(&pairs[0], &alpha, &recorder).expect("pick again");
	assert!(!again.converted);
}

/// One record as the earlier importer wrote it.
fn record(id: &str) -> String {
	json!({"id": id, "source": format!("/v1/sessions/-p/2026-01-02T03-04-05-000Z_{id}.jsonl"), "journal": "/v2/projects/p/sessions/01JABCDEFGHJKMNPQRSTVWXYZ0.oms"}).to_string()
}

#[test]
fn obsolete_import_records_are_retired_and_nothing_else() {
	let fixture = Fixture::new();
	fixture.session(None, "alpha", &fixture.project);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let offline = omp_con::Ctx::new();
	let records = fixture.v2.data_dir.join("v1-sessions");
	// Two records: a safe id names its file, any other id its digest.
	write(&records.join("alpha.json"), &record("alpha"));
	let odd = "a b/c";
	write(&records.join(format!("{}.json", Hash32::sum(odd.as_bytes()).to_hex())), &record(odd));
	// Everything else stays.
	let keep = [
		("beta.json", record("gamma")),
		("partial.json", json!({"id": "partial"}).to_string()),
		(
			"extra.json",
			json!({"id": "extra", "source": "/v1/x_extra.jsonl", "journal": "/v2/y.oms", "more": 1})
				.to_string(),
		),
		(
			"journal.json",
			json!({"id": "journal", "source": "/v1/x_journal.jsonl", "journal": "/v2/y.txt"})
				.to_string(),
		),
		("notes.txt", record("notes")),
		("alpha.json.tmp-7", record("alpha")),
	];
	for (name, contents) in &keep {
		write(&records.join(name), contents);
	}
	fs::create_dir_all(records.join("delta.json")).expect("a directory");
	#[cfg(unix)]
	{
		write(&fixture.root.path().join("elsewhere/linked.json"), &record("linked"));
		std::os::unix::fs::symlink(
			fixture.root.path().join("elsewhere/linked.json"),
			records.join("linked.json"),
		)
		.expect("symlink");
	}
	// Retiring does not wait for the step's marker (an earlier bulk run set it).
	ImportStep::Sessions
		.marker(&fixture.v2.config_dir)
		.set(None)
		.expect("mark");
	let retired = |report: &ImportReport| {
		session_entries(report)
			.into_iter()
			.filter(|entry| {
				matches!(entry.outcome, ImportOutcome::Removed | ImportOutcome::WouldRemove)
			})
			.map(|entry| (entry.subject.clone(), entry.path.clone(), entry.outcome.kind()))
			.collect::<Vec<_>>()
	};

	let before = snapshot(fixture.root.path());
	let dry = run(&pairs, ImportMode::DryRun, CredentialAccess::Offline(&offline));
	assert_eq!(snapshot(fixture.root.path()), before, "a dry run deletes nothing");
	assert_eq!(retired(&dry), [(
		Some(Str::new_static("2 obsolete v1 session import records")),
		Some(records.clone()),
		OutcomeKind::WouldRemove
	)]);

	let report = run(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline));
	assert_eq!(retired(&report), [(
		Some(Str::new_static("2 obsolete v1 session import records")),
		Some(records.clone()),
		OutcomeKind::Removed
	)]);
	let mut left = fs::read_dir(&records)
		.expect("records")
		.map(|entry| {
			entry
				.expect("entry")
				.file_name()
				.to_string_lossy()
				.into_owned()
		})
		.collect::<Vec<_>>();
	left.sort();
	let mut expected = keep
		.iter()
		.map(|(name, _)| (*name).to_owned())
		.collect::<Vec<_>>();
	expected.push("delta.json".to_owned());
	#[cfg(unix)]
	expected.push("linked.json".to_owned());
	expected.sort();
	assert_eq!(left, expected);
	#[cfg(unix)]
	assert!(fixture.root.path().join("elsewhere/linked.json").is_file(), "links are not followed");

	// Once only what was kept is gone, the empty directory goes too, quietly.
	fs::remove_dir_all(&records).expect("clear");
	fs::create_dir_all(&records).expect("empty");
	let quiet = run(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline));
	assert!(retired(&quiet).is_empty());
	assert!(!records.exists(), "the empty directory is removed");
	let absent = run(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline));
	assert!(retired(&absent).is_empty());
	assert!(
		session_entries(&absent)
			.iter()
			.all(|entry| !matches!(entry.outcome, ImportOutcome::NeedsAttention(_)))
	);
}

/// How many transcripts this thread digested so far.
fn digests() -> usize {
	crate::session_imports::DIGESTS.with(std::cell::Cell::get)
}

/// Sets `path`'s modification time a minute back and, on Unix, waits for
/// the change time that moved to now to settle: an import then records its
/// stamp. No call sets the change time, so only waiting settles it.
fn backdate(path: &Path) {
	let earlier = SystemTime::now() - Duration::from_secs(60);
	fs::File::options()
		.write(true)
		.open(path)
		.and_then(|file| file.set_modified(earlier))
		.expect("backdate");
	if cfg!(unix) {
		std::thread::sleep(STAMP_SETTLE + Duration::from_millis(50));
	}
}

/// Rewrites `from` as `to` (of the same length) in place, as v1 rewrote its
/// padded title slot: same size, new modification time.
fn retitle(transcript: &Path, from: &str, to: &str) {
	assert_eq!(from.len(), to.len());
	let bytes = fs::read(transcript).expect("transcript");
	let at = memchr::memmem::find(&bytes, from.as_bytes()).expect("title");
	let mut edited = bytes.clone();
	edited[at..at + to.len()].copy_from_slice(to.as_bytes());
	fs::write(transcript, &edited).expect("retitle");
	assert_eq!(fs::metadata(transcript).expect("stat").len(), bytes.len() as u64);
}

/// The v2 pin list of the fixture project's bucket.
fn project_pins(fixture: &Fixture) -> Vec<String> {
	let state = ProjectBucket::for_cwd(Some(&fixture.project))
		.state_dir(&fixture.v2.data_dir)
		.expect("state");
	fs::read(state.join("session-pins.json"))
		.map(|bytes| serde_json::from_slice(&bytes).expect("pin list"))
		.unwrap_or_default()
}

fn stem(journal: &Path) -> String {
	journal
		.file_stem()
		.and_then(|stem| stem.to_str())
		.expect("ULID stem")
		.to_owned()
}

#[test]
fn the_pin_follows_the_newest_import_of_a_pinned_session() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	let beta = fixture.session(None, "beta", &fixture.project);
	write(&fixture.agent(None).join("session-pins.json"), "[\"alpha\"]");
	let v1_pins = fs::read(fixture.agent(None).join("session-pins.json")).expect("v1 pins");
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();
	let offline = omp_con::Ctx::new();
	let bulk = SessionImport::Bulk(&recorder);
	run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	let index = scan_imports(&pairs[0].target).expect("index");
	let first = index.journal("alpha").expect("alpha").to_owned();
	let beta_first = index.journal("beta").expect("beta").to_owned();
	assert_eq!(project_pins(&fixture), [stem(&first)]);
	// The user pins beta's journal in v2; v1 does not pin beta.
	let state = fixture
		.sessions_dir(&ProjectBucket::for_cwd(Some(&fixture.project)))
		.with_file_name("session-pins.json");
	fs::write(&state, serde_json::to_vec(&[stem(&first), stem(&beta_first)]).expect("json"))
		.expect("user pin");

	grow(&alpha, "went on");
	grow(&beta, "went on too");
	let again = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&again), [
		(Some("alpha"), OutcomeKind::Reimported),
		(Some("beta"), OutcomeKind::Reimported),
	]);
	let index = scan_imports(&pairs[0].target).expect("index");
	let second = index.journal("alpha").expect("alpha").to_owned();
	let beta_second = index.journal("beta").expect("beta").to_owned();
	assert_ne!(second, first);
	// alpha's pin moved to its newest journal; beta's v2 pin, which v1 never
	// set, stays where the user put it, and the new journal is not pinned.
	assert_eq!(project_pins(&fixture), [stem(&beta_first), stem(&second)]);
	assert!(!project_pins(&fixture).contains(&stem(&beta_second)));

	// The picker's re-import moves it too.
	grow(&alpha, "and on");
	let picked = import_selected(&pairs[0], &alpha, &recorder).expect("pick");
	assert_eq!(picked.previous.as_deref(), Some(second.as_path()));
	assert_eq!(project_pins(&fixture), [stem(&beta_first), stem(&picked.journal)]);
	// Pins are v2 state: v1's list is only read.
	assert_eq!(fs::read(fixture.agent(None).join("session-pins.json")).expect("v1"), v1_pins);
}

#[test]
fn an_unchanged_transcript_is_not_read_to_tell_it_is_current() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	backdate(&alpha);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();
	let offline = omp_con::Ctx::new();
	let bulk = SessionImport::Bulk(&recorder);
	let picked = import_selected(&pairs[0], &alpha, &recorder).expect("pick");

	// Listing, a dry run, a bulk run, and picking it again all find it
	// current from its size and modification time alone.
	let before = digests();
	assert_eq!(
		list(&pairs[0]).expect("list")[0].imported,
		Some(PriorImport::Current(picked.journal.clone()))
	);
	let dry = run_with(&pairs, ImportMode::DryRun, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&dry), [(Some("alpha"), OutcomeKind::Skipped)]);
	let rerun = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&rerun), [(Some("alpha"), OutcomeKind::Skipped)]);
	assert!(
		!import_selected(&pairs[0], &alpha, &recorder)
			.expect("reopen")
			.converted
	);
	assert_eq!(digests(), before, "an unchanged transcript is never digested");

	// Touched but unchanged: the stamp differs, the digest still matches.
	fs::File::options()
		.write(true)
		.open(&alpha)
		.and_then(|file| file.set_modified(SystemTime::now()))
		.expect("touch");
	assert_eq!(
		list(&pairs[0]).expect("list")[0].imported,
		Some(PriorImport::Current(picked.journal.clone()))
	);
	assert_eq!(digests(), before + 1);

	// v1 rewrote its padded title slot in place: the size is the same, the
	// modification time is not, so the digest finds the change.
	retitle(&alpha, "alpha title", "alpha TITLE");
	assert_eq!(
		list(&pairs[0]).expect("list")[0].imported,
		Some(PriorImport::Changed(picked.journal))
	);
	assert_eq!(digests(), before + 2);
}

/// An edit that keeps the size and then puts the modification time back
/// (`touch -r`, `rsync -t`, a backup restore) leaves size and modification
/// time as imported; the change time, which no call sets, still moved, so
/// the transcript is digested and the edit found.
#[cfg(unix)]
#[test]
fn a_same_size_edit_with_its_modification_time_restored_is_digested() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	backdate(&alpha);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let picked = import_selected(&pairs[0], &alpha, &Recorder::default()).expect("pick");
	let stat = fs::metadata(&alpha).expect("stat");
	let imported = SourceStamp::of(&stat).expect("stamp");
	let entries = omp_journal::Journal::scan(&picked.journal).expect("journal");
	let origin = omp_session::import::import_origin(&entries).expect("origin");
	assert_eq!(origin.source_stamp, Some(imported));
	let before = digests();
	assert_eq!(
		list(&pairs[0]).expect("list")[0].imported,
		Some(PriorImport::Current(picked.journal.clone()))
	);
	assert_eq!(digests(), before, "unchanged: not digested");

	retitle(&alpha, "alpha title", "alpha TITLE");
	fs::File::options()
		.write(true)
		.open(&alpha)
		.and_then(|file| {
			file.set_times(fs::FileTimes::new().set_modified(stat.modified().expect("mtime")))
		})
		.expect("restore the modification time");
	let edited = SourceStamp::of(&fs::metadata(&alpha).expect("stat")).expect("stamp");
	assert_eq!((edited.size, edited.modified_ns), (imported.size, imported.modified_ns));
	assert_ne!(edited.changed_ns, imported.changed_ns);
	assert_eq!(
		list(&pairs[0]).expect("list")[0].imported,
		Some(PriorImport::Changed(picked.journal))
	);
	assert_eq!(digests(), before + 1);
}

#[test]
fn a_journal_without_a_recorded_stamp_falls_back_to_the_digest() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	backdate(&alpha);
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder { unstamped: true, ..Recorder::default() };
	let picked = import_selected(&pairs[0], &alpha, &recorder).expect("pick");

	let before = digests();
	assert_eq!(
		list(&pairs[0]).expect("list")[0].imported,
		Some(PriorImport::Current(picked.journal.clone()))
	);
	assert_eq!(digests(), before + 1, "no stamp to trust: digested");
	grow(&alpha, "later");
	assert_eq!(
		list(&pairs[0]).expect("list")[0].imported,
		Some(PriorImport::Changed(picked.journal))
	);

	// A transcript modified too recently records no stamp either.
	let fresh = Fixture::new();
	let beta = fresh.session(None, "beta", &fresh.project);
	let pairs = fresh.pairs(&ProfileSelection::All);
	let picked = import_selected(&pairs[0], &beta, &Recorder::default()).expect("pick");
	let entries = omp_journal::Journal::scan(&picked.journal).expect("journal");
	let origin = omp_session::import::import_origin(&entries).expect("origin");
	assert_eq!(origin.source_stamp, None);
	assert!(origin.source_digest.is_some());
}

#[test]
fn another_file_of_an_imported_session_is_its_own_import() {
	let fixture = Fixture::new();
	// The v1 home is reached through a symlink: the picker canonicalizes
	// what it imports, a listing and a bulk run do not.
	#[cfg(unix)]
	{
		let real = fixture.root.path().join("real-home");
		fs::create_dir_all(&real).expect("real home");
		std::os::unix::fs::symlink(&real, &fixture.home).expect("home link");
	}
	let alpha = fixture.session(None, "alpha", &fixture.project);
	// A second file carrying the same session id: a copy that went on.
	let copy = fixture
		.agent(None)
		.join("sessions/-elsewhere/2026-01-02T03-04-05-000Z_alpha.jsonl");
	fs::create_dir_all(copy.parent().expect("parent")).expect("dir");
	fs::copy(&alpha, &copy).expect("copy");
	grow(&copy, "only in the copy");
	write(&fixture.agent(None).join("session-pins.json"), "[\"alpha\"]");
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();
	let offline = omp_con::Ctx::new();
	let bulk = SessionImport::Bulk(&recorder);
	let first = import_selected(&pairs[0], &alpha, &recorder).expect("pick");
	assert_eq!(project_pins(&fixture), [stem(&first.journal)]);

	let prior = |path: &Path| {
		list(&pairs[0])
			.expect("list")
			.into_iter()
			.find(|row| row.path == path)
			.expect("row")
			.imported
	};
	assert_eq!(prior(&alpha), Some(PriorImport::Current(first.journal.clone())));
	assert_eq!(prior(&copy), Some(PriorImport::OtherFile(first.journal.clone())));
	// Transcripts go in path order: `-elsewhere` first.
	let dry = run_with(&pairs, ImportMode::DryRun, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&dry), [
		(Some("alpha (same session, other file)"), OutcomeKind::WouldImport),
		(Some("alpha"), OutcomeKind::Skipped),
	]);

	let report = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&report), [
		(Some("alpha (same session, other file)"), OutcomeKind::Imported),
		(Some("alpha"), OutcomeKind::Skipped),
	]);
	let Some(PriorImport::Current(copied)) = prior(&copy) else {
		panic!("the copy has its own journal now");
	};
	assert_ne!(copied, first.journal);
	assert_eq!(prior(&alpha), Some(PriorImport::Current(first.journal.clone())));
	// Neither import supersedes the other: both files are pinned, as v1 pins
	// the id.
	assert_eq!(project_pins(&fixture), [stem(&first.journal), stem(&copied)]);

	// A change to one file is a change to that file's import only.
	grow(&alpha, "the original went on");
	assert_eq!(prior(&alpha), Some(PriorImport::Changed(first.journal.clone())));
	assert_eq!(prior(&copy), Some(PriorImport::Current(copied.clone())));
	let again = import_selected(&pairs[0], &alpha, &recorder).expect("pick");
	assert_eq!(again.previous.as_deref(), Some(first.journal.as_path()));
	assert_eq!(again.other_file, None);
	assert_eq!(project_pins(&fixture), [stem(&copied), stem(&again.journal)]);
}
