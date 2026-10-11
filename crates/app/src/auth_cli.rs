//! Interactive provider-authentication command presentation.

use std::{
	fs, future,
	io::{self, IsTerminal as _, Write},
	mem,
	path::{Path, PathBuf},
	sync::Arc,
	thread, time,
};

use flume::Receiver;
use miette::{IntoDiagnostic as _, miette};
use nix::sys::termios::{FlushArg, LocalFlags, SetArg, Termios, tcflush, tcgetattr, tcsetattr};
use omp_ai::{
	Client,
	account::{AccountName, AccountPool, AccountSelectError, AccountStateStore},
	answer::{
		AccountSummary, AuthAnswer, AuthEvent, AuthPrompt, AuthPromptKind as InferenceAuthPromptKind,
		AuthResponse, AuthSession,
	},
	auth::CredentialAuditEntry,
	call::{AuthInput, AuthRequest, CallMeta, LoginRequest, Target},
	id::{AccountId, RequestId},
	receipt::ExecutionBudget,
	router,
};
use omp_catalog::ProviderId;
use omp_core::{ExposeSecret as _, SecretString, Str};
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
			let account =
				login(&session, &mut io::stdout(), LoginTerminal::detect(), stdin_lines).await?;
			println!("{} {}", account.account, account.provider);
			println!("\nCredentials saved to {}", database.display());
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

/// Starts an OSC 8 hyperlink; its target follows, ended by
/// [`STRING_TERMINATOR`].
const OSC8_OPEN: &str = "\x1b]8;;";
/// Ends an OSC 8 hyperlink target.
const STRING_TERMINATOR: &str = "\x1b\\";
/// Closes the hyperlink an [`OSC8_OPEN`] opened.
const OSC8_CLOSE: &str = "\x1b]8;;\x1b\\";
/// Returns to the start of the line and erases it, removing a shown prompt.
const ERASE_LINE: &str = "\r\x1b[2K";

/// Shown when a required answer is empty; the prompt stays.
const EMPTY_INPUT: &str = "the input must not be empty";
/// Shown at the browser prompt for an empty line or a confirmation; the prompt
/// stays.
const BROWSER_PENDING: &str = "still waiting for the browser; paste the address it was redirected \
                               to if it cannot reach this machine, or n to cancel";
/// Shown at the browser prompt for a line that is not a callback URL; the
/// prompt stays.
const NOT_A_CALLBACK: &str =
	"not a callback URL: paste the complete address the browser was redirected to, or n to cancel";

/// One line the stdin reader read for a login prompt; `None` at the end of
/// input.
type InputLine = io::Result<Option<Zeroizing<String>>>;

/// Which standard streams of the process are terminals, decided once per login.
#[derive(Clone, Copy)]
struct LoginTerminal {
	/// Standard output is a terminal: URLs are hyperlinks, the browser is
	/// opened, and a withdrawn prompt is erased.
	output: bool,
	/// Standard input is a terminal: it echoes what is typed, except at a secret
	/// prompt.
	input:  bool,
}

impl LoginTerminal {
	fn detect() -> Self {
		Self { output: io::stdout().is_terminal(), input: io::stdin().is_terminal() }
	}
}

/// Drives one interactive login to its account.
///
/// Shows the provider's URLs and prompts and answers each prompt with a typed
/// line, while it keeps listening to the session: when the browser's callback
/// completes the login first, the prompt is withdrawn instead of waiting for
/// input. An empty or unusable line keeps the prompt, the end of input ends the
/// login only when no loopback callback is served (none can then answer it),
/// Ctrl-C cancels the login, and `input` starts the line reader at the first
/// prompt.
async fn login<W: Write>(
	session: &AuthSession,
	out: &mut W,
	terminal: LoginTerminal,
	input: impl FnOnce() -> io::Result<Receiver<InputLine>>,
) -> miette::Result<AccountSummary> {
	let mut screen = Screen { out, terminal, shown: None };
	let mut input = Some(input);
	let mut lines = None;
	let mut input_closed = false;
	let mut callback_served = false;
	let mut waiting_shown = false;
	let interrupt = interrupted();
	tokio::pin!(interrupt);
	loop {
		let line = next_line(
			lines
				.clone()
				.filter(|_| screen.shown.is_some() && !input_closed),
		);
		tokio::select! {
			event = session.events.recv_async() => {
				let event = match event {
					Ok(Ok(event)) => event,
					Ok(Err(error)) => {
						screen.withdraw()?;
						return Err(error).into_diagnostic();
					},
					Err(_) => {
						screen.withdraw()?;
						return Err(miette!("the login ended without a result"));
					},
				};
				match event {
					AuthEvent::OpenUrl { url, launch } => {
						// Only a flow that serves the loopback callback names a launch URL.
						callback_served |= launch.is_some();
						if terminal.output {
							omp_core::open::open_path(launch.as_deref().unwrap_or(&url));
						}
						screen.above(|out, terminal| show_url(out, terminal, &url, launch.as_deref()))?;
					},
					AuthEvent::ShowDeviceCode { code, verification_url } => {
						screen.above(|out, terminal| {
							write!(out, "complete device authorization at ")?;
							write_link(out, terminal, &verification_url)?;
							writeln!(out, " using code {}", code.expose_secret())
						})?;
					},
					AuthEvent::Prompt(prompt) => {
						if lines.is_none()
							&& let Some(start) = input.take()
						{
							lines = Some(start().into_diagnostic()?);
						}
						screen.prompt(prompt)?;
					},
					AuthEvent::Waiting => {
						if !mem::replace(&mut waiting_shown, true) {
							screen.above(|out, _| writeln!(out, "waiting for provider authorization"))?;
						}
					},
					AuthEvent::Complete(account) => {
						screen.withdraw()?;
						return Ok(account);
					},
				}
			},
			line = line => match line {
				Some(Ok(Some(text))) => {
					let Some(prompt) = screen.answered()? else { continue };
					match prompt_answer(&prompt, text.trim()) {
						Ok(answer) => {
							// Fails only once the flow has ended, which the session's events report.
							let _ = session
								.responses
								.send_async(AuthResponse { session: session.id.clone(), input: answer })
								.await;
						},
						Err(notice) => {
							screen.above(|out, _| writeln!(out, "{notice}"))?;
							screen.prompt(prompt)?;
						},
					}
				},
				Some(Err(error)) => {
					screen.withdraw()?;
					session.cancel();
					return Err(error).into_diagnostic();
				},
				None | Some(Ok(None)) => {
					input_closed = true;
					// The browser's redirect can still complete a login that serves the callback.
					if !callback_served {
						screen.withdraw()?;
						session.cancel();
						return Err(miette!("authentication input closed"));
					}
				},
			},
			() = &mut interrupt => {
				screen.withdraw()?;
				session.cancel();
				return Err(miette!("login cancelled"));
			},
		}
	}
}

/// The next line `lines` reads; never while the login reads no input, and
/// `None` once the reader is gone.
async fn next_line(lines: Option<Receiver<InputLine>>) -> Option<InputLine> {
	match lines {
		Some(lines) => lines.recv_async().await.ok(),
		None => future::pending().await,
	}
}

/// Completes on Ctrl-C; never when the handler cannot be installed, so Ctrl-C
/// then ends the process as it would without one.
async fn interrupted() {
	if tokio::signal::ctrl_c().await.is_err() {
		future::pending::<()>().await;
	}
}

/// Reads stdin lines for login prompts on a detached thread.
///
/// Not `spawn_blocking`: when the browser callback completes the login first,
/// the read left pending must not hold up the runtime's shutdown (it waits for
/// blocking tasks), and nothing reads stdin after a login. The channel holds no
/// line ahead of the prompt that takes it.
fn stdin_lines() -> io::Result<Receiver<InputLine>> {
	let (sender, lines) = flume::bounded(0);
	thread::Builder::new()
		.name("omp-auth-input".to_owned())
		.spawn(move || {
			loop {
				let mut line = Zeroizing::new(String::new());
				let read = match io::stdin().read_line(&mut line) {
					Ok(0) => Ok(None),
					Ok(_) => Ok(Some(line)),
					Err(error) => Err(error),
				};
				let last = !matches!(read, Ok(Some(_)));
				if sender.send(read).is_err() || last {
					break;
				}
			}
		})?;
	Ok(lines)
}

/// The login's output, holding at most one prompt on screen.
struct Screen<'w, W> {
	out:      &'w mut W,
	terminal: LoginTerminal,
	shown:    Option<ShownPrompt>,
}

/// A prompt on screen waiting for its line.
struct ShownPrompt {
	prompt: AuthPrompt,
	/// Present while the prompt hides what is typed; restores echo when dropped.
	hidden: Option<HiddenInput>,
}

impl<W: Write> Screen<'_, W> {
	/// Shows `prompt`, hiding what is typed when it asks for a secret on a
	/// terminal.
	fn prompt(&mut self, prompt: AuthPrompt) -> miette::Result<()> {
		self.withdraw()?;
		write_prompt_label(self.out, &prompt.message).into_diagnostic()?;
		self.out.flush().into_diagnostic()?;
		let hidden = (self.terminal.input && masks_input(prompt.input))
			.then(HiddenInput::hide)
			.transpose()?;
		self.shown = Some(ShownPrompt { prompt, hidden });
		Ok(())
	}

	/// Writes lines above the shown prompt, which is shown again after them.
	fn above(
		&mut self,
		write: impl FnOnce(&mut W, LoginTerminal) -> io::Result<()>,
	) -> miette::Result<()> {
		if self.shown.is_some() {
			self.end_prompt_line()?;
		}
		write(self.out, self.terminal).into_diagnostic()?;
		if let Some(shown) = &self.shown {
			write_prompt_label(self.out, &shown.prompt.message).into_diagnostic()?;
		}
		self.out.flush().into_diagnostic()
	}

	/// Takes the shown prompt a typed line answers, ending its line where the
	/// terminal did not echo the Enter.
	fn answered(&mut self) -> miette::Result<Option<AuthPrompt>> {
		let Some(ShownPrompt { prompt, hidden }) = self.shown.take() else {
			return Ok(None);
		};
		let echoed = self.terminal.input && hidden.is_none();
		drop(hidden);
		if !echoed {
			writeln!(self.out).into_diagnostic()?;
		}
		Ok(Some(prompt))
	}

	/// Removes the shown prompt, as when the browser completes the login, and
	/// discards anything typed at it without an Enter, which would otherwise
	/// reach the shell after omp exits.
	fn withdraw(&mut self) -> miette::Result<()> {
		let Some(ShownPrompt { hidden, .. }) = self.shown.take() else {
			return Ok(());
		};
		drop(hidden);
		self.end_prompt_line()?;
		if self.terminal.input {
			tcflush(io::stdin(), FlushArg::TCIFLUSH).into_diagnostic()?;
		}
		self.out.flush().into_diagnostic()
	}

	/// Erases the prompt's line on a terminal; elsewhere, ends it.
	fn end_prompt_line(&mut self) -> miette::Result<()> {
		if self.terminal.output {
			self.out.write_all(ERASE_LINE.as_bytes())
		} else {
			writeln!(self.out)
		}
		.into_diagnostic()
	}
}

/// Terminal settings a secret prompt replaced to stop echoing input, restored
/// when dropped so that every way out of a prompt (an answer, the browser
/// callback, an error) gives echo back.
struct HiddenInput(Termios);

impl HiddenInput {
	fn hide() -> miette::Result<Self> {
		let stdin = io::stdin();
		let original = tcgetattr(&stdin).into_diagnostic()?;
		let mut hidden = original.clone();
		hidden.local_flags.remove(LocalFlags::ECHO);
		tcsetattr(&stdin, SetArg::TCSANOW, &hidden).into_diagnostic()?;
		Ok(Self(original))
	}
}

impl Drop for HiddenInput {
	fn drop(&mut self) {
		if let Err(error) = tcsetattr(io::stdin(), SetArg::TCSANOW, &self.0) {
			tracing::warn!(%error, "could not restore terminal echo after a login prompt");
		}
	}
}

/// Writes a prompt's message before the typed answer, with a colon unless the
/// provider's message already ends a sentence.
fn write_prompt_label<W: Write>(out: &mut W, message: &str) -> io::Result<()> {
	if message.ends_with(['.', ':', '?', '!']) {
		write!(out, "{message} ")
	} else {
		write!(out, "{message}: ")
	}
}

/// Whether a prompt of `kind` hides what is typed.
const fn masks_input(kind: InferenceAuthPromptKind) -> bool {
	!matches!(kind, InferenceAuthPromptKind::Confirmation | InferenceAuthPromptKind::PlainText)
}

/// Writes where to authorize: the short loopback launch URL when the flow
/// serves one, which only this machine's browser reaches, and then the full URL
/// for a browser elsewhere.
fn show_url<W: Write>(
	out: &mut W,
	terminal: LoginTerminal,
	url: &str,
	launch: Option<&str>,
) -> io::Result<()> {
	writeln!(out)?;
	if terminal.output {
		writeln!(out, "Opening your browser to authorize. If it does not open, visit:")?;
	} else {
		writeln!(out, "Open this URL in your browser to authorize:")?;
	}
	write!(out, "  ")?;
	write_link(out, terminal, launch.unwrap_or(url))?;
	writeln!(out)?;
	if launch.is_some() {
		writeln!(
			out,
			"From another machine, open this URL and paste back the address it redirects to:"
		)?;
		write!(out, "  ")?;
		write_link(out, terminal, url)?;
		writeln!(out)?;
	}
	writeln!(out)
}

/// Writes `url`, on a terminal as an OSC 8 hyperlink to itself so it opens with
/// a click even where it wraps. A URL with control characters
/// (provider-supplied text) is written escaped.
fn write_link<W: Write>(out: &mut W, terminal: LoginTerminal, url: &str) -> io::Result<()> {
	if url.chars().any(char::is_control) {
		write!(out, "{}", url.escape_debug())
	} else if terminal.output {
		write!(out, "{OSC8_OPEN}{url}{STRING_TERMINATOR}{url}{OSC8_CLOSE}")
	} else {
		write!(out, "{url}")
	}
}

/// The input one typed line gives `prompt`, or the notice to show while the
/// prompt stays.
///
/// The browser prompt (a confirmation, the only one a provider flow asks) ends
/// with the browser's redirect, so an empty line or a yes keeps it waiting; it
/// takes a pasted callback URL from a browser that cannot reach this machine,
/// and a no cancels.
fn prompt_answer(prompt: &AuthPrompt, value: &str) -> Result<AuthInput, &'static str> {
	Ok(match prompt.input {
		InferenceAuthPromptKind::AuthorizationCode
		| InferenceAuthPromptKind::ApiKey
		| InferenceAuthPromptKind::SessionToken
			if value.is_empty() =>
		{
			return Err(EMPTY_INPUT);
		},
		InferenceAuthPromptKind::AuthorizationCode => pasted_callback(value)
			.unwrap_or_else(|| AuthInput::AuthorizationCode(SecretString::from(value))),
		InferenceAuthPromptKind::ApiKey => AuthInput::ApiKey(SecretString::from(value)),
		InferenceAuthPromptKind::SessionToken => AuthInput::SessionToken(SecretString::from(value)),
		InferenceAuthPromptKind::PlainText => AuthInput::PlainText(Str::new(value)),
		InferenceAuthPromptKind::OptionalSecret => {
			AuthInput::OptionalSecret(SecretString::from(value))
		},
		InferenceAuthPromptKind::Confirmation => {
			if let Some(callback) = pasted_callback(value) {
				callback
			} else if value.eq_ignore_ascii_case("n") || value.eq_ignore_ascii_case("no") {
				AuthInput::Cancel
			} else if value.is_empty()
				|| value.eq_ignore_ascii_case("y")
				|| value.eq_ignore_ascii_case("yes")
			{
				return Err(BROWSER_PENDING);
			} else {
				return Err(NOT_A_CALLBACK);
			}
		},
	})
}

/// A pasted `scheme://` value is the address the browser was redirected to:
/// it answers a code or browser prompt as the complete callback URL. Shared by
/// the `omp auth login` loop and the chat login dialog.
pub(crate) fn pasted_callback(value: &str) -> Option<AuthInput> {
	value
		.contains("://")
		.then(|| AuthInput::CallbackUrl(SecretString::from(value)))
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use omp_ai::{
		answer::AccountState,
		auth::{LoginDriver, default_login_channels},
		id::LoginSessionId,
	};

	use super::*;

	const PLAIN: LoginTerminal = LoginTerminal { output: false, input: false };
	const LAUNCH: &str = "http://127.0.0.1:9/launch";
	const AUTHORIZE: &str = "https://auth.example/authorize?state=s";

	fn prompt(input: InferenceAuthPromptKind) -> AuthPrompt {
		AuthPrompt { id: "p".into(), message: "Complete authorization".into(), input }
	}

	fn account() -> AccountSummary {
		AccountSummary {
			account:   AccountId::from("acct"),
			provider:  ProviderId::from("example"),
			principal: None,
			label:     None,
			state:     AccountState::Active,
		}
	}

	/// Types `text` and Enter; returns once the login took the line.
	async fn type_line(typed: &flume::Sender<InputLine>, text: &str) {
		typed
			.send_async(Ok(Some(Zeroizing::new(text.to_owned()))))
			.await
			.expect("the login reads the line");
	}

	/// Runs `login` against `flow` over a rendezvous line channel, so a typed
	/// line is taken before `flow` goes on.
	async fn drive<F: Future<Output = ()>>(
		flow: impl FnOnce(LoginDriver, flume::Sender<InputLine>) -> F,
	) -> (miette::Result<AccountSummary>, String, AuthSession) {
		let (session, driver, _) = default_login_channels(LoginSessionId::from("cli"));
		let (typed, lines) = flume::bounded(0);
		// The reader stays alive unless a flow sends the end of input.
		let reader = typed.clone();
		let mut out = Vec::new();
		let result = tokio::time::timeout(Duration::from_secs(10), async {
			let (result, ()) =
				tokio::join!(login(&session, &mut out, PLAIN, || Ok(lines)), flow(driver, typed));
			result
		})
		.await
		.expect("the login settles without waiting for more input");
		drop(reader);
		(result, String::from_utf8(out).expect("UTF-8 output"), session)
	}

	#[tokio::test]
	async fn a_browser_callback_withdraws_the_waiting_prompt() {
		let (result, out, _) = drive(|driver, typed| async move {
			driver
				.emit(AuthEvent::OpenUrl { url: AUTHORIZE.into(), launch: Some(LAUNCH.into()) })
				.await
				.expect("url");
			driver
				.emit(AuthEvent::Prompt(prompt(InferenceAuthPromptKind::Confirmation)))
				.await
				.expect("prompt");
			// No line is ever typed: the callback completes the login on its own.
			driver
				.emit(AuthEvent::Complete(account()))
				.await
				.expect("complete");
			drop(typed);
		})
		.await;
		assert_eq!(result.expect("login").account.as_str(), "acct");
		assert!(out.contains(&format!("  {LAUNCH}\n")), "{out}");
		assert!(out.contains(&format!("  {AUTHORIZE}\n")), "{out}");
		assert!(out.ends_with("Complete authorization: \n"), "{out}");
	}

	#[tokio::test]
	async fn empty_and_unusable_lines_keep_the_browser_prompt_waiting() {
		let (result, out, _) = drive(|driver, typed| async move {
			driver
				.emit(AuthEvent::Prompt(prompt(InferenceAuthPromptKind::Confirmation)))
				.await
				.expect("prompt");
			for text in ["", "yes", "not-a-url", "http://127.0.0.1:9/callback?code=c"] {
				type_line(&typed, text).await;
			}
			// Lines are answered in order, so only the callback URL answered the prompt.
			assert!(matches!(driver.receive().await, Ok(AuthInput::CallbackUrl(_))));
			driver
				.emit(AuthEvent::Complete(account()))
				.await
				.expect("complete");
		})
		.await;
		result.expect("login");
		assert_eq!(out.matches(BROWSER_PENDING).count(), 2, "{out}");
		assert_eq!(out.matches(NOT_A_CALLBACK).count(), 1, "{out}");
	}

	#[tokio::test]
	async fn a_pasted_callback_url_answers_the_browser_prompt() {
		let (result, ..) = drive(|driver, typed| async move {
			driver
				.emit(AuthEvent::Prompt(prompt(InferenceAuthPromptKind::Confirmation)))
				.await
				.expect("prompt");
			type_line(&typed, "http://127.0.0.1:9/callback?code=c&state=s").await;
			let AuthInput::CallbackUrl(callback) = driver.receive().await.expect("answer") else {
				panic!("a pasted callback URL answers the browser prompt");
			};
			assert_eq!(callback.expose_secret(), "http://127.0.0.1:9/callback?code=c&state=s");
			driver
				.emit(AuthEvent::Complete(account()))
				.await
				.expect("complete");
		})
		.await;
		result.expect("login");
	}

	#[tokio::test]
	async fn an_empty_code_keeps_its_prompt_until_a_code_is_typed() {
		let (result, out, _) = drive(|driver, typed| async move {
			driver
				.emit(AuthEvent::Prompt(prompt(InferenceAuthPromptKind::AuthorizationCode)))
				.await
				.expect("prompt");
			type_line(&typed, "").await;
			type_line(&typed, "the-code").await;
			let AuthInput::AuthorizationCode(code) = driver.receive().await.expect("answer") else {
				panic!("a typed code answers the code prompt");
			};
			assert_eq!(code.expose_secret(), "the-code");
			driver
				.emit(AuthEvent::Complete(account()))
				.await
				.expect("complete");
		})
		.await;
		result.expect("login");
		assert_eq!(out.matches(EMPTY_INPUT).count(), 1, "{out}");
	}

	#[tokio::test]
	async fn the_end_of_input_still_waits_for_a_served_callback() {
		let (result, ..) = drive(|driver, typed| async move {
			driver
				.emit(AuthEvent::OpenUrl { url: AUTHORIZE.into(), launch: Some(LAUNCH.into()) })
				.await
				.expect("url");
			// The paste prompt a callback-serving flow shows (`PasteCallbackUrl`).
			driver
				.emit(AuthEvent::Prompt(prompt(InferenceAuthPromptKind::AuthorizationCode)))
				.await
				.expect("prompt");
			typed.send_async(Ok(None)).await.expect("end of input");
			// The login reads nothing after the end of input, and keeps the session alive.
			let late = tokio::time::timeout(Duration::from_millis(200), type_line(&typed, "late"));
			assert!(late.await.is_err(), "no line is read after the end of input");
			driver
				.check_cancelled()
				.expect("the browser can still complete the login");
			driver
				.emit(AuthEvent::Complete(account()))
				.await
				.expect("complete");
		})
		.await;
		result.expect("login");
	}

	#[tokio::test]
	async fn the_end_of_input_cancels_a_login_no_callback_can_finish() {
		let (result, _, session) = drive(|driver, typed| async move {
			driver
				.emit(AuthEvent::Prompt(prompt(InferenceAuthPromptKind::AuthorizationCode)))
				.await
				.expect("prompt");
			typed.send_async(Ok(None)).await.expect("end of input");
			driver.wait_cancelled().await;
		})
		.await;
		let error = result.expect_err("no code can arrive");
		assert_eq!(error.to_string(), "authentication input closed");
		drop(session);
	}

	#[tokio::test]
	async fn a_session_that_ends_without_an_account_is_an_error() {
		let (result, ..) = drive(|driver, _typed| async move {
			driver
				.emit(AuthEvent::Prompt(prompt(InferenceAuthPromptKind::Confirmation)))
				.await
				.expect("prompt");
			drop(driver);
		})
		.await;
		assert_eq!(result.expect_err("no account").to_string(), "the login ended without a result");
	}

	#[test]
	fn prompt_answers() {
		let code = prompt(InferenceAuthPromptKind::AuthorizationCode);
		let browser = prompt(InferenceAuthPromptKind::Confirmation);
		let AuthInput::CallbackUrl(value) =
			prompt_answer(&code, "http://localhost/callback?code=abc&state=xyz").unwrap()
		else {
			panic!("a code prompt keeps the complete callback URL");
		};
		assert_eq!(value.expose_secret(), "http://localhost/callback?code=abc&state=xyz");
		assert!(matches!(prompt_answer(&code, "abc"), Ok(AuthInput::AuthorizationCode(_))));
		assert_eq!(prompt_answer(&code, "").err(), Some(EMPTY_INPUT));
		assert_eq!(
			prompt_answer(&prompt(InferenceAuthPromptKind::ApiKey), "").err(),
			Some(EMPTY_INPUT)
		);
		assert!(matches!(
			prompt_answer(&prompt(InferenceAuthPromptKind::OptionalSecret), ""),
			Ok(AuthInput::OptionalSecret(_))
		));
		assert!(matches!(
			prompt_answer(&browser, "https://127.0.0.1:9/callback?code=c"),
			Ok(AuthInput::CallbackUrl(_))
		));
		assert!(matches!(prompt_answer(&browser, "N"), Ok(AuthInput::Cancel)));
		for waiting in ["", "y", "Yes"] {
			assert_eq!(prompt_answer(&browser, waiting).err(), Some(BROWSER_PENDING));
		}
		assert_eq!(prompt_answer(&browser, "maybe").err(), Some(NOT_A_CALLBACK));
	}

	#[test]
	fn a_prompt_label_ends_with_one_separator() {
		let mut out = Vec::new();
		write_prompt_label(&mut out, "Paste the authorization code").unwrap();
		write_prompt_label(&mut out, "|paste it when prompted.").unwrap();
		assert_eq!(out, b"Paste the authorization code: |paste it when prompted. ");
	}

	#[test]
	fn urls_are_hyperlinks_only_on_a_terminal() {
		let terminal = LoginTerminal { output: true, input: false };
		let mut out = Vec::new();
		write_link(&mut out, terminal, LAUNCH).unwrap();
		assert_eq!(out, format!("\x1b]8;;{LAUNCH}\x1b\\{LAUNCH}\x1b]8;;\x1b\\").into_bytes());
		out.clear();
		write_link(&mut out, PLAIN, LAUNCH).unwrap();
		assert_eq!(out, LAUNCH.as_bytes());
		out.clear();
		write_link(&mut out, terminal, "https://x.example/\x1b]8;;evil").unwrap();
		assert_eq!(out, b"https://x.example/\\u{1b}]8;;evil");
	}
}
