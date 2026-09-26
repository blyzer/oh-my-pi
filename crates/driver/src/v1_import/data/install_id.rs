//! The `install-id` step: v1's per-install id (`~/.omp/install-id`) becomes
//! the profile's `<data>/install-id`, the id v2 attributes gateway usage to,
//! unless v2 already minted its own.

use std::{fs, io};

use super::{DataImportError, copied};
use crate::v1_import::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item,
	report::SkipReason, step::atomic_replace,
};

/// v2's install id file under a profile data directory
/// (`omp_ai::auth::attribution`).
const INSTALL_ID: &str = "install-id";

pub(in crate::v1_import) fn import(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let outcome = |path, subject, outcome| ImportEntry {
		step: ImportStep::InstallId,
		item: V1Item::InstallId,
		path,
		subject,
		outcome,
	};
	let v1 = cx.locate(V1Item::InstallId);
	let id = match &v1 {
		Some(path) => fs::read_to_string(path).map_err(DataImportError::read(path))?,
		None => String::new(),
	};
	let id = id.trim();
	let target = cx.pair.target.data_dir.join(INSTALL_ID);
	let existing = match fs::read_to_string(&target) {
		Ok(existing) => Some(existing),
		Err(error) if error.kind() == io::ErrorKind::NotFound => None,
		Err(source) => return Err(DataImportError::Read { path: target, source }.into()),
	};
	let existing = existing
		.as_deref()
		.map(str::trim)
		.filter(|id| !id.is_empty());
	let entry = if id.is_empty() {
		outcome(v1, None, ImportOutcome::NothingToImport)
	} else if let Some(existing) = existing {
		let subject = if existing == id {
			"v2 already has this id"
		} else {
			"v2 keeps its own, different id"
		};
		outcome(
			v1,
			Some(omp_core::Str::new_static(subject)),
			ImportOutcome::Skipped(SkipReason::TargetExists),
		)
	} else {
		if cx.mode == ImportMode::Apply {
			let data = &cx.pair.target.data_dir;
			fs::create_dir_all(data).map_err(DataImportError::write(data))?;
			atomic_replace(&target, id.as_bytes()).map_err(DataImportError::write(&target))?;
		}
		outcome(v1, None, copied(cx.mode))
	};
	if cx.mode == ImportMode::Apply {
		ImportStep::InstallId
			.marker(&cx.pair.target.config_dir)
			.set(None)
			.map_err(DataImportError::write(&cx.pair.target.config_dir))?;
	}
	Ok(vec![entry])
}
