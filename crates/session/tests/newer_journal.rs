//! A journal written by a newer omp is refused by `open`, with a typed error.

use std::fs;

use omp_journal::JournalError;
use omp_session::{ComponentRegistry, Session, SessionError};

const GENESIS: &str = "event: journal@1\nid: 01ARZ3NDEKTSV4RRFFQ69G5FAV\ndata: {\"version\":VERSION,\"cwd\":\"/\",\"created\":\"1\"}\n\n";

#[test]
fn open_reports_a_newer_kind_revision_instead_of_a_generic_frame_error() {
	let directory = tempfile::tempdir().expect("tempdir");
	let path = directory.path().join("newer.oms");
	let text = format!(
		"{}event: msg.user@2\nid: 01ARZ3NDEKTSV4RRFFQ69G5FAW\nby: 01ARZ3NDEKTSV4RRFFQ69G5FAV\ndata: {{}}\n\n",
		GENESIS.replace("VERSION", "1")
	);
	fs::write(&path, &text).expect("write");

	let Err(error) = Session::open(&path, ComponentRegistry::standard()) else {
		panic!("a newer journal must not open");
	};
	let SessionError::Journal(JournalError::NewerRevision { kind }) = error else {
		panic!("expected NewerRevision, got {error:?}");
	};
	assert_eq!(kind.to_string(), "msg.user@2");
	assert_eq!(fs::read_to_string(&path).expect("read"), text, "a refused journal is untouched");
}

#[test]
fn open_reports_a_newer_genesis_version() {
	let directory = tempfile::tempdir().expect("tempdir");
	let path = directory.path().join("future.oms");
	fs::write(&path, GENESIS.replace("VERSION", "999")).expect("write");

	let Err(error) = Session::open(&path, ComponentRegistry::standard()) else {
		panic!("a newer genesis version must not open");
	};
	assert!(matches!(
		error,
		SessionError::Journal(JournalError::NewerGenesisVersion { found: 999, .. })
	));
}
