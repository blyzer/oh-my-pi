//! Interactive provider-authentication command presentation.

use std::{
	fs,
	io::{self, IsTerminal as _, Write as _},
	path::{Path, PathBuf},
	sync::Arc,
	time,
};

use miette::{IntoDiagnostic as _, miette};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use omp_ai::{
	Client,
	account::{AccountName, AccountPool, AccountSelectError, AccountStateStore},
	answer::{
		AuthAnswer, AuthEvent, AuthPrompt, AuthPromptKind as InferenceAuthPromptKind, AuthResponse,
	},
	auth::CredentialAuditEntry,
	call::{AuthInput, AuthRequest, CallMeta, LoginRequest, Target},
	id::{AccountId, RequestId},
	receipt::ExecutionBudget,
	router,
};
use omp_catalog::ProviderId;
use omp_core::{ExposeSecret as _, SecretString, Str};
use tokio::task;
use zeroize::Zeroizing;

use crate::cli::AuthCommand;

/// Opens encrypted credential state and executes one typed authentication
/// operation.
pub async fn run(database: PathBuf, command: AuthCommand) -> miette::Result<()> {
	let data_dir = database
		.parent()
		.filter(|path| !path.as_os_str().is_empty())
		.ok_or_else(|| miette!("HOME or OMP_DATA_DIR must be set"))?;
	fs::create_dir_all(data_dir).into_diagnostic()?;
	if let AuthCommand::Rename { account, name, .. } = &command {
		return rename(&database, account, name.as_deref());
	}
	let store = omp_driver::registry::open_credential_store(&database).into_diagnostic()?;
	if let AuthCommand::Audit { provider, extension, limit, json } = &command {
		let entries = store
			.credential_audit(provider.as_deref(), extension.as_deref(), *limit)
			.into_diagnostic()?;
		return print_audit(&entries, *json);
	}
	let registry = omp_driver::registry::production_registry(data_dir, store)
		.await
		.into_diagnostic()?;
	let default_provider = registry
		.catalog()
		.providers()
		.first()
		.map(|provider| provider.id.clone())
		.ok_or_else(|| miette!("embedded catalog is unavailable"))?;
	let (provider, operation) = match command {
		AuthCommand::Audit { .. } | AuthCommand::Rename { .. } => {
			unreachable!("audit and rename commands returned before registry composition")
		},
		AuthCommand::Login { provider } => {
			let provider = ProviderId::from(provider);
			(provider.clone(), AuthRequest::Login(LoginRequest { provider, method: None }))
		},
		AuthCommand::List { provider } | AuthCommand::Status { provider } => {
			let requested = provider.map(ProviderId::from);
			let target = requested.clone().unwrap_or(default_provider);
			(target, AuthRequest::ListAccounts { provider: requested })
		},
		AuthCommand::Refresh { account } => (default_provider.clone(), AuthRequest::Refresh {
			account: select(&open_accounts(&database)?, &account)?,
		}),
		AuthCommand::Logout { account } => (default_provider.clone(), AuthRequest::Logout {
			account: select(&open_accounts(&database)?, &account)?,
		}),
	};
	let meta = CallMeta {
		id:             RequestId::from("omp-auth-cli"),
		target:         Target::ProviderService(provider),
		deadline:       None,
		budget:         ExecutionBudget::default(),
		session:        None,
		debug_session:  None,
		response_hooks: Default::default(),
	};
	let planner = router::Router::new(registry.clone(), time::Duration::from_secs(30));
	let mut client = Client::new(registry.service(), planner, meta);
	print_auth(client.execute(operation).await.into_diagnostic()?, &database).await
}

fn open_accounts(database: &Path) -> miette::Result<AccountPool> {
	AccountPool::with_store(Arc::new(AccountStateStore::open(database).into_diagnostic()?))
		.into_diagnostic()
}

/// Resolves a typed account selector: an id, `provider/name`, or a unique
/// name. A bare token that names no pooled account is still tried as an id,
/// because credentials can outlive their pool record.
fn select(accounts: &AccountPool, selector: &str) -> miette::Result<AccountId> {
	match accounts.resolve(selector) {
		Ok(account) => Ok(account),
		Err(AccountSelectError::Unknown) if !selector.contains('/') => Ok(AccountId::from(selector)),
		Err(error) => Err(error).into_diagnostic(),
	}
}

/// Names, or with no `name` un-names, one account in the account state store.
fn rename(database: &Path, selector: &str, name: Option<&str>) -> miette::Result<()> {
	let accounts = open_accounts(database)?;
	let account = accounts.resolve(selector).into_diagnostic()?;
	match name {
		Some(name) => {
			let name = AccountName::parse(name).into_diagnostic()?;
			accounts
				.set_name(&account, name.clone())
				.into_diagnostic()?;
			println!("{account} is now named {}", name.as_str());
		},
		None => {
			let cleared = accounts.clear_name(&account).into_diagnostic()?;
			println!(
				"{account} {}",
				if cleared {
					"name cleared"
				} else {
					"had no name"
				}
			);
		},
	}
	Ok(())
}

fn print_audit(entries: &[CredentialAuditEntry], json: bool) -> miette::Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(entries).into_diagnostic()?);
		return Ok(());
	}
	if entries.is_empty() {
		println!("no credential audit records");
		return Ok(());
	}
	for entry in entries {
		println!(
			"{} action={} extension={} provider={} account={} principal={} generation={} host={} \
			 session={} request={} reason={}",
			entry.observed_at_ms,
			entry.action,
			entry.extension,
			entry.provider,
			entry.account_id,
			entry.credential_principal,
			entry.credential_generation,
			entry.host_generation,
			entry.session_generation,
			entry.request_id,
			entry.reason,
		);
	}
	Ok(())
}

async fn print_auth(answer: AuthAnswer, database: &Path) -> miette::Result<()> {
	match answer {
		AuthAnswer::Session(session) => {
			let session_id = session.id.clone();
			while let Ok(event) = session.events.recv_async().await {
				match event.into_diagnostic()? {
					AuthEvent::OpenUrl { url, launch } => {
						println!("\nOpen this URL in your browser:\n{url}");
						if let Some(launch) = launch {
							println!("or open {launch}\n");
						} else {
							println!();
						}
					},
					AuthEvent::ShowDeviceCode { code, verification_url } => println!(
						"complete device authorization at {verification_url} using code {}",
						code.expose_secret()
					),
					AuthEvent::Prompt(prompt) => {
						let input = task::spawn_blocking(move || read_prompt(&prompt))
							.await
							.into_diagnostic()??;
						session
							.responses
							.send_async(AuthResponse { session: session_id.clone(), input })
							.await
							.into_diagnostic()?;
					},
					AuthEvent::Waiting => println!("waiting for provider authorization"),
					AuthEvent::Complete(account) => {
						println!("{} {}", account.account, account.provider);
						println!("\nCredentials saved to {}", database.display());
						break;
					},
				}
			}
		},
		AuthAnswer::Accounts(accounts) => {
			for account in accounts {
				print!(
					"selector={} provider={} state={:?}",
					account.account, account.provider, account.state
				);
				if let Some(principal) = account.principal {
					print!(" principal={principal}");
				}
				if let Some(name) = account.label {
					print!(" name={name}");
				}
				println!();
			}
		},
		AuthAnswer::Refreshed(account) => println!("{} {}", account.account, account.provider),
		AuthAnswer::LoggedOut(account) => println!("{account}"),
		AuthAnswer::Submitted(session) => println!("{session}"),
	}
	Ok(())
}

fn read_prompt(prompt: &AuthPrompt) -> miette::Result<AuthInput> {
	let mut stdout = io::stdout().lock();
	write!(stdout, "{}: ", prompt.message).into_diagnostic()?;
	stdout.flush().into_diagnostic()?;
	drop(stdout);

	let stdin = io::stdin();
	let hide_input = !matches!(
		prompt.input,
		InferenceAuthPromptKind::Confirmation | InferenceAuthPromptKind::PlainText
	) && stdin.is_terminal();
	let original = if hide_input {
		let original = tcgetattr(&stdin).into_diagnostic()?;
		let mut hidden = original.clone();
		hidden.local_flags.remove(LocalFlags::ECHO);
		tcsetattr(&stdin, SetArg::TCSANOW, &hidden).into_diagnostic()?;
		Some(original)
	} else {
		None
	};
	let mut value = Zeroizing::new(String::new());
	let read = stdin.read_line(&mut value);
	if let Some(original) = original {
		tcsetattr(&stdin, SetArg::TCSANOW, &original).into_diagnostic()?;
		println!();
	}
	if read.into_diagnostic()? == 0 {
		return Err(miette!("authentication input closed"));
	}
	auth_input(prompt, value.trim())
}

fn auth_input(prompt: &AuthPrompt, value: &str) -> miette::Result<AuthInput> {
	if value.is_empty()
		&& matches!(
			prompt.input,
			InferenceAuthPromptKind::AuthorizationCode
				| InferenceAuthPromptKind::ApiKey
				| InferenceAuthPromptKind::SessionToken
		) {
		return Err(miette!("authentication input must not be empty"));
	}
	Ok(match prompt.input {
		InferenceAuthPromptKind::AuthorizationCode => {
			if value.contains("://") {
				AuthInput::CallbackUrl(SecretString::from(value))
			} else {
				AuthInput::AuthorizationCode(SecretString::from(value))
			}
		},
		InferenceAuthPromptKind::ApiKey => AuthInput::ApiKey(SecretString::from(value)),
		InferenceAuthPromptKind::SessionToken => AuthInput::SessionToken(SecretString::from(value)),
		InferenceAuthPromptKind::PlainText => AuthInput::PlainText(Str::new(value)),
		InferenceAuthPromptKind::OptionalSecret => {
			AuthInput::OptionalSecret(SecretString::from(value))
		},
		InferenceAuthPromptKind::Confirmation => {
			if matches!(value.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes") {
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

	#[test]
	fn callback_prompt_submits_the_complete_url() {
		let prompt = AuthPrompt {
			id:      "oauth-callback-url".into(),
			message: "callback".into(),
			input:   InferenceAuthPromptKind::AuthorizationCode,
		};
		let AuthInput::CallbackUrl(value) =
			auth_input(&prompt, "http://localhost/callback?code=abc&state=xyz").unwrap()
		else {
			panic!("callback prompt must preserve the complete URL");
		};
		assert_eq!(value.expose_secret(), "http://localhost/callback?code=abc&state=xyz");
	}

	#[test]
	fn confirmation_accepts_an_empty_line() {
		let prompt = AuthPrompt {
			id:      "confirm".into(),
			message: "continue".into(),
			input:   InferenceAuthPromptKind::Confirmation,
		};
		assert!(matches!(auth_input(&prompt, "").unwrap(), AuthInput::DeviceConfirmed));
	}
}
