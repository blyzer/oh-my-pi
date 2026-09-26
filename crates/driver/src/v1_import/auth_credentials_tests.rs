//! Proofs for the `credentials` step over temporary homes, v1 `agent.db`s
//! built with v1's schema, and throwaway v2 stores. Nothing here reads the
//! process environment or the real `~/.omp` / `~/.o2`.

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use omp_ai::{
	AccountId,
	account::AccountPool,
	auth::{
		AuditedCredentialReveal, AuthControlHandle, CredentialControlWrite, CredentialStore,
		HeadlessKeySource, KeyId,
	},
};
use omp_catalog::ProviderId;
use omp_core::{ExposeSecret as _, Str};
use omp_envd::mcp::auth_authority::CombinedAuthAuthority;
use rusqlite::Connection;
use serde::Deserialize;

use super::super::{
	Attention, CredentialAccess, ImportMode, ImportOutcome, ImportPair, ImportReport, ImportStep,
	OutcomeKind, ProfileSelection, SkipReason, V1Inputs, V1Source, V2Roots, plan, run,
};

/// `auth_credentials` exactly as v1's `#createAuthCredentialsTable` creates it.
const V1_SCHEMA: &str = "
	CREATE TABLE IF NOT EXISTS auth_credentials (
		id INTEGER PRIMARY KEY AUTOINCREMENT,
		provider TEXT NOT NULL,
		credential_type TEXT NOT NULL,
		data TEXT NOT NULL,
		disabled_cause TEXT DEFAULT NULL,
		identity_key TEXT DEFAULT NULL,
		created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER)),
		updated_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER))
	);
	CREATE INDEX IF NOT EXISTS idx_auth_provider ON auth_credentials(provider);
	CREATE INDEX IF NOT EXISTS idx_auth_provider_identity ON auth_credentials(provider, identity_key)
		WHERE identity_key IS NOT NULL;
";

/// Every secret the fixtures hold; none may reach a report.
const SECRETS: &[&str] = &[
	"sk-ant-login-key",
	"sk-or-stored-key",
	"sk-duplicate-key",
	"anthropic-access-token",
	"anthropic-refresh-token",
	"gemini-access-token",
	"gemini-refresh-token",
	"gho_enterprise_token",
	"mcp-access-token",
	"mcp-refresh-token",
	"mcp-client-secret",
	"legacy-mcp-access",
	"url-query-secret",
];

const ANTHROPIC_OAUTH: &str = concat!(
	r#"{"access":"anthropic-access-token","refresh":"anthropic-refresh-token","#,
	r#""expires":1893456000000,"email":"Owner@Example.com","orgId":"org-1","#,
	r#""orgName":"Team","authorizedAt":1700000000000}"#,
);
const ANTHROPIC_IDENTITY: &str = "email:owner@example.com|org:org-1";

/// One v1 row: provider, type, data, disabled cause, identity key.
type V1Row<'a> = (&'a str, &'a str, &'a str, Option<&'a str>, Option<&'a str>);

fn write(path: &Path, contents: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, contents).expect("write");
}

/// Builds a v1 `agent.db` the way v1 leaves it at rest: WAL journal mode,
/// last connection closed (checkpointed, no `-wal`/`-shm`).
fn agent_db(path: &Path, rows: &[V1Row<'_>]) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	let connection = Connection::open(path).expect("agent.db");
	connection
		.pragma_update(None, "journal_mode", "WAL")
		.expect("wal");
	connection.execute_batch(V1_SCHEMA).expect("schema");
	insert(&connection, rows);
}

fn insert(connection: &Connection, rows: &[V1Row<'_>]) {
	for (provider, kind, data, disabled, identity) in rows {
		connection
			.execute(
				"INSERT INTO auth_credentials (provider, credential_type, data, disabled_cause, \
				 identity_key) VALUES (?1, ?2, ?3, ?4, ?5)",
				rusqlite::params![provider, kind, data, disabled, identity],
			)
			.expect("row");
	}
}

fn roots(root: &Path) -> V2Roots {
	V2Roots {
		config_dir:     root.join("o2"),
		data_dir:       root.join("share/omp"),
		state_dir:      root.join("state/omp"),
		cache_dir:      root.join("cache/omp"),
		active_profile: None,
	}
}

/// Every file (with its bytes) and directory under `root`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
	let mut tree = BTreeMap::new();
	let mut pending = vec![root.to_owned()];
	while let Some(directory) = pending.pop() {
		let Ok(entries) = fs::read_dir(&directory) else {
			continue;
		};
		for entry in entries {
			let path = entry.expect("entry").path();
			if path.is_dir() {
				tree.insert(path.clone(), None);
				pending.push(path);
			} else {
				tree.insert(path.clone(), Some(fs::read(&path).expect("read")));
			}
		}
	}
	tree
}

/// A control handle over a throwaway encrypted store under `root`.
fn control(root: &Path) -> (AuthControlHandle, Arc<CredentialStore>) {
	fs::create_dir_all(root).expect("store dir");
	let store = Arc::new(
		CredentialStore::open(
			root.join("credentials.sqlite"),
			Arc::new(HeadlessKeySource::new(KeyId::new("v1-credentials-test"), [0x5a; 32])),
		)
		.expect("store"),
	);
	let catalog = Arc::new(omp_catalog::Catalog::embedded().clone());
	let control =
		AuthControlHandle::offline(catalog, Arc::clone(&store), AccountPool::new()).expect("control");
	(control, store)
}

/// The stored secret bytes of one account.
fn reveal(store: &CredentialStore, account: &str) -> Vec<u8> {
	// Every audited reveal carries its own request id.
	static REQUEST: AtomicU64 = AtomicU64::new(1);
	let audit = AuditedCredentialReveal {
		extension:          Str::new_static("v1-import-test"),
		caller_principal:   Str::new_static("test"),
		provider:           Str::new_static("test"),
		host_generation:    1,
		session_generation: 1,
		request_id:         REQUEST.fetch_add(1, Ordering::Relaxed),
		reason:             Str::new_static("verify the imported credential"),
	};
	store
		.with_audited_secret(AccountId::from_ref(account), &audit, |secret| {
			secret.expose(<[u8]>::to_vec)
		})
		.expect("reveal")
}

/// The (access, refresh) of v2's opaque `ORCB1` OAuth bundle.
fn oauth_bundle(bytes: &[u8]) -> (String, String) {
	let mut input = bytes.strip_prefix(b"ORCB1").expect("bundle magic");
	let mut field = || {
		let length = u32::from_be_bytes(input[..4].try_into().expect("length")) as usize;
		let value = String::from_utf8(input[4..4 + length].to_vec()).expect("utf-8");
		input = &input[4 + length..];
		value
	};
	let access = field();
	(access, field())
}

fn subjects(report: &ImportReport) -> Vec<(Option<&str>, OutcomeKind)> {
	report
		.entries()
		.filter(|entry| entry.step == ImportStep::Credentials)
		.map(|entry| (entry.subject.as_deref(), entry.outcome.kind()))
		.collect()
}

fn outcome<'r>(report: &'r ImportReport, subject: &str) -> &'r ImportOutcome {
	&report
		.entries()
		.find(|entry| entry.subject.as_deref() == Some(subject))
		.unwrap_or_else(|| panic!("no entry for {subject}"))
		.outcome
}

/// A scratch root holding a fake home (`home/.omp`) and the v2 roots.
struct Fixture {
	root: tempfile::TempDir,
}

impl Fixture {
	fn new() -> Self {
		Self { root: tempfile::tempdir().expect("scratch") }
	}

	fn home(&self) -> PathBuf {
		self.root.path().join("home")
	}

	fn agent(&self) -> PathBuf {
		self.home().join(".omp/agent")
	}

	fn pairs(&self, selection: &ProfileSelection) -> Vec<ImportPair> {
		let source = V1Source::new(V1Inputs { home: self.home(), ..V1Inputs::default() });
		plan(&source, &roots(self.root.path()), selection).expect("plan")
	}

	fn default_pairs(&self) -> Vec<ImportPair> {
		self.pairs(&ProfileSelection::Named(None))
	}

	/// Applies the default profile with its live store, as its first run does.
	fn apply(&self, control: &AuthControlHandle) -> ImportReport {
		let v2 = roots(self.root.path());
		run(&self.default_pairs(), ImportMode::Apply, CredentialAccess::Live {
			data_dir: &v2.data_dir,
			control,
		})
	}
}

#[test]
fn api_keys_import_readable_once_and_leave_v1_untouched() {
	let fixture = Fixture::new();
	let db = fixture.agent().join("agent.db");
	agent_db(&db, &[
		("anthropic", "api_key", r#"{"key":"sk-ant-login-key","source":"login"}"#, None, None),
		("openrouter", "api_key", r#"{"key":"sk-or-stored-key"}"#, None, None),
	]);
	let v1_before = snapshot(&fixture.home());
	let (control, store) = control(&fixture.root.path().join("store"));

	let report = fixture.apply(&control);

	assert_eq!(subjects(&report), [
		(Some("anthropic api-key (api-key)"), OutcomeKind::Imported),
		(Some("openrouter agent-db (api-key)"), OutcomeKind::Imported),
	]);
	// A key v1's `/login` stored lands on the account v2's own `/login`
	// writes; any other on `agent-db`.
	assert_eq!(reveal(&store, "anthropic:api-key"), b"sk-ant-login-key");
	assert_eq!(reveal(&store, "openrouter:agent-db"), b"sk-or-stored-key");
	let metadata = control
		.metadata(AccountId::from_ref("anthropic:api-key"))
		.expect("metadata")
		.expect("stored");
	assert_eq!(metadata.kind.as_str(), "api-key");
	assert_eq!(
		control
			.accounts(Some(ProviderId::from_ref("openrouter")))
			.len(),
		1
	);
	let config = fixture.root.path().join("o2");
	assert!(ImportStep::Credentials.marker(&config).is_set());
	// Copy only, and read-only: v1's database bytes are unchanged and no
	// `-wal`/`-shm` appeared next to it.
	assert_eq!(snapshot(&fixture.home()), v1_before);
	assert!(!fixture.agent().join("agent.db-wal").exists());
	assert!(!fixture.agent().join("agent.db-shm").exists());

	// A second run is a no-op through the marker, even after v1 changes.
	let connection = Connection::open(&db).expect("v1 db");
	insert(&connection, &[("zai", "api_key", r#"{"key":"sk-late"}"#, None, None)]);
	drop(connection);
	let again = fixture.apply(&control);
	assert!(
		again
			.entries()
			.filter(|entry| entry.step == ImportStep::Credentials)
			.all(|entry| matches!(entry.outcome, ImportOutcome::Skipped(SkipReason::MarkerPresent)))
	);
	assert_eq!(store.list_metadata().expect("metadata").len(), 2);
}

#[test]
fn a_dry_run_writes_nothing_and_reports_what_it_would_import() {
	let fixture = Fixture::new();
	agent_db(&fixture.agent().join("agent.db"), &[
		("anthropic", "api_key", r#"{"key":"sk-ant-login-key","source":"login"}"#, None, None),
		("anthropic", "oauth", ANTHROPIC_OAUTH, None, Some(ANTHROPIC_IDENTITY)),
		("muse-code", "oauth", r#"{"access":"a","refresh":"r","expires":1}"#, None, None),
	]);
	let before = snapshot(fixture.root.path());

	let report = run(
		&fixture.pairs(&ProfileSelection::All),
		ImportMode::DryRun,
		CredentialAccess::Offline(&omp_con::Ctx::new()),
	);

	assert_eq!(snapshot(fixture.root.path()), before, "a dry run must not write anywhere");
	assert_eq!(subjects(&report), [
		(Some("anthropic api-key (api-key)"), OutcomeKind::WouldImport),
		(Some("anthropic email:owner@example.com|org:org-1 (oauth)"), OutcomeKind::WouldImport),
		// A login for a provider v2 does not have.
		(Some("muse-code agent-db (oauth)"), OutcomeKind::NotMigratable),
	]);
}

#[test]
fn a_credential_whose_identity_v2_already_has_is_skipped() {
	let fixture = Fixture::new();
	agent_db(&fixture.agent().join("agent.db"), &[
		("anthropic", "api_key", r#"{"key":"sk-ant-login-key","source":"login"}"#, None, None),
		("openrouter", "api_key", r#"{"key":"sk-or-stored-key"}"#, None, None),
		("openrouter", "api_key", r#"{"key":"sk-duplicate-key"}"#, None, None),
		("anthropic", "oauth", ANTHROPIC_OAUTH, None, Some(ANTHROPIC_IDENTITY)),
	]);
	let (control, store) = control(&fixture.root.path().join("store"));
	// The owner already ran `/login` in v2 for the key and for the OAuth
	// account (whose v2 principal is the login email).
	for identity in ["api-key", "owner@example.com"] {
		control
			.store(CredentialControlWrite {
				provider:      ProviderId::from("anthropic"),
				principal:     omp_ai::PrincipalId::from(identity),
				identity:      Some(Str::new(identity)),
				kind:          Str::new_static("api-key"),
				secret:        omp_core::Secret::from(b"v2-own-key".to_vec()),
				expires_at_ms: None,
			})
			.expect("v2 login");
	}

	let report = fixture.apply(&control);

	assert!(matches!(
		outcome(&report, "anthropic api-key (api-key)"),
		ImportOutcome::Skipped(SkipReason::AccountExists)
	));
	assert!(matches!(
		outcome(&report, "anthropic email:owner@example.com|org:org-1 (oauth)"),
		ImportOutcome::Skipped(SkipReason::AccountExists)
	));
	// Two v1 keys with one identity: the first is taken, the second reported.
	let openrouter = report
		.entries()
		.filter(|entry| entry.subject.as_deref() == Some("openrouter agent-db (api-key)"))
		.map(|entry| &entry.outcome)
		.collect::<Vec<_>>();
	assert!(matches!(openrouter[..], [
		ImportOutcome::Imported,
		ImportOutcome::Skipped(SkipReason::DuplicateIdentity)
	]));
	assert_eq!(reveal(&store, "anthropic:api-key"), b"v2-own-key", "the v2 login is kept");
	assert_eq!(reveal(&store, "openrouter:agent-db"), b"sk-or-stored-key");
}

#[test]
fn oauth_logins_import_access_refresh_and_expiry() {
	let fixture = Fixture::new();
	agent_db(&fixture.agent().join("agent.db"), &[(
		"anthropic",
		"oauth",
		ANTHROPIC_OAUTH,
		None,
		Some(ANTHROPIC_IDENTITY),
	)]);
	let (control, store) = control(&fixture.root.path().join("store"));

	let report = fixture.apply(&control);

	assert_eq!(subjects(&report), [(
		Some("anthropic email:owner@example.com|org:org-1 (oauth)"),
		OutcomeKind::Imported
	)]);
	// The v1 identity keeps two subscriptions of one email apart.
	let account = "anthropic:email:owner@example.com|org:org-1";
	let metadata = control
		.metadata(AccountId::from_ref(account))
		.expect("metadata")
		.expect("stored");
	assert_eq!(metadata.kind.as_str(), "oauth-renewable-v1");
	assert_eq!(metadata.expires_at_ms, Some(1_893_456_000_000));
	assert_eq!(
		oauth_bundle(&reveal(&store, account)),
		("anthropic-access-token".to_owned(), "anthropic-refresh-token".to_owned())
	);
	let record = &control.accounts(Some(ProviderId::from_ref("anthropic")))[0];
	assert!(record.enabled);
	assert_eq!(record.routing.project, None);
}

#[test]
fn provider_extras_follow_what_v2_uses_at_request_time() {
	let fixture = Fixture::new();
	agent_db(&fixture.agent().join("agent.db"), &[
		// Cloud Code Assist needs the login-discovered project on every request.
		(
			"google-gemini-cli",
			"oauth",
			concat!(
				r#"{"access":"gemini-access-token","refresh":"gemini-refresh-token","#,
				r#""expires":1893456000000,"projectId":"proj-42","email":"dev@example.com"}"#,
			),
			None,
			Some("email:dev@example.com"),
		),
		// Without it, v2 cannot encode requests: a fresh login is needed.
		(
			"google-antigravity",
			"oauth",
			r#"{"access":"gemini-access-token","refresh":"gemini-refresh-token","expires":1893456000000}"#,
			None,
			Some("email:dev@example.com"),
		),
		// GitHub Enterprise Copilot rides v2's credential envelope.
		(
			"github-copilot",
			"oauth",
			concat!(
				r#"{"access":"gho_enterprise_token","refresh":"gho_enterprise_token","#,
				r#""expires":9007199254740991,"enterpriseUrl":"https://ghe.example.com","#,
				r#""apiEndpoint":"https://copilot-api.ghe.example.com"}"#,
			),
			None,
			None,
		),
		// A login bound to an endpoint v2 keeps per provider.
		(
			"alibaba-coding-plan",
			"oauth",
			concat!(
				r#"{"access":"k","refresh":"k","expires":9007199254740991,"#,
				r#""enterpriseUrl":"https://coding.dashscope.aliyuncs.com/v1"}"#,
			),
			None,
			None,
		),
	]);
	let (control, store) = control(&fixture.root.path().join("store"));

	let report = fixture.apply(&control);

	assert_eq!(subjects(&report), [
		(Some("google-gemini-cli email:dev@example.com (oauth)"), OutcomeKind::Imported),
		(Some("google-antigravity email:dev@example.com (oauth)"), OutcomeKind::NeedsAttention),
		(Some("github-copilot agent-db (oauth)"), OutcomeKind::Imported),
		(Some("alibaba-coding-plan agent-db (oauth)"), OutcomeKind::NeedsAttention),
	]);
	let gemini = &control.accounts(Some(ProviderId::from_ref("google-gemini-cli")))[0];
	assert_eq!(
		gemini
			.routing
			.project
			.as_ref()
			.map(|project| project.as_str()),
		Some("proj-42")
	);
	assert!(matches!(
		outcome(&report, "google-antigravity email:dev@example.com (oauth)"),
		ImportOutcome::NeedsAttention(Attention::ReloginRequired)
	));
	assert!(matches!(
		outcome(&report, "alibaba-coding-plan agent-db (oauth)"),
		ImportOutcome::NeedsAttention(Attention::CustomEndpoint)
	));
	let envelope = reveal(&store, "github-copilot:agent-db");
	let parsed = omp_ai::auth::parse_copilot_api_key(std::str::from_utf8(&envelope).expect("utf-8"));
	assert_eq!(parsed.access_token.expose_secret(), "gho_enterprise_token");
	assert_eq!(parsed.enterprise_url.as_deref(), Some("ghe.example.com"));
	assert_eq!(parsed.api_endpoint.as_deref(), Some("https://copilot-api.ghe.example.com"));
}

#[test]
fn a_codex_login_derives_its_residency_from_the_access_token() {
	let fixture = Fixture::new();
	let payload = omp_core::encoding::base64_url::encode_raw(
		br#"{"https://api.openai.com/auth":{"chatgpt_data_residency":"eu","chatgpt_account_id":"acct-1"}}"#,
	)
	.into_string();
	let data = format!(
		r#"{{"access":"e30.{payload}.sig","refresh":"r","expires":1893456000000,"accountId":"acct-1"}}"#
	);
	agent_db(&fixture.agent().join("agent.db"), &[(
		"openai-codex",
		"oauth",
		&data,
		None,
		Some("email:dev@example.com|org:acct-1"),
	)]);
	let (control, _) = control(&fixture.root.path().join("store"));

	fixture.apply(&control);

	let codex = &control.accounts(Some(ProviderId::from_ref("openai-codex")))[0];
	assert_eq!(codex.routing.region.as_ref().map(|region| region.as_str()), Some("eu"));
}

#[test]
fn a_disabled_v1_login_imports_disabled() {
	let fixture = Fixture::new();
	agent_db(&fixture.agent().join("agent.db"), &[(
		"anthropic",
		"oauth",
		ANTHROPIC_OAUTH,
		Some("invalid_grant"),
		Some(ANTHROPIC_IDENTITY),
	)]);
	let (control, _) = control(&fixture.root.path().join("store"));

	let report = fixture.apply(&control);

	assert_eq!(subjects(&report), [(
		Some("anthropic email:owner@example.com|org:org-1 (oauth, disabled in v1)"),
		OutcomeKind::Imported
	)]);
	let record = &control.accounts(Some(ProviderId::from_ref("anthropic")))[0];
	assert!(!record.enabled, "a disabled v1 login stays disabled");
}

#[derive(Deserialize)]
struct McpRecord {
	access_token:   String,
	refresh_token:  Option<String>,
	token_endpoint: String,
	client_id:      String,
	client_secret:  Option<String>,
	resource:       Option<String>,
	expires_at_ms:  Option<u64>,
}

#[test]
fn mcp_grants_import_into_the_mcp_hosts_record() {
	let fixture = Fixture::new();
	let url = "https://mcp.example.com/mcp?token=url-query-secret";
	let scoped = format!("mcp_oauth:profile:default:{url}");
	agent_db(&fixture.agent().join("agent.db"), &[
		(
			&scoped,
			"oauth",
			concat!(
				r#"{"access":"mcp-access-token","refresh":"mcp-refresh-token","#,
				r#""expires":1893456000000,"tokenUrl":"https://auth.example.com/token","#,
				r#""clientId":"client-1","clientSecret":"mcp-client-secret","#,
				r#""resource":"https://mcp.example.com/"}"#,
			),
			None,
			None,
		),
		// A legacy random id: its server and refresh material live in mcp.json.
		(
			"mcp_oauth_k3y",
			"oauth",
			r#"{"access":"legacy-mcp-access","refresh":"","expires":1893456000000}"#,
			None,
			None,
		),
		// Neither the id nor mcp.json names the server.
		("mcp_oauth_orphan", "oauth", r#"{"access":"x","refresh":"y","expires":1}"#, None, None),
	]);
	write(
		&fixture.agent().join("mcp.json"),
		r#"{"mcpServers":{
			"docs":{"type":"http","url":"https://mcp.example.com/mcp?token=url-query-secret"},
			"legacy":{"type":"http","url":"https://legacy.example.com/sse",
				"auth":{"type":"oauth","credentialId":"mcp_oauth_k3y",
					"tokenUrl":"https://legacy.example.com/token","clientId":"legacy-client"}}
		}}"#,
	);
	let (control, store) = control(&fixture.root.path().join("store"));

	let report = fixture.apply(&control);

	assert_eq!(subjects(&report), [
		(Some("mcp docs (mcp-oauth)"), OutcomeKind::Imported),
		(Some("mcp legacy (mcp-oauth)"), OutcomeKind::Imported),
		(Some("mcp mcp_oauth_orphan (mcp-oauth)"), OutcomeKind::NeedsAttention),
	]);
	let affinity = |url: &str| {
		CombinedAuthAuthority::mcp_affinity("default", url, omp_ai::PrincipalId::from("default"))
			.account
	};
	let record: McpRecord =
		serde_json::from_slice(&reveal(&store, affinity(url).as_str())).expect("record");
	assert_eq!(record.access_token, "mcp-access-token");
	assert_eq!(record.refresh_token.as_deref(), Some("mcp-refresh-token"));
	assert_eq!(record.token_endpoint, "https://auth.example.com/token");
	assert_eq!(record.client_id, "client-1");
	assert_eq!(record.client_secret.as_deref(), Some("mcp-client-secret"));
	assert_eq!(record.resource.as_deref(), Some("https://mcp.example.com/"));
	assert_eq!(record.expires_at_ms, Some(1_893_456_000_000));
	let legacy: McpRecord =
		serde_json::from_slice(&reveal(&store, affinity("https://legacy.example.com/sse").as_str()))
			.expect("legacy record");
	assert_eq!(legacy.access_token, "legacy-mcp-access");
	assert_eq!(legacy.token_endpoint, "https://legacy.example.com/token");
	assert_eq!(legacy.client_id, "legacy-client");
	assert_eq!(legacy.refresh_token, None);
	assert_eq!(store.list_metadata().expect("metadata").len(), 2);
	assert!(control.accounts(None).is_empty(), "MCP grants are not provider accounts");
}

#[test]
fn a_running_v1_is_read_through_its_wal_without_being_disturbed() {
	let fixture = Fixture::new();
	let db = fixture.agent().join("agent.db");
	agent_db(&db, &[]);
	// v1 is running: its connection holds an uncheckpointed write in `-wal`.
	let running = Connection::open(&db).expect("v1 connection");
	running
		.pragma_update(None, "wal_autocheckpoint", 0)
		.expect("no autocheckpoint");
	insert(&running, &[("openrouter", "api_key", r#"{"key":"sk-or-stored-key"}"#, None, None)]);
	assert!(fixture.agent().join("agent.db-wal").exists());
	let main_before = fs::read(&db).expect("main db");
	let (control, store) = control(&fixture.root.path().join("store"));

	let report = fixture.apply(&control);

	assert_eq!(subjects(&report), [(Some("openrouter agent-db (api-key)"), OutcomeKind::Imported)]);
	assert_eq!(reveal(&store, "openrouter:agent-db"), b"sk-or-stored-key");
	assert_eq!(fs::read(&db).expect("main db"), main_before, "a reader never checkpoints");
	// v1 keeps writing.
	insert(&running, &[("zai", "api_key", r#"{"key":"sk-late"}"#, None, None)]);
}

#[test]
fn reports_never_carry_secret_values() {
	let fixture = Fixture::new();
	agent_db(&fixture.agent().join("agent.db"), &[
		("anthropic", "api_key", r#"{"key":"sk-ant-login-key","source":"login"}"#, None, None),
		("anthropic", "oauth", ANTHROPIC_OAUTH, Some("invalid_grant"), None),
		(
			"google-antigravity",
			"oauth",
			r#"{"access":"gemini-access-token","refresh":"gemini-refresh-token","expires":1}"#,
			None,
			None,
		),
		(
			"mcp_oauth:https://mcp.example.com/mcp?token=url-query-secret",
			"oauth",
			r#"{"access":"mcp-access-token","refresh":"mcp-refresh-token","expires":1,"clientSecret":"mcp-client-secret"}"#,
			None,
			None,
		),
		("broken", "oauth", r#"{"access":"url-query-secret","expires":"not a number"}"#, None, None),
	]);
	let (control, _) = control(&fixture.root.path().join("store"));

	for report in [
		run(
			&fixture.default_pairs(),
			ImportMode::DryRun,
			CredentialAccess::Offline(&omp_con::Ctx::new()),
		),
		fixture.apply(&control),
	] {
		let mut rendered = format!("{report:?}");
		for entry in report.entries() {
			if let ImportOutcome::NeedsAttention(attention) = &entry.outcome {
				rendered.push_str(&attention.to_string());
			}
		}
		for secret in SECRETS {
			assert!(!rendered.contains(secret), "{secret} leaked into the report");
		}
		// An MCP server mcp.json does not name is reported by host alone.
		assert!(rendered.contains("mcp mcp.example.com (mcp-oauth)"), "{rendered}");
	}
}

#[test]
fn each_v1_profile_imports_into_its_own_v2_profile_store() {
	let fixture = Fixture::new();
	agent_db(&fixture.agent().join("agent.db"), &[(
		"anthropic",
		"api_key",
		r#"{"key":"sk-ant-login-key","source":"login"}"#,
		None,
		None,
	)]);
	agent_db(&fixture.home().join(".omp/profiles/work/agent/agent.db"), &[(
		"openrouter",
		"api_key",
		r#"{"key":"sk-or-stored-key"}"#,
		None,
		None,
	)]);
	let v2 = roots(fixture.root.path());
	let work = v2.target(Some("work"));
	let pairs = fixture.pairs(&ProfileSelection::All);
	let (control, store) = control(&work.data_dir);

	// The `work` profile's own first run: its live store owns `work`'s data
	// directory, so the default profile's credentials wait, unmarked.
	let report = run(&pairs, ImportMode::Apply, CredentialAccess::Live {
		data_dir: &work.data_dir,
		control:  &control,
	});

	let credentials = |profile: Option<&str>| {
		report
			.pairs
			.iter()
			.find(|pair| pair.target_profile.as_deref() == profile)
			.expect("pair")
			.entries
			.iter()
			.filter(|entry| entry.step == ImportStep::Credentials)
			.map(|entry| (entry.subject.as_deref(), &entry.outcome))
			.collect::<Vec<_>>()
	};
	assert!(matches!(credentials(None)[..], [(
		None,
		ImportOutcome::Skipped(SkipReason::WaitsForProfile)
	)]));
	assert!(matches!(credentials(Some("work"))[..], [(
		Some("openrouter agent-db (api-key)"),
		ImportOutcome::Imported
	)]));
	assert!(!ImportStep::Credentials.marker(&v2.config_dir).is_set());
	assert!(ImportStep::Credentials.marker(&work.config_dir).is_set());
	assert_eq!(
		store
			.list_metadata()
			.expect("metadata")
			.iter()
			.map(|row| row.account_id.as_str().to_owned())
			.collect::<Vec<_>>(),
		["openrouter:agent-db"],
		"only the work profile's v1 credentials reach the work store"
	);
}

#[test]
fn without_an_agent_db_nothing_is_opened_and_the_step_is_marked() {
	let fixture = Fixture::new();
	fs::create_dir_all(fixture.agent()).expect("agent dir");
	let v2 = roots(fixture.root.path());

	let report = run(
		&fixture.default_pairs(),
		ImportMode::Apply,
		CredentialAccess::Offline(&omp_con::Ctx::new()),
	);

	assert_eq!(subjects(&report), [(None, OutcomeKind::NothingToImport)]);
	assert!(ImportStep::Credentials.marker(&v2.config_dir).is_set());
	assert!(!v2.data_dir.exists(), "no credential store is created without a credential");
}
