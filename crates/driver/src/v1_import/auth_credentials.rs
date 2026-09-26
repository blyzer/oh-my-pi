//! The `credentials` step: v1 `agent.db` logins into the encrypted store.
//!
//! v1 kept every login as a plaintext JSON row of `auth_credentials`
//! (`packages/ai/src/auth/sqlite-credential-store.ts`): `api_key` rows
//! (`{key, source?}`), `oauth` rows (`{access, refresh, expires, …extras}`),
//! and MCP OAuth grants in the same table under `mcp_oauth…` credential ids.
//! The database is opened read-only and never locked or journalled against
//! a running v1 (see [`open_read_only`]).
//!
//! # Mapping
//!
//! - An `api_key` row is stored through
//!   [`omp_ai::auth::AuthControlHandle::store`] as an `api-key` credential. Its
//!   identity is v1's `identity_key` when set, else `api-key` for a key v1's
//!   `/login` stored (the account v2's own `/login` writes), else `agent-db`.
//! - An `oauth` row is imported through
//!   [`omp_ai::auth::AuthControlHandle::import_oauth`] (access, refresh,
//!   expiry) under its v1 identity (`email:…|org:…`). Per v1 extra, checked
//!   against how v2 logs in and builds requests:
//!   - `projectId`: Cloud Code Assist (the `google-antigravity` and
//!     `google-gemini-cli` logins, whose v2 exchange discovers it) refuses to
//!     encode a request without the account's routing project, and v2 cannot
//!     rediscover it offline: it becomes the account's routing project. A row
//!     without it needs a fresh `/login`.
//!   - `enterpriseUrl`: GitHub Copilot on GitHub Enterprise. v2's login has no
//!     enterprise path; its request shaper reads the enterprise domain from a
//!     `{token, enterpriseUrl, apiEndpoint}` credential envelope, so such a
//!     login is stored as that envelope. Elsewhere (Alibaba Coding Plan) it is
//!     the chosen endpoint: kept when it is the provider's v2 endpoint, else
//!     reported, since v2 configures endpoints per provider.
//!   - `apiEndpoint`: v2 probes the Copilot plan endpoint at request time.
//!   - `accountId`, `email`, `orgId`, `orgName`: identity only. v2 derives the
//!     Codex workspace (`chatgpt-account-id`) and its residency from the access
//!     token.
//!   - `authorizedAt`: v2 has no grant-deadline warning.
//! - A login whose v2 provider takes only a static key (a v1 `oauth` row
//!   without a refresh token, such as Kilo's) is stored as an `api-key` with
//!   its v1 expiry.
//! - A disabled row is imported, then disabled in the v2 account pool.
//! - An MCP grant goes to the MCP host's store record for its server URL (from
//!   the credential id, or v1 `mcp.json` for legacy random ids), with the v1
//!   refresh material (token endpoint, client id and secret, resource).
//!
//! Reports name the provider (or MCP server), identity, and kind only.

use std::{
	collections::{BTreeMap, HashSet},
	fs, io, mem,
	path::{Path, PathBuf},
	sync::Arc,
	time::{SystemTime, UNIX_EPOCH},
};

use omp_ai::{
	PrincipalId, ProjectId,
	auth::{CredentialControlWrite, OAuthControlImport, StoreError, normalize_enterprise_domain},
};
use omp_catalog::{
	ProviderId,
	provider::{AuthSpecKind, OAuthExchangeKind, OAuthFlowSpec},
	snapshot::Catalog,
};
use omp_core::{Secret, SecretString, Str, sf};
use omp_envd::mcp::auth_authority::{CombinedAuthAuthority, McpOAuthGrant, McpOAuthStoreError};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item,
	report::{Attention, NotMigratable, SkipReason},
};

/// The provider whose credentials v2 shapes from a JSON envelope carrying a
/// GitHub Enterprise domain (`omp_ai::auth::GithubCopilotShaper`).
const GITHUB_COPILOT: &str = "github-copilot";

/// The profile v2's MCP host keys every grant under, inside each v2
/// profile's own store (`omp_envd::mcp::manager`).
const MCP_AFFINITY_PROFILE: &str = "default";

/// v1 credential ids minted by its MCP OAuth flows:
/// `mcp_oauth:profile:<p>:<url>`, `mcp_oauth:<url>`, and the legacy random
/// `mcp_oauth_<id>`.
const MCP_URL_PREFIX: &str = "mcp_oauth:";
const MCP_PROFILE_PREFIX: &str = "mcp_oauth:profile:";
const MCP_LEGACY_PREFIX: &str = "mcp_oauth_";

/// A failure reading v1's credentials or writing them into v2.
#[derive(Debug, Error)]
pub enum CredentialsImportError {
	/// `agent.db` could not be opened read-only.
	#[error("could not open the v1 credential database {} read-only", path.display())]
	Open {
		/// The v1 database.
		path:   PathBuf,
		/// The SQLite failure.
		#[source]
		source: rusqlite::Error,
	},
	/// `auth_credentials` could not be read.
	#[error("could not read the v1 credentials in {}", path.display())]
	Read {
		/// The v1 database.
		path:   PathBuf,
		/// The SQLite failure.
		#[source]
		source: rusqlite::Error,
	},
	/// A provider credential could not be stored or disabled.
	#[error("could not store the v1 credential for {provider}")]
	Store {
		/// The provider the credential authenticates.
		provider: Str,
		/// The store failure.
		#[source]
		source:   StoreError,
	},
	/// An MCP grant could not be stored.
	#[error("could not store the v1 MCP grant for {server}")]
	McpStore {
		/// The MCP server name, or its host.
		server: Str,
		/// The store failure.
		#[source]
		source: McpOAuthStoreError,
	},
	/// The step's marker could not be written.
	#[error("could not record the credentials import")]
	Marker(#[source] io::Error),
}

/// The shape of a reported credential.
#[derive(Clone, Copy, Debug, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum Kind {
	ApiKey,
	#[strum(to_string = "oauth")]
	OAuth,
	McpOauth,
}

/// One `auth_credentials` row. `data` is plaintext secret JSON.
struct Row {
	provider:        String,
	credential_type: String,
	data:            Zeroizing<String>,
	disabled:        Option<String>,
	identity_key:    Option<String>,
}

/// v1 `api_key` row data.
#[derive(Deserialize, Zeroize)]
struct ApiKeyData {
	key:    String,
	#[serde(default)]
	source: Option<String>,
}

/// v1 `oauth` row data (`OAuthCredentials`, plus `MCPStoredOAuthCredential`'s
/// refresh material on MCP rows).
#[derive(Default, Deserialize, Zeroize)]
#[serde(default, rename_all = "camelCase")]
struct OAuthData {
	access:         String,
	refresh:        String,
	expires:        f64,
	email:          Option<String>,
	account_id:     Option<String>,
	project_id:     Option<String>,
	enterprise_url: Option<String>,
	api_endpoint:   Option<String>,
	token_url:      Option<String>,
	client_id:      Option<String>,
	client_secret:  Option<String>,
	resource:       Option<String>,
}

/// v2's GitHub Copilot credential envelope
/// (`omp_ai::auth::parse_copilot_api_key`).
#[derive(Serialize)]
struct CopilotEnvelope<'a> {
	token:          &'a str,
	#[serde(rename = "enterpriseUrl")]
	enterprise_url: &'a str,
	#[serde(rename = "apiEndpoint", skip_serializing_if = "Option::is_none")]
	api_endpoint:   Option<&'a str>,
}

/// The fields of v1 `mcp.json` that locate an MCP grant.
#[derive(Default, Deserialize)]
struct McpConfig {
	#[serde(rename = "mcpServers", default)]
	servers: BTreeMap<String, McpServer>,
}

#[derive(Deserialize)]
struct McpServer {
	#[serde(default)]
	url:  Option<String>,
	#[serde(default)]
	auth: Option<McpAuth>,
}

#[derive(Default, Deserialize, Zeroize)]
#[serde(default, rename_all = "camelCase")]
struct McpAuth {
	credential_id: Option<String>,
	token_url:     Option<String>,
	client_id:     Option<String>,
	client_secret: Option<String>,
}

/// What the step does with one row.
enum Plan {
	/// A provider account.
	Account(AccountWrite),
	/// An MCP grant through [`CombinedAuthAuthority::import_mcp_oauth`].
	Mcp { url: String, grant: McpOAuthGrant },
	/// Nothing to write; report this.
	Report(ImportOutcome),
}

/// How a provider account is written.
enum AccountWrite {
	/// A scalar credential through `AuthControlHandle::store`.
	Key { secret: Secret, expires_at_ms: Option<u64> },
	/// A renewable login through `AuthControlHandle::import_oauth`.
	OAuth {
		access:        SecretString,
		refresh:       SecretString,
		expires_at_ms: Option<u64>,
		project:       Option<ProjectId>,
	},
}

/// One planned row.
struct Planned {
	provider:   Str,
	/// Store account identity (`<provider>:<identity>`); MCP rows use their
	/// server URL's affinity instead.
	identity:   Str,
	/// v1 principals (email, account id) an existing v2 login of the same
	/// person carries.
	principals: [Option<String>; 2],
	subject:    Str,
	disabled:   Option<Str>,
	plan:       Plan,
}

impl Planned {
	const fn writes(&self) -> bool {
		!matches!(self.plan, Plan::Report(_))
	}
}

pub(super) fn import_credentials(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let path = cx.locate(V1Item::AgentDb);
	let entry = |subject: Option<Str>, outcome| ImportEntry {
		step: ImportStep::Credentials,
		item: V1Item::AgentDb,
		path: path.clone(),
		subject,
		outcome,
	};
	let rows = match &path {
		Some(path) => read_rows(path)?,
		None => Vec::new(),
	};
	let planned = if rows.is_empty() {
		Vec::new()
	} else {
		let catalog = Catalog::try_embedded().map_err(ImportError::Catalog)?;
		let mcp = cx
			.locate(V1Item::Mcp)
			.and_then(|path| fs::read(path).ok())
			.and_then(|bytes| serde_json::from_slice::<McpConfig>(&bytes).ok())
			.unwrap_or_default();
		plan_rows(rows, catalog, &mcp)
	};
	if cx.mode == ImportMode::DryRun {
		if planned.is_empty() {
			return Ok(vec![entry(None, ImportOutcome::NothingToImport)]);
		}
		return Ok(planned
			.into_iter()
			.map(|planned| {
				let outcome = match planned.plan {
					Plan::Report(outcome) => outcome,
					_ => ImportOutcome::WouldImport,
				};
				entry(Some(planned.subject), outcome)
			})
			.collect());
	}
	let mut entries = Vec::with_capacity(planned.len().max(1));
	let mut mcp = None;
	for planned in planned {
		let subject = planned.subject.clone();
		entries.push(entry(Some(subject), apply(cx, &mut mcp, planned)?));
	}
	ImportStep::Credentials
		.marker(&cx.pair.target.config_dir)
		.set(None)
		.map_err(CredentialsImportError::Marker)?;
	if entries.is_empty() {
		entries.push(entry(None, ImportOutcome::NothingToImport));
	}
	Ok(entries)
}

/// Writes one planned row, keeping any v2 account of the same identity. The
/// target store opens on the first row that writes.
fn apply(
	cx: &StepContext<'_>,
	mcp: &mut Option<CombinedAuthAuthority>,
	planned: Planned,
) -> Result<ImportOutcome, ImportError> {
	let Planned { provider, identity, principals, subject, disabled, plan } = planned;
	let store_error = |source| CredentialsImportError::Store { provider: provider.clone(), source };
	let (control, write) = match plan {
		Plan::Report(outcome) => return Ok(outcome),
		Plan::Mcp { url, grant } => {
			let authority = match mcp {
				Some(authority) => authority,
				None => mcp.insert(CombinedAuthAuthority::new(Arc::clone(
					cx.credentials.get()?.credential_store(),
				))),
			};
			let affinity = CombinedAuthAuthority::mcp_affinity(
				MCP_AFFINITY_PROFILE,
				&url,
				PrincipalId::from(MCP_AFFINITY_PROFILE),
			);
			let stored = authority
				.import_mcp_oauth(&affinity, grant, unix_ms_now())
				.map_err(|source| CredentialsImportError::McpStore { server: subject, source })?;
			return Ok(match stored {
				Some(_) => ImportOutcome::Imported,
				None => ImportOutcome::Skipped(SkipReason::AccountExists),
			});
		},
		Plan::Account(write) => (cx.credentials.get()?, write),
	};
	let account = sf!("{provider}:{identity}");
	let existing = control
		.accounts(Some(ProviderId::from_ref(provider.as_str())))
		.into_iter()
		.any(|record| {
			record.account.as_str() == account.as_str()
				|| principals.iter().flatten().any(|principal| {
					record
						.principal
						.as_str()
						.eq_ignore_ascii_case(principal.as_str())
				})
		});
	if existing {
		return Ok(ImportOutcome::Skipped(SkipReason::AccountExists));
	}
	let provider_id = ProviderId::from(provider.as_str());
	let principal = PrincipalId::from(identity.as_str());
	let written = match write {
		AccountWrite::OAuth { access, refresh, expires_at_ms, project } => {
			control.import_oauth(OAuthControlImport {
				provider: provider_id,
				principal,
				identity: Some(identity),
				access_token: Some(access),
				refresh_token: refresh,
				expires_at_ms,
				project,
			})
		},
		AccountWrite::Key { secret, expires_at_ms } => control.store(CredentialControlWrite {
			provider: provider_id,
			principal,
			identity: Some(identity),
			kind: Str::new_static(Kind::ApiKey.into()),
			secret,
			expires_at_ms,
		}),
	};
	let (_, record) = written.map_err(store_error)?;
	if let Some(cause) = disabled {
		control
			.set_enabled(&record.account, false, Some(cause.as_str()))
			.map_err(store_error)?;
	}
	Ok(ImportOutcome::Imported)
}

/// Opens v1's database without writing, locking against, or journalling next
/// to it.
///
/// At rest (no `-wal` file) the file is opened `immutable`: SQLite takes no
/// locks and creates no `-wal`/`-shm` siblings. While v1 runs (or after it
/// crashed) the newest logins may live only in its `-wal`, so the database
/// is opened `mode=ro` instead: a read-only connection reads through v1's
/// shared-memory index under shared locks and can never checkpoint, so v1
/// is not disturbed.
fn open_read_only(path: &Path) -> Result<Connection, rusqlite::Error> {
	let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
	let mut wal = absolute.clone().into_os_string();
	wal.push("-wal");
	let query = if Path::new(&wal).exists() {
		"mode=ro"
	} else {
		"mode=ro&immutable=1"
	};
	let uri = url::Url::from_file_path(&absolute).map_or_else(
		|()| format!("file:{}?{query}", absolute.display()),
		|mut url| {
			url.set_query(Some(query));
			url.into()
		},
	);
	let connection = Connection::open_with_flags(
		uri,
		OpenFlags::SQLITE_OPEN_READ_ONLY
			| OpenFlags::SQLITE_OPEN_URI
			| OpenFlags::SQLITE_OPEN_NO_MUTEX,
	)?;
	connection.busy_timeout(std::time::Duration::from_secs(5))?;
	Ok(connection)
}

/// Every `auth_credentials` row, enabled rows first, oldest first.
fn read_rows(path: &Path) -> Result<Vec<Row>, CredentialsImportError> {
	let connection = open_read_only(path)
		.map_err(|source| CredentialsImportError::Open { path: path.to_owned(), source })?;
	let read = |source| CredentialsImportError::Read { path: path.to_owned(), source };
	// Columns older v1 schemas lacked read as NULL.
	let columns = connection
		.prepare("SELECT name FROM pragma_table_info('auth_credentials')")
		.and_then(|mut statement| {
			statement
				.query_map([], |row| row.get::<_, String>(0))?
				.collect::<Result<HashSet<_>, _>>()
		})
		.map_err(read)?;
	if columns.is_empty() {
		return Ok(Vec::new());
	}
	let column = |name: &'static str| if columns.contains(name) { name } else { "NULL" };
	let disabled = column("disabled_cause");
	let order = if columns.contains("id") {
		"id"
	} else {
		"rowid"
	};
	let query = format!(
		"SELECT provider, credential_type, data, {disabled}, {identity} FROM auth_credentials ORDER \
		 BY {disabled} IS NOT NULL, {order}",
		identity = column("identity_key"),
	);
	let mut statement = connection.prepare(&query).map_err(read)?;
	statement
		.query_map([], |row| {
			Ok(Row {
				provider:        row.get(0)?,
				credential_type: row.get(1)?,
				data:            Zeroizing::new(row.get(2)?),
				disabled:        row.get(3)?,
				identity_key:    row.get(4)?,
			})
		})
		.and_then(Iterator::collect)
		.map_err(read)
}

fn plan_rows(rows: Vec<Row>, catalog: &Catalog, mcp: &McpConfig) -> Vec<Planned> {
	let mut seen = HashSet::new();
	rows
		.into_iter()
		.map(|row| {
			let mut planned = plan_row(row, catalog, mcp);
			// The first (enabled, oldest) credential of an identity wins.
			if planned.writes() && !seen.insert(sf!("{}\0{}", planned.provider, planned.identity)) {
				planned.plan = Plan::Report(ImportOutcome::Skipped(SkipReason::DuplicateIdentity));
			}
			planned
		})
		.collect()
}

fn plan_row(row: Row, catalog: &Catalog, mcp: &McpConfig) -> Planned {
	let Row { provider, credential_type, data, disabled, identity_key } = row;
	let disabled = disabled.map(|cause| {
		let cause = cause.trim();
		Str::new(if cause.is_empty() { "disabled" } else { cause })
	});
	let identity_key = identity_key
		.as_deref()
		.map(str::trim)
		.filter(|key| !key.is_empty())
		.map(Str::new);
	let planned = |kind: Kind, identity: Str, principals, plan| {
		let kind: &'static str = kind.into();
		let subject = match &disabled {
			Some(_) => sf!("{provider} {identity} ({kind}, disabled in v1)"),
			None => sf!("{provider} {identity} ({kind})"),
		};
		Planned {
			provider: Str::new(&provider),
			identity,
			principals,
			subject,
			disabled: disabled.clone(),
			plan,
		}
	};
	let relogin = || Plan::Report(ImportOutcome::NeedsAttention(Attention::ReloginRequired));
	match credential_type.as_str() {
		"api_key" => {
			let Ok(mut key) = serde_json::from_str::<ApiKeyData>(&data).map(Zeroizing::new) else {
				return planned(Kind::ApiKey, sf!("agent-db"), [None, None], relogin());
			};
			let identity = identity_key.unwrap_or_else(|| {
				Str::new_static(if key.source.as_deref() == Some("login") {
					"api-key"
				} else {
					"agent-db"
				})
			});
			let plan = if key.key.is_empty() {
				relogin()
			} else {
				Plan::Account(AccountWrite::Key {
					secret:        Secret::from(mem::take(&mut key.key).into_bytes()),
					expires_at_ms: None,
				})
			};
			planned(Kind::ApiKey, identity, [None, None], plan)
		},
		"oauth" if is_mcp_credential(&provider) => {
			let (subject, plan) = plan_mcp(&provider, disabled.is_some(), &data, mcp);
			Planned {
				provider: Str::new(&provider),
				identity: Str::new(&provider),
				principals: [None, None],
				subject: sf!("{subject} ({})", <&'static str>::from(Kind::McpOauth)),
				disabled: None,
				plan,
			}
		},
		"oauth" => {
			let Ok(mut oauth) = serde_json::from_str::<OAuthData>(&data).map(Zeroizing::new) else {
				return planned(
					Kind::OAuth,
					identity_key.unwrap_or_else(|| sf!("agent-db")),
					[None, None],
					relogin(),
				);
			};
			let identity = identity_key.unwrap_or_else(|| fallback_identity(&oauth));
			let principals = [
				nonempty(oauth.email.as_deref()).map(str::to_lowercase),
				nonempty(oauth.account_id.as_deref()).map(str::to_owned),
			];
			let plan = plan_oauth(catalog, &provider, &mut oauth);
			planned(Kind::OAuth, identity, principals, plan)
		},
		other => Planned {
			provider:   Str::new(&provider),
			identity:   Str::new(&provider),
			principals: [None, None],
			subject:    sf!("{provider} ({other})"),
			disabled:   None,
			plan:       Plan::Report(ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent)),
		},
	}
}

/// v1's identity for an OAuth row without a stored `identity_key`: its
/// account, else email, else project (`resolveProviderCredentialIdentityKey`).
fn fallback_identity(oauth: &OAuthData) -> Str {
	if let Some(account) = nonempty(oauth.account_id.as_deref()) {
		return sf!("account:{account}");
	}
	if let Some(email) = nonempty(oauth.email.as_deref()) {
		return sf!("email:{}", email.to_lowercase());
	}
	if let Some(project) = nonempty(oauth.project_id.as_deref()) {
		return sf!("project:{project}");
	}
	Str::new_static("agent-db")
}

/// Classifies one v1 OAuth login against the v2 provider it authenticates.
fn plan_oauth(catalog: &Catalog, provider: &str, oauth: &mut OAuthData) -> Plan {
	let relogin = || Plan::Report(ImportOutcome::NeedsAttention(Attention::ReloginRequired));
	let Some(definition) = catalog.provider(ProviderId::from_ref(provider)) else {
		return Plan::Report(ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent));
	};
	let specs = || {
		definition
			.auth
			.iter()
			.filter_map(|id| catalog.auth_spec(id))
	};
	let flow = specs()
		.filter(|spec| spec.kind == AuthSpecKind::Oauth)
		.find_map(|spec| catalog.oauth_spec(spec.oauth.as_ref()?))
		.map(|spec| &spec.flow);
	let scalar = specs().any(|spec| {
		matches!(
			spec.kind,
			AuthSpecKind::ApiKey | AuthSpecKind::Bearer | AuthSpecKind::OptionalBearer
		)
	});
	if oauth.access.is_empty() {
		return relogin();
	}
	let expires_at_ms = expiry_ms(oauth.expires);
	if let Some(enterprise) = nonempty(oauth.enterprise_url.as_deref()) {
		if provider == GITHUB_COPILOT {
			if let Some(domain) = normalize_enterprise_domain(enterprise) {
				// v1 keeps the GitHub token as both access and refresh.
				let token = if oauth.refresh.is_empty() {
					&oauth.access
				} else {
					&oauth.refresh
				};
				let envelope = serde_json::to_vec(&CopilotEnvelope {
					token,
					enterprise_url: &domain,
					api_endpoint: nonempty(oauth.api_endpoint.as_deref()),
				})
				.map(Zeroizing::new);
				return match envelope {
					Ok(mut envelope) => Plan::Account(AccountWrite::Key {
						secret:        Secret::from(mem::take(&mut *envelope)),
						expires_at_ms: None,
					}),
					Err(_) => relogin(),
				};
			}
		} else if !catalog.routes().iter().any(|route| {
			route.provider.as_str() == provider
				&& route.endpoint.base_url.trim_end_matches('/') == enterprise.trim_end_matches('/')
		}) {
			return Plan::Report(ImportOutcome::NeedsAttention(Attention::CustomEndpoint));
		}
	}
	let project = match flow {
		Some(OAuthFlowSpec::Custom {
			exchange: OAuthExchangeKind::GoogleAntigravity | OAuthExchangeKind::GoogleGeminiCli,
			..
		}) => match nonempty(oauth.project_id.as_deref()) {
			Some(project) => Some(ProjectId::from(project)),
			None => return relogin(),
		},
		_ => None,
	};
	if flow.is_some() && !oauth.refresh.is_empty() {
		return Plan::Account(AccountWrite::OAuth {
			access: SecretString::from(mem::take(&mut oauth.access)),
			refresh: SecretString::from(mem::take(&mut oauth.refresh)),
			expires_at_ms,
			project,
		});
	}
	if scalar {
		return Plan::Account(AccountWrite::Key {
			secret: Secret::from(mem::take(&mut oauth.access).into_bytes()),
			expires_at_ms,
		});
	}
	relogin()
}

fn is_mcp_credential(provider: &str) -> bool {
	provider.starts_with(MCP_URL_PREFIX) || provider.starts_with(MCP_LEGACY_PREFIX)
}

/// The server URL a URL-keyed MCP credential id embeds.
fn mcp_url(credential_id: &str) -> Option<&str> {
	let url = match credential_id.strip_prefix(MCP_PROFILE_PREFIX) {
		Some(scoped) => scoped.split_once(':')?.1,
		None => credential_id.strip_prefix(MCP_URL_PREFIX)?,
	};
	(!url.is_empty()).then_some(url)
}

/// Plans one MCP grant; the subject is the server's `mcp.json` name, else its
/// host (a URL may carry secrets in its query).
fn plan_mcp(credential_id: &str, disabled: bool, data: &str, mcp: &McpConfig) -> (Str, Plan) {
	let reauthorize = || Plan::Report(ImportOutcome::NeedsAttention(Attention::McpReauthorize));
	let url = mcp_url(credential_id);
	let server = mcp
		.servers
		.iter()
		.find(|(_, server)| {
			server
				.auth
				.as_ref()
				.and_then(|auth| auth.credential_id.as_deref())
				== Some(credential_id)
		})
		.or_else(|| {
			url.and_then(|url| {
				mcp.servers
					.iter()
					.find(|(_, server)| server.url.as_deref() == Some(url))
			})
		});
	let url = url.or_else(|| server.and_then(|(_, server)| server.url.as_deref()));
	let subject = match (server, url) {
		(Some((name, _)), _) => sf!("mcp {name}"),
		(None, Some(url)) => url::Url::parse(url)
			.ok()
			.and_then(|url| url.host_str().map(|host| sf!("mcp {host}")))
			.unwrap_or_else(|| sf!("mcp {credential_id}")),
		(None, None) => sf!("mcp {credential_id}"),
	};
	let Some(url) = url else {
		return (subject, reauthorize());
	};
	if disabled {
		return (subject, reauthorize());
	}
	let Ok(mut oauth) = serde_json::from_str::<OAuthData>(data).map(Zeroizing::new) else {
		return (subject, reauthorize());
	};
	let config = server.and_then(|(_, server)| server.auth.as_ref());
	let pick = |own: &mut Option<String>, configured: Option<&Option<String>>| {
		own.take().filter(|value| !value.is_empty()).or_else(|| {
			configured
				.and_then(Option::clone)
				.filter(|value| !value.is_empty())
		})
	};
	let token_endpoint = pick(&mut oauth.token_url, config.map(|auth| &auth.token_url));
	let client_id = pick(&mut oauth.client_id, config.map(|auth| &auth.client_id));
	let (Some(token_endpoint), Some(client_id)) = (token_endpoint, client_id) else {
		return (subject, reauthorize());
	};
	if oauth.access.is_empty() {
		return (subject, reauthorize());
	}
	let client_secret = pick(&mut oauth.client_secret, config.map(|auth| &auth.client_secret));
	let grant = McpOAuthGrant {
		access_token:   SecretString::from(mem::take(&mut oauth.access)),
		refresh_token:  Some(mem::take(&mut oauth.refresh))
			.filter(|token| !token.is_empty())
			.map(SecretString::from),
		token_endpoint: Str::from(token_endpoint),
		client_id:      Str::from(client_id),
		client_secret:  client_secret.map(SecretString::from),
		resource:       oauth
			.resource
			.take()
			.filter(|value| !value.is_empty())
			.map(Str::from),
		expires_at_ms:  expiry_ms(oauth.expires),
	};
	(subject, Plan::Mcp { url: url.to_owned(), grant })
}

fn nonempty(value: Option<&str>) -> Option<&str> {
	value.map(str::trim).filter(|value| !value.is_empty())
}

/// v1's absolute expiry in Unix milliseconds. Values too small to be
/// milliseconds are seconds, as v1's `normalizeOAuthCredentialExpiry` read
/// them; a missing or non-positive expiry is unknown.
fn expiry_ms(expires: f64) -> Option<u64> {
	if !expires.is_finite() || expires <= 0.0 {
		return None;
	}
	let millis = if expires < 10_000_000_000.0 {
		expires * 1000.0
	} else {
		expires
	};
	#[allow(
		clippy::cast_possible_truncation,
		clippy::cast_sign_loss,
		reason = "finite, positive, and saturated by the float-to-int cast"
	)]
	let millis = millis as u64;
	Some(millis)
}

fn unix_ms_now() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "auth_credentials_tests.rs"]
mod tests;
