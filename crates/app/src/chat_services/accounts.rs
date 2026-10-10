//! Stored provider accounts behind `/login`, `/logout`, `/setup`, and `/pin`:
//! the live [`AuthManager`] drives every interactive login on the runtime and
//! streams what the dialog must show over the [`LoginFlow`] channels, exactly
//! like the `omp auth login` loop in [`crate::auth_cli`] but without a TTY.

use std::path::Path;

use flume::{Receiver, Sender};
use omp_ai::{
	account::{AI_ACCOUNT_PINS, AccountName, AccountPin, with_pin, without_pin},
	answer::{
		AccountSummary, AuthAnswer, AuthEvent, AuthPrompt, AuthPromptKind, AuthResponse, AuthSession,
	},
	auth::{AuthControlHandle, AuthManager},
	call::{AuthInput, AuthRequest, LoginRequest},
	id::AccountId,
};
use omp_catalog::{ProviderId, provider::AuthSpecKind};
use omp_chat::overlays::services::{
	AccountRow, LoginEvent, LoginFlow, Pending, ProviderRow, ServiceError, ServiceResult,
};
use omp_core::{ExposeSecret as _, SecretString, Str, sf};

use super::{ServiceState, StackHandles};

const GATEWAY: &str = "provider accounts (remote gateway)";
const LOGIN_CANCELLED: &str = "Login cancelled";

fn stack(state: &ServiceState) -> ServiceResult<&StackHandles> {
	state
		.stack
		.as_ref()
		.ok_or(ServiceError::Unavailable(GATEWAY))
}

fn provider_name(state: &ServiceState, provider: &ProviderId<str>) -> Str {
	state
		.catalog
		.as_ref()
		.and_then(|catalog| {
			catalog
				.load()
				.provider(provider)
				.map(|def| def.name.clone())
		})
		.unwrap_or_else(|| Str::new(provider.as_str()))
}

/// Every stored account, in the pool's stable account-id order.
pub fn rows(state: &ServiceState) -> ServiceResult<Vec<AccountRow>> {
	let handles = stack(state)?;
	let control = &handles.auth_control;
	let pins = handles.auth.session_pins(&AI_ACCOUNT_PINS.get(&state.con));
	Ok(control
		.accounts(None)
		.into_iter()
		.map(|record| {
			let (kind, source) = match control.metadata(&record.account) {
				Ok(Some(metadata)) => (metadata.kind.clone(), sf!("stored {}", metadata.kind)),
				Ok(None) => (sf!("external"), sf!("environment or external authority")),
				Err(_) => (sf!("unknown"), sf!("credential source unavailable")),
			};
			let name = control.account_name(&record.account);
			let named = name.is_some();
			AccountRow {
				pinned: pins.for_provider(&record.provider)
					== Some(&AccountPin::Account(record.account.clone())),
				name: name.map(AccountName::into_inner),
				id: record.account.as_inner().clone(),
				provider: record.provider.as_inner().clone(),
				provider_name: provider_name(state, &record.provider),
				label: record.principal.as_inner().clone(),
				detail: match (record.enabled, named) {
					(true, false) => source,
					(true, true) => sf!("{} · {source}", record.principal.as_str()),
					(false, false) => sf!("{source} · disabled"),
					(false, true) => sf!("{} · {source} · disabled", record.principal.as_str()),
				},
				kind,
				active: record.enabled,
			}
		})
		.collect())
}

/// Catalog providers with an interactive login method, flagged with whether
/// an account is already stored.
pub fn providers(state: &ServiceState) -> ServiceResult<Vec<ProviderRow>> {
	let control = &stack(state)?.auth_control;
	let catalog = state
		.catalog
		.as_ref()
		.ok_or(ServiceError::Unavailable("provider catalog (remote gateway)"))?
		.load();
	let accounts = control.accounts(None);
	Ok(catalog
		.providers()
		.iter()
		.filter_map(|provider| {
			let mut login = false;
			let mut oauth = false;
			for id in &provider.auth {
				let Some(spec) = catalog.auth_spec(id) else {
					continue;
				};
				match spec.kind {
					AuthSpecKind::None | AuthSpecKind::Basic => {},
					AuthSpecKind::Oauth => {
						login = true;
						oauth = true;
					},
					_ => login = true,
				}
			}
			login.then(|| ProviderRow {
				id: provider.id.as_inner().clone(),
				name: provider.name.clone(),
				oauth,
				logged_in: accounts.iter().any(|record| record.provider == provider.id),
			})
		})
		.collect())
}

/// Starts an interactive login and drives it on the runtime.
pub fn login(state: &ServiceState, provider: &str) -> ServiceResult<LoginFlow> {
	let auth = stack(state)?.auth.clone();
	let provider_id = ProviderId::new(provider);
	if state
		.catalog
		.as_ref()
		.is_some_and(|catalog| catalog.load().provider(&provider_id).is_none())
	{
		return Err(ServiceError::Failed(sf!("Unknown OAuth provider: {provider}")));
	}
	let name = provider_name(state, &provider_id);
	let (events_tx, events) = flume::unbounded();
	let (input, input_rx) = flume::unbounded();
	let (done_tx, done) = flume::bounded(1);
	let (cancel, cancel_rx) = flume::bounded(1);
	let database = state.data_dir.join("credentials.db");
	let request = LoginRequest { provider: provider_id.clone(), method: None };
	let title = name.clone();
	let discovery = state.discovery.clone();
	let refreshed = provider_id.clone();
	state.runtime.spawn(async move {
		let driver = Driver { auth, events: events_tx, input: input_rx, cancel: cancel_rx };
		let outcome = driver.run(request, &title, &database).await;
		// New credentials can list models the old ones could not: re-probe
		// this provider now instead of at the next launch.
		if outcome.is_ok()
			&& let Some(discovery) = &discovery
		{
			discovery.request(omp_driver::registry::DiscoveryRefresh::LoggedIn(refreshed));
		}
		let _ = done_tx.send(outcome);
	});
	Ok(LoginFlow {
		provider: provider_id.into_inner(),
		provider_name: name,
		events,
		input,
		done,
		cancel,
	})
}

/// Deletes one stored account; settles once the encrypted store commits.
pub fn logout(state: &ServiceState, account: &AccountRow) -> ServiceResult<Pending<()>> {
	let control: AuthControlHandle = stack(state)?.auth_control.clone();
	let id = AccountId::new(account.id.clone());
	let (tx, rx) = flume::bounded(1);
	state.runtime.spawn(async move {
		let _ = tx.send(control.delete(id).await.map_err(ServiceError::failed));
	});
	Ok(rx)
}

/// `/pin <provider> [account]`: pins the session to one account identity, or
/// lifts the pin.
///
/// The pin is the session convar [`AI_ACCOUNT_PINS`]: the opaque
/// credential-affinity digest of the account, journaled with the session and
/// resolved by the live route on every request. Requests for the provider then
/// use only that account and fail with a typed notice instead of falling back
/// when it is unavailable.
pub fn pin(state: &ServiceState, account: &AccountRow, pinned: bool) -> ServiceResult<Str> {
	let handles = stack(state)?;
	let provider = ProviderId::new(account.provider.clone());
	let recorded = AI_ACCOUNT_PINS.get(&state.con);
	if !pinned {
		AI_ACCOUNT_PINS
			.set(&state.con, without_pin(&recorded, &provider))
			.map_err(ServiceError::failed)?;
		return Ok(sf!(
			"Unpinned {}; this session may use any {} account.",
			account.display_name(),
			account.provider_name
		));
	}
	let record = handles
		.auth_control
		.accounts(Some(&provider))
		.into_iter()
		.find(|record| record.account.as_str() == account.id.as_str())
		.ok_or_else(|| {
			ServiceError::Failed(sf!("{} is no longer stored.", account.display_name()))
		})?;
	let digest = handles
		.auth
		.affinity_digest(&record)
		.ok_or(ServiceError::Unavailable("credential affinity key"))?;
	AI_ACCOUNT_PINS
		.set(&state.con, with_pin(&recorded, &provider, &digest))
		.map_err(ServiceError::failed)?;
	Ok(sf!(
		"Pinned {} for {}; requests in this session use only this account.",
		account.display_name(),
		account.provider_name
	))
}

/// Journal stem of the live session (`/pin` without an argument).
pub fn live_session_id(state: &ServiceState) -> ServiceResult<Str> {
	state
		.journal
		.file_stem()
		.and_then(|stem| stem.to_str())
		.filter(|stem| !stem.is_empty() && state.journal.is_file())
		.map(Str::new)
		.ok_or_else(|| ServiceError::Failed(sf!("No active session to pin.")))
}

/// One login's channel ends, owned by the runtime task.
struct Driver {
	auth:   AuthManager,
	events: Sender<LoginEvent>,
	input:  Receiver<Str>,
	cancel: Receiver<()>,
}
enum PromptOutcome {
	Input(AuthInput),
	Complete(AccountSummary),
}

impl Driver {
	async fn run(&self, request: LoginRequest, name: &str, database: &Path) -> ServiceResult<Str> {
		let started = tokio::select! {
			answer = self.auth.execute(AuthRequest::Login(request)) => answer.map_err(ServiceError::failed)?,
			_ = self.cancel.recv_async() => return Err(ServiceError::Failed(sf!(LOGIN_CANCELLED))),
		};
		let session = match started {
			AuthAnswer::Session(session) => session,
			// Extension-hosted logins complete without a session.
			AuthAnswer::Refreshed(summary) => return Ok(success(name, &summary, database)),
			AuthAnswer::Accounts(_) | AuthAnswer::LoggedOut(_) | AuthAnswer::Submitted(_) => {
				return Err(ServiceError::Failed(sf!("Login to {name} returned no session")));
			},
		};
		loop {
			let event = tokio::select! {
				event = session.events.recv_async() => event,
				_ = self.cancel.recv_async() => {
					session.cancel();
					return Err(ServiceError::Failed(sf!(LOGIN_CANCELLED)));
				},
			};
			let Ok(event) = event else {
				return Err(ServiceError::Failed(sf!("Login to {name} ended without a result")));
			};
			match event.map_err(ServiceError::failed)? {
				AuthEvent::OpenUrl { url, launch } => {
					omp_core::open::open_path(launch.as_deref().unwrap_or(url.as_str()));
					self.show(LoginEvent::OpenUrl { url, launched: true });
				},
				AuthEvent::ShowDeviceCode { code, verification_url } => {
					self.show(LoginEvent::DeviceCode {
						code: Str::new(code.expose_secret()),
						verification_url,
					});
				},
				AuthEvent::Prompt(prompt) => match self.answer(&prompt, &session).await? {
					PromptOutcome::Complete(summary) => return Ok(success(name, &summary, database)),
					PromptOutcome::Input(input) => {
						if session
							.responses
							.send_async(AuthResponse { session: session.id.clone(), input })
							.await
							.is_err()
						{
							return Err(ServiceError::Failed(sf!(
								"Login to {name} ended without a result"
							)));
						}
					},
				},
				AuthEvent::Waiting => {
					self.show(LoginEvent::Info(sf!("Waiting for {name} authorization…")))
				},
				AuthEvent::Complete(summary) => return Ok(success(name, &summary, database)),
			}
		}
	}

	fn show(&self, event: LoginEvent) {
		let _ = self.events.send(event);
	}

	/// Shows the prompt and waits for the dialog's answer, re-prompting on
	/// input the method rejects (an empty code) until one is accepted.
	async fn answer(
		&self,
		prompt: &AuthPrompt,
		session: &AuthSession,
	) -> ServiceResult<PromptOutcome> {
		let mut current = prompt.clone();
		loop {
			self.show(LoginEvent::Prompt { label: current.message.clone() });
			tokio::select! {
				value = self.input.recv_async() => {
					let Ok(value) = value else {
						session.cancel();
						return Err(ServiceError::Failed(sf!(LOGIN_CANCELLED)));
					};
					match auth_input(&current, value.as_str().trim()) {
						Ok(input) => return Ok(PromptOutcome::Input(input)),
						Err(message) => self.show(LoginEvent::Info(Str::new_static(message))),
					}
				},
				event = session.events.recv_async() => {
					let Ok(event) = event else {
						return Err(ServiceError::Failed(sf!(
							"Login session ended while waiting for callback"
						)));
					};
					match event.map_err(ServiceError::failed)? {
						AuthEvent::OpenUrl { url, launch } => {
							omp_core::open::open_path(launch.as_deref().unwrap_or(url.as_str()));
							self.show(LoginEvent::OpenUrl { url, launched: true });
						},
						AuthEvent::ShowDeviceCode { code, verification_url } => {
							self.show(LoginEvent::DeviceCode {
								code: Str::new(code.expose_secret()),
								verification_url,
							});
						},
						AuthEvent::Waiting => {
							self.show(LoginEvent::Info(sf!("Waiting for authorization…")));
						},
						AuthEvent::Prompt(next) => current = next,
						AuthEvent::Complete(summary) => return Ok(PromptOutcome::Complete(summary)),
					}
				},
				_ = self.cancel.recv_async() => {
					session.cancel();
					return Err(ServiceError::Failed(sf!(LOGIN_CANCELLED)));
				},
			}
		}
	}
}

fn success(name: &str, summary: &AccountSummary, database: &Path) -> Str {
	let who = summary
		.principal
		.as_ref()
		.map(|principal| principal.as_str())
		.or(summary.label.as_deref())
		.filter(|who| !who.is_empty())
		.map_or_else(Str::default, |who| sf!(" as {who}"));
	sf!("Successfully logged in to {name}{who} · credentials saved to {}", database.display())
}

/// Typed answer for one prompt (the `omp auth login` mapping): a pasted
/// `scheme://` value answers an authorization-code prompt as the callback
/// URL, an empty required secret is rejected, and a confirmation accepts
/// `y`/`yes`/empty.
fn auth_input(prompt: &AuthPrompt, value: &str) -> Result<AuthInput, &'static str> {
	if value.is_empty()
		&& matches!(
			prompt.input,
			AuthPromptKind::AuthorizationCode | AuthPromptKind::ApiKey | AuthPromptKind::SessionToken
		) {
		return Err("authentication input must not be empty");
	}
	Ok(match prompt.input {
		AuthPromptKind::AuthorizationCode => {
			if value.contains("://") {
				AuthInput::CallbackUrl(SecretString::from(value))
			} else {
				AuthInput::AuthorizationCode(SecretString::from(value))
			}
		},
		AuthPromptKind::ApiKey => AuthInput::ApiKey(SecretString::from(value)),
		AuthPromptKind::SessionToken => AuthInput::SessionToken(SecretString::from(value)),
		AuthPromptKind::PlainText => AuthInput::PlainText(Str::new(value)),
		AuthPromptKind::OptionalSecret => AuthInput::OptionalSecret(SecretString::from(value)),
		AuthPromptKind::Confirmation => {
			if matches!(value.to_ascii_lowercase().as_str(), "" | "y" | "yes") {
				AuthInput::DeviceConfirmed
			} else {
				AuthInput::Cancel
			}
		},
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn prompt(input: AuthPromptKind) -> AuthPrompt {
		AuthPrompt { id: sf!("p"), message: sf!("Paste"), input }
	}

	#[test]
	fn pasted_urls_answer_code_prompts_as_callbacks_and_empty_secrets_are_rejected() {
		assert!(matches!(
			auth_input(&prompt(AuthPromptKind::AuthorizationCode), "https://x/cb?code=1"),
			Ok(AuthInput::CallbackUrl(_))
		));
		assert!(matches!(
			auth_input(&prompt(AuthPromptKind::AuthorizationCode), "abc"),
			Ok(AuthInput::AuthorizationCode(_))
		));
		assert!(auth_input(&prompt(AuthPromptKind::ApiKey), "").is_err());
		assert!(matches!(
			auth_input(&prompt(AuthPromptKind::Confirmation), "YES"),
			Ok(AuthInput::DeviceConfirmed)
		));
		assert!(matches!(
			auth_input(&prompt(AuthPromptKind::Confirmation), "n"),
			Ok(AuthInput::Cancel)
		));
		assert!(matches!(
			auth_input(&prompt(AuthPromptKind::OptionalSecret), ""),
			Ok(AuthInput::OptionalSecret(_))
		));
	}
}
