//! The `models` and `models-keys` steps: v1 model config into `models.toml`,
//! and its literal `apiKey`s into the encrypted credential store.

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item,
	report::{Attention, SkipReason},
};
use crate::discovery::models::{
	LegacyApiKeyImport, LegacyKeyTarget, LegacyModelsImport, ModelsConfigLocation,
	ModelsConfigSource, import_legacy_api_keys, import_legacy_models,
};

pub(super) fn import_models(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let location = ModelsConfigLocation::for_pair(cx.pair);
	let (path, outcome) = match import_legacy_models(&location, cx.mode)? {
		LegacyModelsImport::NativePresent(native) => {
			(Some(native), ImportOutcome::Skipped(SkipReason::TargetExists))
		},
		LegacyModelsImport::AlreadyImported => {
			(location.legacy_path(), ImportOutcome::Skipped(SkipReason::MarkerPresent))
		},
		LegacyModelsImport::NothingToImport => (None, ImportOutcome::NothingToImport),
		LegacyModelsImport::Imported(loaded) => {
			let path = match loaded.source {
				ModelsConfigSource::NativeToml(path)
				| ModelsConfigSource::MovedToml(path)
				| ModelsConfigSource::LegacyJson(path)
				| ModelsConfigSource::LegacyYaml(path) => path,
			};
			(Some(path), match cx.mode {
				ImportMode::Apply => ImportOutcome::Imported,
				ImportMode::DryRun => ImportOutcome::WouldImport,
			})
		},
	};
	Ok(vec![ImportEntry::new(ImportStep::Models, V1Item::Models, path, outcome)])
}

pub(super) fn import_keys(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let location = ModelsConfigLocation::for_pair(cx.pair);
	let target = match cx.mode {
		ImportMode::DryRun => LegacyKeyTarget::DryRun,
		ImportMode::Apply => {
			LegacyKeyTarget::Store(cx.credentials.ok_or(ImportError::NoCredentialStore)?)
		},
	};
	let path = location.legacy_path();
	let report = import_legacy_api_keys(&location, target)?;
	if report.is_empty() {
		return Ok(vec![ImportEntry::new(
			ImportStep::ModelsKeys,
			V1Item::Models,
			path,
			ImportOutcome::NothingToImport,
		)]);
	}
	Ok(report
		.into_iter()
		.map(|key| {
			let (provider, outcome) = match key {
				LegacyApiKeyImport::Stored { provider } => (provider, ImportOutcome::Imported),
				LegacyApiKeyImport::WouldStore { provider } => (provider, ImportOutcome::WouldImport),
				LegacyApiKeyImport::AlreadyLoggedIn { provider } => {
					(provider, ImportOutcome::Skipped(SkipReason::AccountExists))
				},
				LegacyApiKeyImport::NeedsEnvironment { provider } => {
					(provider, ImportOutcome::NeedsAttention(Attention::KeyNeedsEnvironment))
				},
			};
			ImportEntry {
				step: ImportStep::ModelsKeys,
				item: V1Item::Models,
				path: path.clone(),
				subject: Some(provider),
				outcome,
			}
		})
		.collect())
}
