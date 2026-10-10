//! Audited provider credential projection for `omp token`.

use std::{
	process,
	time::{SystemTime, UNIX_EPOCH},
};

use miette::{Context as _, IntoDiagnostic as _, miette};
use omp_ai::{
	answer::{AccountState, AccountSummary, AuthAnswer},
	auth::{AuditedCredentialReveal, CredentialStore, StoreError},
	call::AuthRequest,
};
use omp_catalog::ProviderId;
use omp_core::Str;
use serde_json::Value;

use crate::cli::TokenArgs;

/// Lists or prints one provider credential after durable reveal auditing.
pub(crate) async fn run(args: TokenArgs) -> miette::Result<()> {
	let data = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let store = omp_driver::registry::open_credential_store(data.join("credentials.db"))
		.into_diagnostic()
		.wrap_err(
			"credential storage could not be unlocked; unattended callers may set \
			 OMP_LLM_KEY_SOURCE=local-file for the owner-only local encrypted store",
		)?;
	let (_registry, auth) = omp_driver::registry::production_rpc_registry(&data, store.clone())
		.await
		.into_diagnostic()
		.wrap_err(
			"credential storage could not be unlocked; unattended callers may set \
			 OMP_LLM_KEY_SOURCE=local-file for the owner-only local encrypted store",
		)?;
	let provider = ProviderId::from(args.provider.clone());
	let accounts = match auth
		.execute(AuthRequest::ListAccounts { provider: Some(provider.clone()) })
		.await
		.into_diagnostic()?
	{
		AuthAnswer::Accounts(accounts) => stored_active_accounts(&store, accounts).into_diagnostic()?,
		_ => return Err(miette!("provider account listing returned an unexpected response")),
	};
	if args.list {
		for (index, account) in accounts.iter().enumerate() {
			println!("{}. {}", index + 1, account.account);
		}
		return Ok(());
	}
	if accounts.is_empty() {
		return Err(miette!("no active credential found for provider `{}`", args.provider));
	}
	let account = &accounts[select_account_index(accounts.len(), args.account)?];
	if args.force_refresh {
		let _ = auth
			.execute(AuthRequest::Refresh { account: account.account.clone() })
			.await
			.into_diagnostic()?;
	}
	let request_id = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos() as u64;
	let audit = AuditedCredentialReveal {
		extension: Str::new_static("omp.cli.token"),
		caller_principal: Str::from(format!("pid:{}", process::id())),
		provider: Str::new(provider.as_str()),
		host_generation: 1,
		session_generation: 1,
		request_id,
		reason: Str::new_static("explicit operator token command"),
	};
	let rendered = store
		.with_audited_secret(&account.account, &audit, |secret| {
			secret.expose(|bytes| {
				let raw = std::str::from_utf8(bytes)
					.map_err(|_| miette!("stored credential is not valid UTF-8"))?;
				Ok::<_, miette::Report>(render_token(raw, args.raw))
			})
		})
		.into_diagnostic()??;
	println!("{rendered}");
	Ok(())
}

fn stored_active_accounts(
	store: &CredentialStore,
	accounts: Vec<AccountSummary>,
) -> Result<Vec<AccountSummary>, StoreError> {
	let mut stored = Vec::new();
	for account in accounts {
		if account.state == AccountState::Active && store.metadata(&account.account)?.is_some() {
			stored.push(account);
		}
	}
	stored.sort_by(|left, right| left.account.cmp(&right.account));
	Ok(stored)
}

fn select_account_index(count: usize, selected: Option<usize>) -> miette::Result<usize> {
	let selected = match selected {
		Some(selected) => selected,
		None if count == 1 => 1,
		None => return Err(miette!("provider has {count} active accounts; use --list and --account")),
	};
	if selected == 0 || selected > count {
		return Err(miette!("invalid --account {selected}; provider has {count} active account(s)"));
	}
	Ok(selected - 1)
}
fn render_token(raw: &str, unparsed: bool) -> String {
	if unparsed {
		return raw.to_owned();
	}
	serde_json::from_str::<Value>(raw)
		.ok()
		.and_then(|value| {
			["token", "access_token", "accessToken", "api_key", "apiKey"]
				.into_iter()
				.find_map(|key| value.get(key).and_then(Value::as_str).map(str::to_owned))
		})
		.unwrap_or_else(|| raw.to_owned())
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn token_listing_ignores_active_accounts_without_stored_credentials() {
		let directory = tempfile::tempdir().unwrap();
		let store = CredentialStore::open(
			directory.path().join("credentials.db"),
			std::sync::Arc::new(omp_ai::auth::HeadlessKeySource::new(
				omp_ai::auth::KeyId::new("token-test"), [8; 32],
			)),
		).unwrap();
		let account = omp_ai::AccountId::from("provider:z-stored");
		let principal = omp_ai::PrincipalId::from("stored");
		store.put(omp_ai::auth::CredentialWrite {
			account_id: &account, principal_id: &principal, kind: "api-key",
			secret: &omp_core::SecretBox::new(Box::new(b"test-secret".to_vec())),
			expires_at_ms: None, origin: omp_ai::auth::CredentialOrigin::Persistent,
			now_ms: 1, expected_generation: None,
		}).unwrap();
		let summary = |account: &str, state| AccountSummary {
			account: omp_ai::AccountId::from(account), provider: ProviderId::from("provider"),
			principal: None, label: None, state,
		};
		let stored = stored_active_accounts(&store, vec![
			summary("provider:a-environment", AccountState::Active),
			summary(account.as_str(), AccountState::Active),
			summary(account.as_str(), AccountState::Disabled),
		]).unwrap();
		assert_eq!(stored.len(), 1);
		assert_eq!(stored[0].account, account);
		assert_eq!(select_account_index(stored.len(), None).unwrap(), 0);
	}

	#[test]
	fn account_selection_requires_an_explicit_choice_when_ambiguous() {
		assert_eq!(select_account_index(1, None).unwrap(), 0);
		assert!(select_account_index(2, None).is_err());
		assert_eq!(select_account_index(2, Some(2)).unwrap(), 1);
		assert!(select_account_index(2, Some(0)).is_err());
		assert!(select_account_index(2, Some(3)).is_err());
	}
	#[test]
	fn nested_token_projection_and_raw_mode_are_distinct() {
		let raw = r#"{"access_token":"secret","refresh_token":"hidden"}"#;
		assert_eq!(render_token(raw, false), "secret");
		assert_eq!(render_token(raw, true), raw);
	}
}
