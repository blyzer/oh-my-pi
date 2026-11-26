//! Stored-credential source and composition for extension HTTP placement
//! (ADR 0075 S4).
//!
//! The Environment host places a provider's key on an extension's granted
//! request; this module tells it where the key is stored. The key is leased
//! from the encrypted credential store and never leaves the host.

use std::{path::Path, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use omp_ai::{
	AccountId,
	auth::{AuthControlHandle, CredentialError, CredentialLease, CredentialNeed},
};
use omp_catalog::{AuthSpecId, ProviderId};
use omp_envd::{
	forge_url::CredentialAuthority,
	placement::{JsonlPlacementAudit, PlacementAuthority, PlacementBinding, PlacementCredentials},
};

/// The audit file under the host data directory. Rows carry extension,
/// provider, host, port, method and request id, never a key.
const PLACEMENT_AUDIT_FILE: &str = "placement-audit.jsonl";

/// The stored accounts of one provider, with whether each may be used.
pub trait PlacementAccounts: Send + Sync + 'static {
	/// Every stored account of `provider` and its enabled flag.
	fn accounts(&self, provider: &ProviderId) -> Vec<(AccountId, bool)>;
}

impl PlacementAccounts for AuthControlHandle {
	fn accounts(&self, provider: &ProviderId) -> Vec<(AccountId, bool)> {
		Self::accounts(self, Some(provider))
			.into_iter()
			.map(|record| (record.account, record.enabled))
			.collect()
	}
}

/// Leases a provider's stored key for placement.
///
/// A provider with exactly one enabled stored account places that account's
/// key. With none, or with several, nothing is chosen on the extension's behalf
/// and the request is refused as having no stored credential.
pub struct StoredPlacementCredentials {
	accounts:    Arc<dyn PlacementAccounts>,
	credentials: Arc<dyn CredentialAuthority>,
}

impl StoredPlacementCredentials {
	/// Reads account rows from `accounts` and leases through `credentials`.
	#[must_use]
	pub fn new(
		accounts: Arc<dyn PlacementAccounts>,
		credentials: Arc<dyn CredentialAuthority>,
	) -> Self {
		Self { accounts, credentials }
	}
}

#[async_trait]
impl PlacementCredentials for StoredPlacementCredentials {
	async fn lease(
		&self,
		provider: &ProviderId,
		spec: &AuthSpecId,
	) -> Result<CredentialLease, CredentialError> {
		let mut enabled = self
			.accounts
			.accounts(provider)
			.into_iter()
			.filter_map(|(account, enabled)| enabled.then_some(account));
		let (Some(account), None) = (enabled.next(), enabled.next()) else {
			return Err(CredentialError::Unavailable);
		};
		self
			.credentials
			.provider_lease(CredentialNeed {
				spec:        spec.clone(),
				account:     Some(account),
				principal:   None,
				valid_after: SystemTime::now(),
			})
			.await
	}
}

/// Binds credential placement for extension HTTP requests over the production
/// credential stack. A second bind is ignored.
pub fn bind_placement(
	slot: &PlacementAuthority,
	data_dir: &Path,
	accounts: Arc<dyn PlacementAccounts>,
	credentials: Arc<dyn CredentialAuthority>,
) {
	let binding = PlacementBinding {
		credentials: Arc::new(StoredPlacementCredentials::new(accounts, credentials)),
		audit:       Arc::new(JsonlPlacementAudit::new(data_dir.join(PLACEMENT_AUDIT_FILE))),
		client:      None,
	};
	if slot.bind(binding).is_err() {
		tracing::debug!("extension credential placement was already bound");
	}
}

#[cfg(test)]
mod tests {
	use omp_ai::{
		auth::{CredentialFuture, LeaseMeta},
		id::PrincipalId,
	};
	use omp_core::SecretString;
	use parking_lot::Mutex;

	use super::*;

	struct FakeAccounts(Vec<(&'static str, bool)>);

	impl PlacementAccounts for FakeAccounts {
		fn accounts(&self, _provider: &ProviderId) -> Vec<(AccountId, bool)> {
			self
				.0
				.iter()
				.map(|(account, enabled)| (AccountId::from(*account), *enabled))
				.collect()
		}
	}

	#[derive(Default)]
	struct FakeAuthority {
		needs: Mutex<Vec<CredentialNeed>>,
	}

	impl CredentialAuthority for FakeAuthority {
		fn provider_lease(
			&self,
			need: CredentialNeed,
		) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>> {
			let account = need.account.clone().expect("an account is always chosen");
			self.needs.lock().push(need);
			futures::future::Either::Left(futures::future::ready(Ok(CredentialLease::api_key(
				LeaseMeta {
					account,
					principal: PrincipalId::from("test"),
					generation: 1,
					expires_at: None,
				},
				SecretString::from("stored-key".to_owned()),
			))))
		}
	}

	async fn lease_with(
		accounts: Vec<(&'static str, bool)>,
	) -> (Result<CredentialLease, CredentialError>, Arc<FakeAuthority>) {
		let authority = Arc::new(FakeAuthority::default());
		let source =
			StoredPlacementCredentials::new(Arc::new(FakeAccounts(accounts)), authority.clone());
		let lease = source
			.lease(&ProviderId::from("buildbuddy"), &AuthSpecId::from("buildbuddy"))
			.await;
		(lease, authority)
	}

	#[tokio::test]
	async fn one_enabled_account_is_leased() {
		let (lease, authority) =
			lease_with(vec![("buildbuddy:other", false), ("buildbuddy:owner", true)]).await;

		assert!(lease.is_ok());
		let needs = authority.needs.lock();
		assert_eq!(needs.len(), 1);
		assert_eq!(needs[0].account, Some(AccountId::from("buildbuddy:owner")));
		assert_eq!(needs[0].spec, AuthSpecId::from("buildbuddy"));
	}

	#[tokio::test]
	async fn no_enabled_account_means_no_stored_credential() {
		for accounts in [vec![], vec![("buildbuddy:off", false)]] {
			let (lease, authority) = lease_with(accounts).await;

			assert!(matches!(lease, Err(CredentialError::Unavailable)));
			assert!(authority.needs.lock().is_empty(), "nothing was leased");
		}
	}

	#[tokio::test]
	async fn several_enabled_accounts_are_not_guessed() {
		let (lease, authority) =
			lease_with(vec![("buildbuddy:a", true), ("buildbuddy:b", true)]).await;

		assert!(matches!(lease, Err(CredentialError::Unavailable)));
		assert!(authority.needs.lock().is_empty(), "no account was chosen");
	}

	#[tokio::test]
	async fn bind_placement_binds_once() {
		let slot = PlacementAuthority::default();
		let directory = tempfile::tempdir().expect("tempdir");
		let accounts: Arc<dyn PlacementAccounts> = Arc::new(FakeAccounts(vec![]));
		let authority: Arc<dyn CredentialAuthority> = Arc::new(FakeAuthority::default());

		bind_placement(&slot, directory.path(), Arc::clone(&accounts), Arc::clone(&authority));
		assert!(format!("{slot:?}").contains("bound: true"));
		// A second bind leaves the first in place.
		bind_placement(&slot, directory.path(), accounts, authority);
		assert!(format!("{slot:?}").contains("bound: true"));
	}
}
