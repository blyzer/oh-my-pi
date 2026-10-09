//! Access to a target profile's credential stores: opened offline for
//! `omp config import-v1`, or over the store the production composition
//! opened for the first-run import.

use std::sync::Arc;

use omp_ai::{
	account::{AccountPool, AccountStateStore},
	auth::{AuthControlHandle, CredentialStore, UnavailableKeySource},
};
use omp_catalog::{OverlaySource, OverlayStack, UnsafeTrustScope, snapshot};

use super::{ImportError, ModelsImportError, V2Target};
use crate::discovery::models::{load_models_config, lower_user_overlay};

/// The target profile's catalog: the bundled snapshot plus its own
/// `models.toml`, so accounts cover configured providers' routes too.
fn target_catalog(target: &V2Target) -> Result<Arc<snapshot::Catalog>, ImportError> {
	let bundled = snapshot::Catalog::try_embedded().map_err(ImportError::Catalog)?;
	let native = target.config_dir.join("models.toml");
	if !native.is_file() {
		return Ok(Arc::new(bundled.clone()));
	}
	let overlay = load_models_config(&native)
		.and_then(|config| lower_user_overlay(&config))
		.map_err(ModelsImportError::from)?;
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
	let store =
		crate::registry::open_credential_store_from_con(target.data_dir.join("credentials.db"), ctx)
			.map_err(ImportError::CredentialStore)?;
	control_over(target, store)
}

/// A control-only handle over the target profile's existing stores that
/// cannot decrypt: its store has no key source, so a dry run reads plaintext
/// metadata through it without creating a key or touching a keychain.
pub(super) fn locked_control(target: &V2Target) -> Result<AuthControlHandle, ImportError> {
	let store = crate::registry::open_credential_store_with_key_source(
		target.data_dir.join("credentials.db"),
		Arc::new(UnavailableKeySource),
	)
	.map_err(ImportError::CredentialStore)?;
	control_over(target, store)
}

/// A control-only handle over `store`, an already open credential store for
/// the target's data directory, and the account state beside it.
pub(super) fn control_over(
	target: &V2Target,
	store: Arc<CredentialStore>,
) -> Result<AuthControlHandle, ImportError> {
	let database = target.data_dir.join("credentials.db");
	let accounts = AccountPool::with_store(Arc::new(AccountStateStore::open(&database)?))?;
	Ok(AuthControlHandle::offline(target_catalog(target)?, store, accounts)?)
}
