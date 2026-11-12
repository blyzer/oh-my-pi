//! A journal can accept a registered vocabulary without widening session
//! journals.

use omp_core::Str;
use omp_journal::{
	EntryDraft, Journal, JournalError, Kind, KindError, Vocabulary,
	kind::{JOURNAL, TURN_START},
};
use tempfile::tempdir;

fn kind(name: &'static str) -> Kind {
	Kind { name: Str::new_static(name), rev: 1 }
}

fn genesis() -> EntryDraft {
	EntryDraft {
		kind:  kind(JOURNAL),
		by:    None,
		prior: None,
		label: None,
		data:  Str::new_static(r#"{"version":1,"cwd":"/tmp","created":"now"}"#),
	}
}

fn run_kind() -> Kind {
	Kind::new("adw.run.start", 1).expect("valid kind")
}

fn run_vocabulary() -> Vocabulary {
	Vocabulary::builtin().with(run_kind()).expect("registered")
}

fn run_entry(by: omp_journal::EntryId) -> EntryDraft {
	EntryDraft {
		kind:  run_kind(),
		by:    Some(by),
		prior: None,
		label: None,
		data:  Str::new_static(r#"{"run":"r1"}"#),
	}
}

#[test]
fn a_session_journal_still_rejects_an_unregistered_kind() {
	let directory = tempdir().expect("tempdir");
	let mut journal = Journal::create(directory.path().join("session.oms")).expect("create");
	let genesis = journal.append(genesis()).expect("genesis");
	let refused = journal.append(run_entry(genesis.id));
	assert!(matches!(refused, Err(JournalError::UnknownKind { .. })), "{refused:?}");
}

#[test]
fn a_run_journal_accepts_its_registered_kind_and_reads_it_back() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	{
		let mut journal = Journal::create_with(&path, run_vocabulary()).expect("create");
		let genesis = journal.append(genesis()).expect("genesis");
		let written = journal.append(run_entry(genesis.id)).expect("run entry");
		assert_eq!(written.kind, run_kind());
	}
	let (_journal, entries) = Journal::open_with(&path, run_vocabulary()).expect("open");
	assert_eq!(entries.len(), 2);
	assert_eq!(entries[1].kind, run_kind());
	assert_eq!(
		Journal::scan_with(&path, &run_vocabulary())
			.expect("scan")
			.len(),
		2
	);
}

#[test]
fn a_session_reader_refuses_a_run_journal_instead_of_misreading_it() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	{
		let mut journal = Journal::create_with(&path, run_vocabulary()).expect("create");
		let genesis = journal.append(genesis()).expect("genesis");
		journal.append(run_entry(genesis.id)).expect("run entry");
	}
	assert!(matches!(Journal::scan(&path), Err(JournalError::NewerRevision { .. })));
	assert!(matches!(Journal::open(&path), Err(JournalError::NewerRevision { .. })));
}

#[test]
fn a_run_journal_accepts_only_what_it_registered() {
	let directory = tempdir().expect("tempdir");
	let mut journal =
		Journal::create_with(directory.path().join("run.oms"), run_vocabulary()).expect("create");
	let genesis = journal.append(genesis()).expect("genesis");
	let other = Kind::new("adw.phase.dispatch", 1).expect("valid kind");
	let refused = journal.append(EntryDraft {
		kind:  other,
		by:    Some(genesis.id),
		prior: None,
		label: None,
		data:  Str::new_static("{}"),
	});
	assert!(matches!(refused, Err(JournalError::UnknownKind { .. })), "{refused:?}");
}

#[test]
fn the_builtin_kinds_cannot_be_registered_again() {
	let known = kind(TURN_START);
	assert!(matches!(Vocabulary::builtin().with(known), Err(KindError::AlreadyKnown)));
}
