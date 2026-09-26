//! The `history` step: v1 `history.db` prompts into `<data>/history.db`.
//!
//! Both versions share one schema (`history(id, prompt UNIQUE, created_at,
//! cwd, session_id)` plus the `history_fts` index over it). Without a v2
//! database the v1 one is copied whole, and v2 migrates it on open (a v1
//! `user_version` below v2's rebuilds the table and its index). Otherwise the
//! v1 rows are merged with `INSERT OR IGNORE` on the prompt, so v2's copy of
//! a prompt and its provenance win; v2's `history_ai` trigger indexes each
//! inserted row, and a database without it has its index rebuilt.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension as _};

use super::{DataImportError, copied, counted, same_file, sqlite::V1Database};
use crate::v1_import::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item,
	report::SkipReason,
};

/// v2's prompt-history database under a profile data directory.
const HISTORY_DB: &str = "history.db";

pub(in crate::v1_import) fn import(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let entry = |path: &Path, subject, outcome| ImportEntry {
		step: ImportStep::History,
		item: V1Item::HistoryDb,
		path: Some(path.to_owned()),
		subject,
		outcome,
	};
	let Some(v1) = cx.locate(V1Item::HistoryDb) else {
		finish(cx)?;
		return Ok(vec![ImportEntry::new(
			ImportStep::History,
			V1Item::HistoryDb,
			None,
			ImportOutcome::NothingToImport,
		)]);
	};
	let v2 = cx.pair.target.data_dir.join(HISTORY_DB);
	if same_file(&v1, &v2) {
		finish(cx)?;
		return Ok(vec![entry(&v1, None, ImportOutcome::Skipped(SkipReason::SharedWithV2))]);
	}
	let source = V1Database::open(&v1)?;
	let merge = if v2.is_file() {
		merge(&source, &v2, cx.mode)?
	} else {
		let total = count(&source.connect()?, source.path())?;
		if total > 0 && cx.mode == ImportMode::Apply {
			source.copy_to(&v2)?;
		}
		Merge { new: total, kept: 0 }
	};
	finish(cx)?;
	let mut entries = Vec::with_capacity(2);
	if merge.new > 0 {
		entries.push(entry(&v1, Some(counted(None, merge.new, "prompt", "")), copied(cx.mode)));
	}
	if merge.kept > 0 {
		entries.push(entry(
			&v1,
			Some(counted(None, merge.kept, "prompt", " already in v2")),
			ImportOutcome::Skipped(SkipReason::TargetExists),
		));
	}
	if entries.is_empty() {
		entries.push(entry(&v1, None, ImportOutcome::NothingToImport));
	}
	Ok(entries)
}

fn finish(cx: &StepContext<'_>) -> Result<(), ImportError> {
	if cx.mode == ImportMode::Apply {
		ImportStep::History
			.marker(&cx.pair.target.config_dir)
			.set(None)
			.map_err(DataImportError::write(&cx.pair.target.config_dir))?;
	}
	Ok(())
}

/// Prompts the v1 database adds, and prompts v2 already had.
struct Merge {
	new:  usize,
	kept: usize,
}

fn count(connection: &Connection, path: &Path) -> Result<usize, DataImportError> {
	connection
		.query_row("SELECT count(*) FROM history", [], |row| row.get::<_, usize>(0))
		.map_err(DataImportError::sqlite(path))
}

/// Merges (or, in a dry run, counts) the v1 rows missing from the existing
/// v2 database at `v2`.
fn merge(source: &V1Database, v2: &Path, mode: ImportMode) -> Result<Merge, DataImportError> {
	let sql = DataImportError::sqlite;
	// A dry run reads v2 as immutably as v1; the snapshot outlives the
	// connection.
	let read_only;
	let connection = match mode {
		ImportMode::Apply => {
			let connection = Connection::open_with_flags(
				v2,
				OpenFlags::SQLITE_OPEN_READ_WRITE
					| OpenFlags::SQLITE_OPEN_URI
					| OpenFlags::SQLITE_OPEN_NO_MUTEX,
			)
			.map_err(sql(v2))?;
			connection
				.busy_timeout(std::time::Duration::from_secs(2))
				.map_err(sql(v2))?;
			connection
		},
		ImportMode::DryRun => {
			read_only = V1Database::open(v2)?;
			read_only.connect()?
		},
	};
	connection
		.execute("ATTACH DATABASE ?1 AS v1", [source.uri()])
		.map_err(sql(source.path()))?;
	let total = connection
		.query_row("SELECT count(*) FROM v1.history", [], |row| row.get::<_, usize>(0))
		.map_err(sql(source.path()))?;
	let new = match mode {
		ImportMode::DryRun => connection
			.query_row(
				"SELECT count(*) FROM v1.history WHERE prompt NOT IN (SELECT prompt FROM main.history)",
				[],
				|row| row.get::<_, usize>(0),
			)
			.map_err(sql(v2))?,
		ImportMode::Apply => {
			let columns = v1_columns(&connection).map_err(sql(source.path()))?;
			let select = |name: &'static str| {
				if columns.iter().any(|column| column == name) {
					name
				} else {
					"NULL"
				}
			};
			let created = if columns.iter().any(|column| column == "created_at") {
				"created_at"
			} else {
				"0"
			};
			let insert = format!(
				"INSERT OR IGNORE INTO main.history(prompt, created_at, cwd, session_id)
				 SELECT prompt, {created}, {}, {} FROM v1.history ORDER BY rowid",
				select("cwd"),
				select("session_id"),
			);
			let transaction = connection.unchecked_transaction().map_err(sql(v2))?;
			let new = transaction.execute(&insert, []).map_err(sql(v2))?;
			if new > 0 && !indexed_by_trigger(&transaction).map_err(sql(v2))? {
				transaction
					.execute("INSERT INTO main.history_fts(history_fts) VALUES('rebuild')", [])
					.map_err(sql(v2))?;
			}
			transaction.commit().map_err(sql(v2))?;
			new
		},
	};
	connection
		.execute("DETACH DATABASE v1", [])
		.map_err(sql(source.path()))?;
	Ok(Merge { new, kept: total.saturating_sub(new) })
}

/// The v1 `history` table's columns (early v1 builds lacked `session_id`).
fn v1_columns(connection: &Connection) -> rusqlite::Result<Vec<String>> {
	let mut statement = connection.prepare("SELECT name FROM pragma_table_info('history', 'v1')")?;
	statement
		.query_map([], |row| row.get::<_, String>(0))?
		.collect()
}

/// Whether v2's insert trigger keeps `history_fts` in step with `history`.
fn indexed_by_trigger(connection: &Connection) -> rusqlite::Result<bool> {
	connection
		.query_row(
			"SELECT 1 FROM main.sqlite_master WHERE type = 'trigger' AND name = 'history_ai'",
			[],
			|_| Ok(()),
		)
		.optional()
		.map(|found| found.is_some())
}
