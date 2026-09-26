//! Sessions-step proofs over temporary homes, v1 trees, and v2 roots, with a
//! recording converter standing in for the app's `ForeignFormat::Omp1`
//! importer (whose conversion `omp-app`'s `session_import` tests prove).

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use omp_core::Str;
use parking_lot::Mutex;
use serde_json::json;

use super::{super::*, ConvertError, import_selected, list};

/// One call the converter saw.
#[derive(Clone, Debug)]
struct Call {
	source:    PathBuf,
	id:        String,
	blobs:     Option<PathBuf>,
	artifacts: Option<PathBuf>,
	children:  Vec<V1ChildJob>,
}

/// Writes a one-message journal per transcript and records every call; the
/// transcript named `fail` fails after writing a partial destination.
#[derive(Default)]
struct Recorder {
	calls: Mutex<Vec<Call>>,
	fail:  Option<PathBuf>,
}

impl V1SessionConverter for Recorder {
	fn convert(&self, conversion: &V1Conversion<'_>) -> Result<usize, ConvertError> {
		self.calls.lock().push(Call {
			source:    conversion.source.to_owned(),
			id:        conversion.id.to_owned(),
			blobs:     conversion.blobs.map(Path::to_owned),
			artifacts: conversion.artifacts.map(Path::to_owned),
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
	assert_eq!(recorder.sources(), [scout, alpha.clone(), gone.clone()]);
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
	assert_eq!(calls[1].artifacts.as_deref(), Some(alpha.with_extension("").as_path()));
	assert_eq!(calls[1].blobs.as_deref(), Some(blobs.as_path()));
	assert_eq!(calls[2].artifacts, None);
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

	// A second run is a no-op through the marker.
	let v2_before = snapshot(&fixture.v2.data_dir);
	let again = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert!(matches!(
		session_entries(&again)[0].outcome,
		ImportOutcome::Skipped(SkipReason::MarkerPresent)
	));
	// Without the marker, the per-session records still skip every session.
	fs::remove_file(ImportStep::Sessions.marker(&fixture.v2.config_dir).path()).expect("unmark");
	let records = run_with(&pairs, ImportMode::Apply, CredentialAccess::Offline(&offline), bulk);
	assert_eq!(outcomes(&records)[..2], [
		(Some("alpha"), OutcomeKind::Skipped),
		(Some("gone (project directory gone: no-directory)"), OutcomeKind::Skipped),
	]);
	assert_eq!(recorder.sources().len(), 3, "nothing converts twice");
	assert_eq!(snapshot(&fixture.v2.data_dir), v2_before);
}

#[test]
fn the_picker_lists_without_converting_and_converts_on_pick() {
	let fixture = Fixture::new();
	let alpha = fixture.session(None, "alpha", &fixture.project);
	fixture.child(&alpha, "0-Scout", "scout");
	fixture.session(None, "gone", &fixture.root.path().join("vanished"));
	let pairs = fixture.pairs(&ProfileSelection::All);
	let recorder = Recorder::default();

	let rows = list(&pairs[0].source).expect("list");
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
	assert!(work.data_dir.join("v1-sessions/w1.json").is_file());
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
