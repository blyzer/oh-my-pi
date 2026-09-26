//! Offline access to a target profile's credential stores, for imports run
//! outside a live inference stack (`omp config import-v1`).

use std::sync::Arc;

use omp_ai::{
	account::{AccountPool, AccountStateStore},
	auth::AuthControlHandle,
};
use omp_catalog::{OverlaySource, OverlayStack, UnsafeTrustScope, snapshot};

use super::{ImportError, V2Target};
use crate::discovery::models::{load_models_config, lower_user_overlay};

/// The target profile's catalog: the bundled snapshot plus its own
/// `models.toml`, so accounts cover configured providers' routes too.
fn target_catalog(target: &V2Target) -> Result<Arc<snapshot::Catalog>, ImportError> {
	let bundled = snapshot::Catalog::try_embedded().map_err(ImportError::Catalog)?;
	let native = target.config_dir.join("models.toml");
	if !native.is_file() {
		return Ok(Arc::new(bundled.clone()));
	}
	let overlay = lower_user_overlay(&load_models_config(&native)?)?;
	let catalog = bundled
		.with_overlay_stack(
			&OverlayStack::from_layers([(OverlaySource::UserConfig, overlay)]),
			UnsafeTrustScope::ALL,
		)
		.map_err(ImportError::ConfiguredCatalog)?;
	Ok(Arc::new(catalog))
}

/// Opens the target profile's credential and account stores behind a
/// control-only handle. Accounts written through it appear in every later
/// production stack over the same data directory.
pub(super) fn offline_control(
	target: &V2Target,
	ctx: &omp_con::Ctx,
) -> Result<AuthControlHandle, ImportError> {
	std::fs::create_dir_all(&target.data_dir).map_err(|source| {
		ImportError::CredentialStore(crate::registry::RegistryError::PrepareState(source))
	})?;
	let database = target.data_dir.join("credentials.db");
	let store = crate::registry::open_credential_store_from_con(&database, ctx)
		.map_err(ImportError::CredentialStore)?;
	let accounts = AccountPool::with_store(Arc::new(AccountStateStore::open(&database)?))?;
	Ok(AuthControlHandle::offline(target_catalog(target)?, store, accounts)?)
}
