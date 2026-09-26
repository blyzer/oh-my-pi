//! One-shot import of a v1 (TypeScript `omp`) install into v2.
//!
//! # Shape
//!
//! - [`locate`]: where v1 keeps each [`V1Item`], resolved exactly as v1's
//!   `dirs.ts` did (`PI_CONFIG_DIR`, `PI_CODING_AGENT_DIR`,
//!   `OMP_PROFILE`/`PI_PROFILE`, XDG relocation), or from an explicit `--from`
//!   root. The `PI_*` variables are read there and nowhere else in v2. [`plan`]
//!   pairs each v1 profile with the same-named v2 profile
//!   ([`ProfileSelection`], owner decision #8), and
//!   [`ImportPair::xdg_collisions`] reports a v1 XDG root v2 shares (decision
//!   #9; never acted on).
//! - [`step`]: the registered [`ImportStep`]s, each idempotent through its own
//!   [`Marker`] in the v2 profile configuration root (`.<step>-migration-v1`),
//!   and the [`run`] loop.
//! - [`report`]: the typed [`ImportReport`], rendered once at the app boundary.
//!
//! Imports only copy (decision #2): nothing under a v1 root is ever written,
//! moved, or deleted.
//!
//! # Entry points
//!
//! - [`first_run`]: the automatic hook production composition calls once its
//!   credential store exists. It imports every v1 profile, logs the report, and
//!   never fails startup.
//! - `omp config import-v1` (in `omp-app`): [`plan`] + [`run`] with
//!   [`ImportMode::DryRun`] or [`ImportMode::Apply`] and
//!   [`CredentialAccess::Offline`].
//!
//! # Adding a step
//!
//! Each later migrator PR adds one step:
//!
//! 1. Add a variant to [`ImportStep`]. Its kebab-case name is its report
//!    identity and names its marker; declaration order is run order.
//! 2. Map it to the [`V1Item`] it reads in [`ImportStep::item`], adding the
//!    item to [`V1Item`] (with its v1 anchor and candidate names) if it is new.
//!    Set [`ImportStep::needs_credentials`] if it writes the encrypted store.
//! 3. Write `fn import_x(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>,
//!    ImportError>` in its own module and dispatch it from `ImportStep::run`.
//!    The runner has already skipped it when its marker is set, and turns an
//!    `Err` into [`Attention::Failed`]. The step:
//!    - reads v1 through [`StepContext::locate`] / [`StepContext::pair`], never
//!      through paths of its own;
//!    - in [`ImportMode::DryRun`] reads only and reports
//!      [`ImportOutcome::WouldImport`];
//!    - in [`ImportMode::Apply`] copies into [`V2Target`] and then sets
//!      `ImportStep::X.marker(&target.config_dir)` — including when there was
//!      nothing to import, so a v1 file appearing later is not picked up;
//!    - reports one [`ImportEntry`] per item or sub-item, using
//!      [`ImportOutcome::NotMigratable`] and [`ImportOutcome::NeedsAttention`]
//!      for what v2 cannot take or the owner must do.
//! 4. Test it at this seam with a temporary home, v1 tree, and v2 roots: dry
//!    run writes nothing, a second run is a no-op, and the v1 tree is
//!    byte-identical afterwards.

mod assets;
mod auth_credentials;
mod credentials;
pub mod locate;
mod models;
pub mod report;
mod settings;
pub mod step;

#[cfg(test)]
mod tests;

use std::path::Path;

pub use assets::{AssetError, import_project_assets, project_assets_marker};
pub use auth_credentials::CredentialsImportError;
pub use locate::{
	ImportPair, ItemShape, LocateError, ProfileSelection, V1Inputs, V1Item, V1Layout, V1Source,
	V2Roots, V2Target, XdgCategory, XdgCollision, plan,
};
pub use report::{
	Attention, ImportEntry, ImportOutcome, ImportReport, NotMigratable, OutcomeKind, PairReport,
	SkipReason,
};
pub use settings::{SettingsImportError, import_project_settings, project_marker};
pub use step::{CredentialAccess, ImportError, ImportMode, ImportStep, Marker, StepContext, run};

/// The active v2 profile's pair: the same-named v1 profile (the v1 default
/// for the v2 default), which the lazy `models.toml` import reads.
///
/// # Errors
///
/// Returns [`LocateError`] without a home directory or with an invalid
/// `OMP_PROFILE`.
pub fn active_pair() -> Result<ImportPair, LocateError> {
	let inputs = V1Inputs::from_process().ok_or(LocateError::HomeUnset)?;
	let roots = V2Roots::from_process()?;
	let selection = ProfileSelection::Named(roots.active_profile.clone());
	let mut pairs = plan(&V1Source::new(inputs), &roots, &selection)?;
	Ok(pairs.swap_remove(0))
}

/// The automatic first-run import (owner decisions #1 and #8).
///
/// Runs every registered step for every v1 profile ([`ProfileSelection::All`]),
/// idempotently through the markers, then imports `project`'s own v1 settings
/// and v1-only `.omp/` files (once per project, [`import_project_settings`],
/// [`import_project_assets`]), and logs the report.
/// Credential steps use the live store for the profile that owns `data_dir`;
/// other profiles' credential steps wait, unmarked, for their own first run.
/// A `data_dir` other than the process default (a test's or an explicit state
/// directory) is isolated: nothing is imported. Never fails: locating errors
/// are logged and step failures are in the report.
pub fn first_run(
	data_dir: &Path,
	project: Option<&Path>,
	credentials: &omp_ai::auth::AuthControlHandle,
) -> Option<ImportReport> {
	if omp_core::dirs::data_dir(None).ok().as_deref() != Some(data_dir) {
		return None;
	}
	let located = V1Inputs::from_process()
		.ok_or(LocateError::HomeUnset)
		.and_then(|inputs| {
			let roots = V2Roots::from_process()?;
			let source = V1Source::new(inputs);
			let pairs = plan(&source, &roots, &ProfileSelection::All)?;
			Ok((source, roots, pairs))
		});
	let (source, roots, pairs) = match located {
		Ok(located) => located,
		Err(error) => {
			tracing::warn!(
				error = &error as &dyn std::error::Error,
				"could not locate the v1 install"
			);
			return None;
		},
	};
	let mut report =
		run(&pairs, ImportMode::Apply, CredentialAccess::Live { data_dir, control: credentials });
	if let Some(project) = project {
		report.project = import_project_settings(project, &roots, ImportMode::Apply);
		report
			.project
			.extend(import_project_assets(project, &source, &roots, ImportMode::Apply));
	}
	report.log();
	Some(report)
}
