//! Exactly-once remote user messages: the key is journaled with the message.

use std::path::Path;

use omp_core::{Hash32, Str};
use omp_journal::{
	blob::BlobRef,
	data::Attachment,
	idempotency::{IdempotencyKey, IdempotencyKeyError},
};
use omp_session::{AppendOutcome, ComponentRegistry, Session, SessionError};

fn key(text: &'static str) -> Option<IdempotencyKey> {
	Some(IdempotencyKey::new_static(text))
}

fn create(path: &Path) -> Session {
	let mut session = Session::create(path, ComponentRegistry::default()).expect("session creates");
	session.begin_turn().expect("turn starts");
	session
}

fn reopen(path: &Path) -> Session {
	Session::open(path, ComponentRegistry::default()).expect("session reopens")
}

fn user_nodes(session: &Session) -> usize {
	session
		.dom()
		.select("body turn user")
		.expect("selector")
		.count()
}

#[test]
fn a_retry_with_the_same_key_replays_the_original_entry() {
	let directory = tempfile::tempdir().expect("temporary session directory");
	let mut session = create(&directory.path().join("replay.oms"));

	let first = session
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("first submit");
	let AppendOutcome::Appended(id) = first else {
		panic!("the first submit appends: {first:?}");
	};
	let entries = session.entries().len();

	let retry = session
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("retry");
	assert_eq!(retry, AppendOutcome::Replayed(id));
	assert_eq!(retry.entry(), id);
	assert_eq!(session.entries().len(), entries, "a replay appends nothing");
	assert_eq!(user_nodes(&session), 1);
}

#[test]
fn replay_survives_dropping_the_session_and_reopening_the_journal() {
	let directory = tempfile::tempdir().expect("temporary session directory");
	let path = directory.path().join("reopen.oms");
	let mut session = create(&path);
	let id = session
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("first submit")
		.entry();
	drop(session);

	let mut reopened = reopen(&path);
	let entries = reopened.entries().len();
	let retry = reopened
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("retry after reopen");
	assert_eq!(retry, AppendOutcome::Replayed(id));
	assert_eq!(reopened.entries().len(), entries, "the journaled key answers, not memory");
	assert_eq!(user_nodes(&reopened), 1);
}

#[test]
fn a_different_key_appends_another_message() {
	let directory = tempfile::tempdir().expect("temporary session directory");
	let mut session = create(&directory.path().join("keys.oms"));

	let first = session
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("first submit")
		.entry();
	let second = session
		.user_authored("deploy", Vec::new(), "Ada", key("K2"))
		.expect("second submit");
	assert!(matches!(second, AppendOutcome::Appended(id) if id != first), "{second:?}");
	assert_eq!(user_nodes(&session), 2);
}

#[test]
fn the_same_key_with_different_content_is_a_conflict() {
	let directory = tempfile::tempdir().expect("temporary session directory");
	let mut session = create(&directory.path().join("conflict.oms"));
	let id = session
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("first submit")
		.entry();
	let entries = session.entries().len();

	let text = session
		.user_authored("rollback", Vec::new(), "Ada", key("K"))
		.expect_err("different text under the same key");
	assert!(
		matches!(&text, SessionError::KeyConflict { key, existing } if key.as_str() == "K" && *existing == id),
		"{text:?}"
	);

	let image = Attachment {
		blob: BlobRef { hash: Hash32::new([0xab; 32]), size: 5 },
		mime: Str::new_static("image/png"),
	};
	let attachments = session
		.user_authored("deploy", vec![image], "Ada", key("K"))
		.expect_err("different attachments under the same key");
	assert!(matches!(attachments, SessionError::KeyConflict { existing, .. } if existing == id));

	assert_eq!(session.entries().len(), entries, "a conflict appends nothing");
	assert_eq!(user_nodes(&session), 1);
}

#[test]
fn messages_without_a_key_always_append() {
	let directory = tempfile::tempdir().expect("temporary session directory");
	let path = directory.path().join("keyless.oms");
	let mut session = create(&path);

	let first = session
		.user_authored("deploy", Vec::new(), "Ada", None)
		.expect("first");
	let second = session
		.user_authored("deploy", Vec::new(), "Ada", None)
		.expect("second");
	assert!(matches!(first, AppendOutcome::Appended(_)));
	assert!(matches!(second, AppendOutcome::Appended(_)));
	assert_ne!(first.entry(), second.entry());
	assert_eq!(user_nodes(&session), 2);

	drop(session);
	let mut reopened = reopen(&path);
	let entries = reopened.entries().len();
	let keyed = reopened
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("a keyed submit after keyless history");
	assert!(matches!(keyed, AppendOutcome::Appended(_)), "keyless entries are never indexed");
	assert_eq!(reopened.entries().len(), entries + 1);
}

#[test]
fn an_out_of_bounds_key_is_refused_before_anything_is_journaled() {
	let directory = tempfile::tempdir().expect("temporary session directory");
	let mut session = create(&directory.path().join("invalid.oms"));
	let entries = session.entries().len();

	let empty = session
		.user_authored("deploy", Vec::new(), "Ada", Some(IdempotencyKey::new("")))
		.expect_err("empty key");
	assert!(
		matches!(empty, SessionError::InvalidIdempotencyKey(IdempotencyKeyError::Empty)),
		"{empty:?}"
	);
	let oversized = IdempotencyKey::new("k".repeat(IdempotencyKey::MAX_BYTES + 1));
	let long = session
		.user_authored("deploy", Vec::new(), "Ada", Some(oversized))
		.expect_err("oversized key");
	assert!(
		matches!(long, SessionError::InvalidIdempotencyKey(IdempotencyKeyError::TooLong { .. })),
		"{long:?}"
	);
	assert_eq!(session.entries().len(), entries);
}

#[test]
fn rewinding_past_a_keyed_message_frees_its_key_and_reopen_agrees() {
	let directory = tempfile::tempdir().expect("temporary session directory");
	let path = directory.path().join("rewind.oms");
	let mut session = Session::create(&path, ComponentRegistry::default()).expect("session creates");
	let turn = session.begin_turn().expect("turn starts");
	let abandoned = session
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("first submit")
		.entry();

	session.rewind(turn).expect("rewind succeeds");
	session.begin_turn().expect("branch turn starts");
	let retried = session
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("retry on the new branch");
	let AppendOutcome::Appended(live) = retried else {
		panic!("the abandoned branch no longer holds the key: {retried:?}");
	};
	assert_ne!(live, abandoned);
	drop(session);

	let mut reopened = reopen(&path);
	let again = reopened
		.user_authored("deploy", Vec::new(), "Ada", key("K"))
		.expect("retry after reopen");
	assert_eq!(again, AppendOutcome::Replayed(live), "the index follows the live chain");
}


#[test]
fn keyed_patches_replay_after_reopen_and_share_the_user_key_index() {
	use omp_dom::{Op, PropKey, Txn, Value};

	fn change(session: &Session) -> Txn {
		Txn {
			cause: session.head().expect("session has a head"),
			label: Some(Str::new_static("approval.decide")),
			ops: vec![Op::Set {
				h: session.dom().meta(),
				prop: PropKey::Custom(Str::new_static("keyed-test")),
				value: Value::Str(Str::new_static("changed")),
			}],
		}
	}

	let directory = tempfile::tempdir().expect("temporary session directory");
	let path = directory.path().join("patch-replay.oms");
	let mut session = create(&path);
	let key = IdempotencyKey::new_static("K");
	let first = session.patch_keyed(change(&session), key.clone(), &"allow").expect("first patch");
	let AppendOutcome::Appended(id) = first else { panic!("first patch must append") };
	let entries = session.entries().len();
	let retry = session.patch_keyed(change(&session), key.clone(), &"allow").expect("retry");
	assert_eq!(retry, AppendOutcome::Replayed(id));
	assert_eq!(session.entries().len(), entries);
	assert!(matches!(
		session.patch_keyed(change(&session), key.clone(), &"deny"),
		Err(SessionError::KeyConflict { existing, .. }) if existing == id
	));
	assert!(matches!(
		session.user_authored("allow", Vec::new(), "Ada", Some(key.clone())),
		Err(SessionError::KeyConflict { existing, .. }) if existing == id
	));
	drop(session);

	let mut reopened = reopen(&path);
	let entries = reopened.entries().len();
	assert_eq!(
		reopened.replay_keyed_patch(&key, "approval.decide", &"allow").expect("lookup"),
		Some(id)
	);
	assert_eq!(
		reopened.patch_keyed(change(&reopened), key, &"allow").expect("replay"),
		AppendOutcome::Replayed(id)
	);
	assert_eq!(reopened.entries().len(), entries);
}

