//! The run journal chains its records, resumes only the same definition, and
//! stops at a terminal record.

use omp_core::Str;
use omp_journal::{
	Journal, JournalError,
	idempotency::IdempotencyKey,
	runlog::{
		EffectKey, InputVersion, PhaseDispatch, PhaseSettle, RunDone, RunHalt, RunLog, RunLogError,
		RunRecord, RunStart, SettleStatus, vocabulary,
	},
};
use tempfile::tempdir;

fn start(digest: &str) -> RunStart {
	RunStart {
		run:                 Str::new_static("01RUN"),
		workflow:            Str::new_static("ship"),
		workflow_digest:     Str::new(digest),
		input_digest:        Str::new_static("empty"),
		max_attempts:        3,
		model:               Str::new_static("default"),
		model_explicit:      false,
		exec_policy:         Str::new_static("local-only"),
		exec_fallback_local: false,
		workspace_trusted:   true,
		workspace_root:      Str::new_static("/work/app"),
		workspace_head:      Some(Str::new_static("abc123")),
		created:             Str::new_static("2026-11-12T00:00:00Z"),
	}
}

fn dispatch(attempt: u32) -> RunRecord {
	RunRecord::Dispatch(PhaseDispatch {
		phase: Str::new_static("build"),
		attempt,
		inputs: vec![InputVersion { phase: Str::new_static("plan"), version: 1 }],
		executor: Str::new_static("native"),
		posture: Str::new_static("write=workspace-write network=disabled approval=never"),
		effects: vec![EffectKey {
			effect: Str::new_static("pr.create"),
			key:    IdempotencyKey::parse(&"a".repeat(64)).expect("key"),
		}],
	})
}

fn settle(attempt: u32, status: SettleStatus) -> RunRecord {
	RunRecord::Settle(PhaseSettle {
		phase: Str::new_static("build"),
		attempt,
		status,
		feedback: Str::empty(),
	})
}

#[test]
fn records_round_trip_in_order_and_chain_through_by() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	{
		let mut log = RunLog::create(&path, start("d1")).expect("create");
		log.append(&dispatch(1)).expect("dispatch");
		log.append(&settle(1, SettleStatus::Accepted))
			.expect("settle");
	}
	let records = RunLog::read(&path).expect("read");
	assert_eq!(records.len(), 3);
	assert_eq!(records[1], dispatch(1));
	assert_eq!(records[2], settle(1, SettleStatus::Accepted));
	let entries = Journal::scan_with(&path, &vocabulary()).expect("scan");
	assert_eq!(entries.len(), 4);
	assert!(entries[0].by.is_none());
	for pair in entries.windows(2) {
		assert_eq!(pair[1].by, Some(pair[0].id), "entries must chain through by");
		assert!(!pair[1].data.contains('\n'));
	}
}

#[test]
fn resume_returns_the_records_and_appends_after_them() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	RunLog::create(&path, start("d1"))
		.expect("create")
		.append(&dispatch(1))
		.expect("dispatch");
	let (mut log, records) = RunLog::open(&path, "d1").expect("open");
	assert_eq!(records.len(), 2);
	log.append(&settle(1, SettleStatus::Failed))
		.expect("append");
	drop(log);
	assert_eq!(RunLog::read(&path).expect("read").len(), 3);
}

#[test]
fn resume_refuses_a_different_workflow_definition() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	RunLog::create(&path, start("d1")).expect("create");
	let refused = RunLog::open(&path, "d2");
	assert!(matches!(refused, Err(RunLogError::DigestMismatch { .. })), "{refused:?}");
}

#[test]
fn a_finished_run_accepts_no_more_records() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	let mut log = RunLog::create(&path, start("d1")).expect("create");
	log.append(&RunRecord::Done(RunDone { accepted: true }))
		.expect("done");
	assert!(matches!(log.append(&dispatch(1)), Err(RunLogError::Finished)));
	drop(log);
	let (mut reopened, _) = RunLog::open(&path, "d1").expect("open");
	assert!(matches!(reopened.append(&dispatch(1)), Err(RunLogError::Finished)));
}

#[test]
fn a_halt_does_not_end_the_run_and_it_can_be_resumed() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	let mut log = RunLog::create(&path, start("d1")).expect("create");
	log.append(&RunRecord::Halt(RunHalt { reason: Str::new_static("cancelled") }))
		.expect("halt");
	log.append(&dispatch(1))
		.expect("a halted run accepts more records");
	drop(log);
	let (mut reopened, records) = RunLog::open(&path, "d1").expect("open");
	assert_eq!(records.len(), 3);
	reopened
		.append(&settle(1, SettleStatus::Accepted))
		.expect("resumed");
}

#[test]
fn an_entry_that_does_not_name_the_previous_one_breaks_the_chain() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	{
		let mut journal = Journal::create_with(&path, vocabulary()).expect("create");
		let genesis = journal
			.append(omp_journal::EntryDraft {
				kind:  omp_journal::Kind::new(omp_journal::kind::JOURNAL, 1).expect("kind"),
				by:    None,
				prior: None,
				label: None,
				data:  Str::new_static(r#"{"version":1,"cwd":"/tmp","created":"now"}"#),
			})
			.expect("genesis");
		let draft = |record: &RunRecord, by| {
			let (kind, data) = record.encode().expect("encode");
			omp_journal::EntryDraft { kind, by: Some(by), prior: None, label: None, data }
		};
		journal
			.append(draft(&RunRecord::Start(start("d1")), genesis.id))
			.expect("start");
		journal
			.append(draft(&dispatch(1), genesis.id))
			.expect("dispatch");
	}
	assert!(matches!(RunLog::read(&path), Err(RunLogError::BrokenChain { index: 2 })));
	assert!(matches!(RunLog::open(&path, "d1"), Err(RunLogError::BrokenChain { index: 2 })));
}

#[test]
fn an_effect_key_that_is_not_a_64_character_hex_digest_is_refused() {
	let directory = tempdir().expect("tempdir");
	let mut log = RunLog::create(directory.path().join("run.oms"), start("d1")).expect("create");
	let short = RunRecord::Dispatch(PhaseDispatch {
		phase:    Str::new_static("build"),
		attempt:  1,
		inputs:   vec![],
		executor: Str::new_static("native"),
		posture:  Str::new_static("p"),
		effects:  vec![EffectKey {
			effect: Str::new_static("pr.create"),
			key:    IdempotencyKey::parse("short").expect("parses"),
		}],
	});
	assert!(matches!(log.append(&short), Err(RunLogError::BadEffectKey { .. })));
	let not_hex = RunRecord::Dispatch(PhaseDispatch {
		effects: vec![EffectKey {
			effect: Str::new_static("pr.create"),
			key:    IdempotencyKey::parse(&"z".repeat(64)).expect("parses"),
		}],
		..PhaseDispatch {
			phase:    Str::new_static("build"),
			attempt:  1,
			inputs:   vec![],
			executor: Str::new_static("native"),
			posture:  Str::new_static("p"),
			effects:  vec![],
		}
	});
	assert!(matches!(log.append(&not_hex), Err(RunLogError::BadEffectKey { .. })));
}

#[test]
fn an_oversized_rejection_feedback_is_cut_so_the_settle_can_still_be_written() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	let mut log = RunLog::create(&path, start("d1")).expect("create");
	let huge = Str::new("x".repeat(omp_journal::runlog::FEEDBACK_CAP * 2));
	log.append(&RunRecord::Settle(PhaseSettle {
		phase:    Str::new_static("build"),
		attempt:  1,
		status:   SettleStatus::Rejected,
		feedback: huge,
	}))
	.expect("an oversized feedback must not strand the phase");
	drop(log);
	let records = RunLog::read(&path).expect("read");
	let RunRecord::Settle(settled) = &records[1] else {
		panic!("settle expected")
	};
	assert!(settled.feedback.as_str().ends_with(" bytes]"));
	assert!(settled.feedback.len() < omp_journal::runlog::FEEDBACK_CAP + 64);
}

#[test]
fn a_journal_that_does_not_begin_with_a_start_record_is_refused() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	{
		let mut journal = Journal::create_with(&path, vocabulary()).expect("create");
		let genesis = journal
			.append(omp_journal::EntryDraft {
				kind:  omp_journal::Kind::new(omp_journal::kind::JOURNAL, 1).expect("kind"),
				by:    None,
				prior: None,
				label: None,
				data:  Str::new_static(r#"{"version":1,"cwd":"/tmp","created":"now"}"#),
			})
			.expect("genesis");
		let (kind, data) = dispatch(1).encode().expect("encode");
		journal
			.append(omp_journal::EntryDraft {
				kind,
				by: Some(genesis.id),
				prior: None,
				label: None,
				data,
			})
			.expect("dispatch");
	}
	assert!(matches!(RunLog::open(&path, "d1"), Err(RunLogError::MissingStart)));
}

#[test]
fn a_session_reader_refuses_a_run_journal() {
	let directory = tempdir().expect("tempdir");
	let path = directory.path().join("run.oms");
	RunLog::create(&path, start("d1")).expect("create");
	assert!(matches!(Journal::scan(&path), Err(JournalError::NewerRevision { .. })));
}
