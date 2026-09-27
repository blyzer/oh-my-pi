//! The `secret-placeholder-key` step (owner decision #10): v1's transcript
//! placeholder key (`secret-placeholder.key`) becomes v2's, under the v2 state
//! root, so placeholders in imported v1 sessions keep resolving. A key v2
//! already minted is kept: every v2 transcript depends on it.

use std::fs;

use omp_cache::secret_key::{self, Adoption};
use zeroize::Zeroizing;

use super::{DataImportError, copied, same_file};
use crate::v1_import::{
	Attention, ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item,
	report::SkipReason,
};

pub(in crate::v1_import) fn import(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let v1 = cx.locate(V1Item::SecretPlaceholderKey);
	let target = secret_key::path_in(&cx.pair.target.state_dir);
	let (subject, outcome) = match &v1 {
		None => (None, ImportOutcome::NothingToImport),
		Some(path) if same_file(path, &target) => {
			(None, ImportOutcome::Skipped(SkipReason::SharedWithV2))
		},
		Some(path) => {
			let bytes = Zeroizing::new(fs::read(path).map_err(DataImportError::read(path))?);
			match str::from_utf8(&bytes)
				.ok()
				.map(str::trim)
				.filter(|key| secret_key::is_valid_key(key))
			{
				None => (
					None,
					ImportOutcome::NeedsAttention(Attention::Incompatible(
						DataImportError::InvalidPlaceholderKey { path: path.clone() }.into(),
					)),
				),
				Some(key) => {
					let found = match cx.mode {
						ImportMode::Apply => secret_key::adopt_at(&target, key),
						ImportMode::DryRun => secret_key::read_at(&target).map(|existing| {
							existing.map_or(Adoption::Installed, |existing| Adoption::Kept {
								same: existing == key,
							})
						}),
					}
					.map_err(DataImportError::PlaceholderKey)?;
					match found {
						Adoption::Installed => (None, copied(cx.mode)),
						Adoption::Kept { same: true } => {
							(None, ImportOutcome::Skipped(SkipReason::AlreadyPresent))
						},
						Adoption::Kept { same: false } => (
							Some(omp_core::Str::new_static(
								"v2 keeps its own key; secrets in imported v1 sessions stay as \
								 placeholders",
							)),
							ImportOutcome::Skipped(SkipReason::TargetExists),
						),
					}
				},
			}
		},
	};
	if cx.mode == ImportMode::Apply {
		ImportStep::SecretPlaceholderKey
			.marker(&cx.pair.target.config_dir)
			.set(None)
			.map_err(DataImportError::write(&cx.pair.target.config_dir))?;
	}
	Ok(vec![ImportEntry {
		step: ImportStep::SecretPlaceholderKey,
		item: V1Item::SecretPlaceholderKey,
		path: v1,
		subject,
		outcome,
	}])
}
