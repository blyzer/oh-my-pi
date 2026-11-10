//! Resolver for `issue://` and `pr://`.
//!
//! A numbered resource is parsed with [`omp_forge::parse_resource_url`]. A
//! bare number uses [`infer_repo`]. A missing host is `github.com`. The body
//! comes from the forge [`omp_forge::ForgeHost`] (`view_change_request`,
//! `view_issue`, the unified diff, and the comment list) and is rendered as
//! `# title`, a blank line, and the body. The cache stores that markdown
//! with no etag.
//!
//! List URLs such as `issue://owner/repo?state=open` and `pr://…` also go
//! through [`ForgeHost::search`] (Dyn for `github.com` / `gitlab.com`, in-core
//! GitHub adapter for remaining GHE hosts). Neutral filter tokens
//! (`state:` / `author:` / `label:`) ride `SearchQuery.text`. The search path
//! has no HTTP etag, so list bodies are cached without one (same honesty as
//! numbered reads). The private GitHub REST client in this module remains for
//! legacy detail/diff fetch paths that still need it; checkout/push still
//! need `github.rs`.

use std::{
	collections::BTreeMap,
	fmt::{self, Display},
	path::{Path, PathBuf},
	str,
	sync::{Arc, OnceLock},
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bytes::BytesMut;
use futures::StreamExt as _;
use http::{
	HeaderMap, HeaderValue,
	header::{ACCEPT, ETAG, IF_NONE_MATCH, USER_AGENT},
};
use omp_ai::{
	auth::{
		CredentialError, CredentialFuture, CredentialLease, CredentialNeed, HeaderPlacement,
		LeaseMeta,
	},
	id::{AccountId, PrincipalId},
};
#[cfg(feature = "cap-forge")]
use omp_cache::forge_cache::{ForgeCache, ForgeCacheKey, ForgeCacheStatus, ForgeResourceKind};
use omp_catalog::AuthSpecId;
#[cfg(feature = "cap-forge")]
use omp_core::CowBytes;
use omp_core::{SecretString, Str, sf};
#[cfg(feature = "cap-forge")]
use omp_forge::{
	Comment, DiffSelector, ForgeError, ForgeHost, RepoRef, ResourceKind, ResourceSelector,
	SchemeError, SchemeRef, SearchHit, SearchKind, SearchPage, SearchQuery, parse_resource_url,
};
#[cfg(feature = "cap-forge")]
use omp_tool::{Diag, DiagKind};
use omp_tools::read::Fault;
#[cfg(feature = "cap-forge")]
use omp_tools::read::{
	resolver::{Resolve, ResolvedRead},
	selector::ParsedSelector,
};
use omp_vcs::git::GitRepo;
use parking_lot::{Mutex, MutexGuard};
#[cfg(feature = "cap-forge")]
use serde_json::Value;
#[cfg(feature = "cap-forge")]
use smallvec::smallvec;

use super::tool_url;

const MAX_BODY: usize = 8 * 1024 * 1024;

pub(super) const GITHUB_HOST: &str = "github.com";

#[derive(Clone, Debug)]
pub(super) struct GithubRepo {
	host:     Str,
	slug:     Str,
	identity: Str,
}

impl GithubRepo {
	pub(super) fn parse(value: &str) -> Result<Self, Fault> {
		let mut parts = value.trim().trim_end_matches(".git").split('/');
		let first = parts.next().unwrap_or_default();
		let second = parts.next().unwrap_or_default();
		let third = parts.next();
		if parts.next().is_some() {
			return Err(invalid("GitHub repository must be [host/]owner/repo."));
		}
		match third {
			Some(name) => Self::new(first, second, name),
			None => Self::new(GITHUB_HOST, first, second),
		}
	}

	pub(super) fn new(host: &str, owner: &str, name: &str) -> Result<Self, Fault> {
		if !valid_github_host(host) || !valid_repo_component(owner) || !valid_repo_component(name) {
			return Err(invalid("GitHub repository must be [host/]owner/repo."));
		}
		let host = Str::new(host);
		let slug = Str::new(format!("{owner}/{name}"));
		let identity = if host.eq_ignore_ascii_case(GITHUB_HOST) {
			slug.clone()
		} else {
			Str::new(format!("{host}/{slug}"))
		};
		Ok(Self { host, slug, identity })
	}

	pub(super) fn host(&self) -> &str {
		&self.host
	}

	pub(super) fn slug(&self) -> &str {
		&self.slug
	}

	pub(super) fn identity(&self) -> &str {
		&self.identity
	}

	pub(super) fn api_url(&self, path: &str) -> String {
		api_url_for_host(&self.host, path)
	}
}

pub(super) fn api_url_for_host(host: &str, path: &str) -> String {
	if host.eq_ignore_ascii_case(GITHUB_HOST) {
		format!("https://api.github.com{path}")
	} else {
		format!("https://{host}/api/v3{path}")
	}
}

fn valid_github_host(host: &str) -> bool {
	!host.is_empty()
		&& host.len() <= 255
		&& host
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_repo_component(component: &str) -> bool {
	!component.is_empty()
		&& component.len() <= 255
		&& component
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Provider credential authority consumed by environment-owned GitHub
/// resources.
pub trait CredentialAuthority: Send + Sync + 'static {
	/// Issues one sealed provider lease.
	fn provider_lease(
		&self,
		need: CredentialNeed,
	) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>>;
}

/// How long a host with no ambient GitHub credential is remembered as a miss
/// before the environment and credential helpers are consulted again.
const AMBIENT_MISS_TTL: Duration = Duration::from_secs(60);

/// Environment variables consulted for `github.com`, in precedence order
/// (the `gh` CLI convention).
const GITHUB_COM_TOKEN_VARS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];
/// Environment variables consulted for GitHub Enterprise hosts, in
/// precedence order (the `gh` CLI convention).
const GITHUB_ENTERPRISE_TOKEN_VARS: [&str; 2] = ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"];

/// One host's ambient-credential lookup outcome.
#[derive(Clone)]
enum AmbientEntry {
	Found(CredentialLease),
	Missing(Instant),
}

/// Late-bound projection of the one combined daemon credential authority,
/// with a host-scoped ambient fallback for the GitHub REST API.
pub struct GithubCredentialBridge {
	authority: OnceLock<Arc<dyn CredentialAuthority>>,
	ambient:   Mutex<BTreeMap<String, AmbientEntry>>,
}

impl GithubCredentialBridge {
	/// Creates an unbound credential projection.
	pub const fn new() -> Self {
		Self { authority: OnceLock::new(), ambient: Mutex::new(BTreeMap::new()) }
	}

	/// Leases the credential for one GitHub host's REST API.
	///
	/// Resolution order, first hit wins:
	/// 1. the stored `github` provider credential, for `github.com` only, so a
	///    github.com token is never projected onto an Enterprise host;
	/// 2. the daemon's environment: `GH_TOKEN`/`GITHUB_TOKEN` for `github.com`,
	///    `GH_ENTERPRISE_TOKEN`/`GITHUB_ENTERPRISE_TOKEN` for any other host;
	/// 3. the user's git credential helpers for `https://<host>`, the same
	///    source push and fetch already authenticate with.
	///
	/// Ambient results are cached per host (misses for [`AMBIENT_MISS_TTL`]);
	/// [`Self::reject_host`] drops a cached token the host refused. `Ok(None)`
	/// means requests to this host go out anonymously.
	pub async fn lease_for_host(
		&self,
		host: &str,
		cwd: &Path,
	) -> Result<Option<CredentialLease>, Fault> {
		let host = host.to_ascii_lowercase();
		if host == GITHUB_HOST
			&& let Some(lease) = self.lease().await?
		{
			return Ok(Some(lease));
		}
		if let Some(entry) = self.cached(&host) {
			return Ok(match entry {
				AmbientEntry::Found(lease) => Some(lease),
				AmbientEntry::Missing(_) => None,
			});
		}
		let lease = match ambient_token(&host, |name| std::env::var(name).ok()) {
			Some((variable, token)) => Some(ambient_lease(&host, variable, token)),
			None => omp_vcs::git::credential_fill(cwd, &host, None)
				.await
				.ok()
				.flatten()
				.map(|filled| ambient_lease(&host, "git-credential", filled.password)),
		};
		let entry = lease
			.clone()
			.map_or_else(|| AmbientEntry::Missing(Instant::now()), AmbientEntry::Found);
		self.ambient_map().insert(host, entry);
		Ok(lease)
	}

	/// Forgets the cached ambient credential for `host` after the host rejected
	/// it (HTTP 401), so the next request re-reads the environment and helpers.
	/// The user's credential helpers are never told to erase anything.
	pub fn reject_host(&self, host: &str) {
		self.ambient_map().remove(&host.to_ascii_lowercase());
	}

	fn cached(&self, host: &str) -> Option<AmbientEntry> {
		let mut map = self.ambient_map();
		match map.get(host)? {
			AmbientEntry::Missing(at) if at.elapsed() >= AMBIENT_MISS_TTL => {
				map.remove(host);
				None
			},
			entry => Some(entry.clone()),
		}
	}

	fn ambient_map(&self) -> MutexGuard<'_, BTreeMap<String, AmbientEntry>> {
		self.ambient.lock()
	}

	/// Installs the sole combined credential authority for this environment.
	pub fn bind(
		&self,
		authority: Arc<dyn CredentialAuthority>,
	) -> Result<(), Arc<dyn CredentialAuthority>> {
		self.authority.set(authority)
	}

	/// Leases the default GitHub provider credential when one is available.
	pub async fn lease(&self) -> Result<Option<CredentialLease>, Fault> {
		self.lease_for("github").await
	}

	/// Leases one named provider credential when one is available.
	pub async fn lease_for(&self, spec: &str) -> Result<Option<CredentialLease>, Fault> {
		self.lease_for_account(spec, None).await
	}

	/// Leases one named provider credential, optionally pinned to its durable
	/// account row.
	pub async fn lease_for_account(
		&self,
		spec: &str,
		account: Option<u64>,
	) -> Result<Option<CredentialLease>, Fault> {
		let Some(authority) = self.authority.get() else {
			return Ok(None);
		};
		match authority
			.provider_lease(CredentialNeed {
				spec:        AuthSpecId::from(Str::new(spec)),
				account:     Some(account.map_or_else(
					|| AccountId::from(sf!("{spec}:environment")),
					|account| AccountId::from(account.to_string()),
				)),
				principal:   Some(PrincipalId::from("environment")),
				valid_after: SystemTime::now(),
			})
			.await
		{
			Ok(lease) => Ok(Some(lease)),
			Err(CredentialError::Unavailable) => Ok(None),
			Err(_) => Err(Fault::Source { message: sf!("credential lease failed.") }),
		}
	}
}

impl fmt::Debug for GithubCredentialBridge {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("GithubCredentialBridge")
			.field("bound", &self.authority.get().is_some())
			.finish()
	}
}

/// Picks the host-appropriate token from an environment lookup.
///
/// github.com and Enterprise hosts read disjoint variables, so a token minted
/// for one is never sent to the other.
fn ambient_token(
	host: &str,
	env: impl Fn(&str) -> Option<String>,
) -> Option<(&'static str, SecretString)> {
	let names: &[&'static str] = if host.eq_ignore_ascii_case(GITHUB_HOST) {
		&GITHUB_COM_TOKEN_VARS
	} else {
		&GITHUB_ENTERPRISE_TOKEN_VARS
	};
	names.iter().find_map(|name| {
		env(name)
			.map(|value| value.trim().to_owned())
			.filter(|value| !value.is_empty())
			.map(|value| (*name, SecretString::from(value)))
	})
}

/// Wraps an ambient token as a bearer lease attributed to its source.
fn ambient_lease(host: &str, source: &str, token: SecretString) -> CredentialLease {
	CredentialLease::bearer(
		LeaseMeta {
			account:    AccountId::from(sf!("github:{host}")),
			principal:  PrincipalId::from(source),
			generation: 0,
			expires_at: None,
		},
		token,
	)
}

#[cfg(feature = "cap-forge")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GithubScheme {
	Issue,
	PullRequest,
}

#[cfg(feature = "cap-forge")]
pub(super) struct GithubResolver<H> {
	scheme:      GithubScheme,
	root:        PathBuf,
	cache:       Arc<ForgeCache>,
	credentials: Arc<GithubCredentialBridge>,
	client:      omp_http::Client,
	forge:       H,
}

#[cfg(feature = "cap-forge")]
impl<H> GithubResolver<H> {
	pub(super) fn new(
		scheme: GithubScheme,
		root: PathBuf,
		cache: Arc<ForgeCache>,
		credentials: Arc<GithubCredentialBridge>,
		forge: H,
	) -> Self {
		Self { scheme, root, cache, credentials, client: omp_http::no_redirect_client(), forge }
	}

	/// Names the GitHub host a read of `resource` reaches, as a read resolves
	/// it: the host the resource names or, without one, the workspace's git
	/// remote, in ASCII lowercase. `None` when neither names one, which fails
	/// the read the same way.
	pub(crate) fn fetch_host(&self, resource: &str, query: Option<&str>) -> Option<Str> {
		match numbered_ref(self.scheme, resource, query, &self.root) {
			Ok(Some(parsed)) => parsed
				.host
				.as_deref()
				.map(|host| Str::new(host.to_ascii_lowercase())),
			Ok(None) => {
				let target = Target::parse(self.scheme, resource, query, &self.root).ok()?;
				Some(Str::new(target.repo.host().to_ascii_lowercase()))
			},
			Err(_) => None,
		}
	}

	#[tracing::instrument(
		name = "github_resource_resolve",
		level = "debug",
		skip_all,
		fields(scheme = ?self.scheme, repo = tracing::field::Empty, number = tracing::field::Empty),
	)]
	async fn resolve(&self, resource: &str, query: Option<&str>) -> Result<GithubRead, Fault>
	where
		H: ForgeHost,
	{
		if let Some(parsed) = numbered_ref(self.scheme, resource, query, &self.root)? {
			tracing::Span::current()
				.record("repo", tracing::field::display(format!("{}/{}", parsed.owner, parsed.name)));
			tracing::Span::current().record("number", parsed.number);
			return self.resolve_numbered(parsed).await;
		}
		let target = Target::parse(self.scheme, resource, query, &self.root)?;
		tracing::Span::current().record("repo", target.repo.identity());
		if let Some(number) = target.number {
			tracing::Span::current().record("number", number);
		}
		if matches!(target.view, View::List { .. }) {
			return self.resolve_list(target).await;
		}
		let key = target.cache_key()?;
		let now = now_ms();
		let cached = self.cache.get(&key, now).map_err(cache_fault)?;
		if cached
			.as_ref()
			.is_some_and(|entry| entry.status == ForgeCacheStatus::Fresh)
		{
			return Ok(GithubRead { data: cached.expect("fresh entry").body.to_vec(), stale: false });
		}
		match self
			.fetch(&target, cached.as_ref().and_then(|entry| entry.etag.as_deref()))
			.await
		{
			Ok(Fetch::NotModified) => {
				self.cache.touch(&key, now).map_err(cache_fault)?;
				Ok(GithubRead {
					data:  cached.expect("304 requires cached entity").body.to_vec(),
					stale: false,
				})
			},
			Ok(Fetch::Body { body, etag }) => {
				let comments = if target.comments_enabled() {
					match self.fetch_comments(&target).await {
						Ok(comments) => Some(comments),
						Err(error) => {
							tracing::warn!(
								error = ?error,
								cached = cached.is_some(),
								"GitHub comments refresh failed",
							);
							if let Some(cached) = &cached {
								return Ok(GithubRead { data: cached.body.to_vec(), stale: true });
							}
							return Err(error);
						},
					}
				} else {
					None
				};
				let rendered = target.render(&body, comments.as_deref())?;
				self
					.cache
					.put(&key, &rendered, etag.as_deref(), now)
					.map_err(cache_fault)?;
				Ok(GithubRead { data: rendered, stale: false })
			},
			Err(error) => {
				tracing::warn!(
					error = ?error,
					cached = cached.is_some(),
					"GitHub resource refresh failed",
				);
				if let Some(cached) = cached {
					Ok(GithubRead { data: cached.body.to_vec(), stale: true })
				} else {
					Err(error)
				}
			},
		}
	}

	/// List URLs via [`ForgeHost::search`]. No etag — cache the rendered
	/// markdown only (same contract as numbered forge reads).
	async fn resolve_list(&self, target: Target) -> Result<GithubRead, Fault>
	where
		H: ForgeHost,
	{
		let key = target.cache_key()?;
		let now = now_ms();
		let cached = self.cache.get(&key, now).map_err(cache_fault)?;
		if cached
			.as_ref()
			.is_some_and(|entry| entry.status == ForgeCacheStatus::Fresh)
		{
			return Ok(GithubRead { data: cached.expect("fresh entry").body.to_vec(), stale: false });
		}
		match self.fetch_list(&target).await {
			Ok(rendered) => {
				self
					.cache
					.put(&key, &rendered, None, now)
					.map_err(cache_fault)?;
				Ok(GithubRead { data: rendered, stale: false })
			},
			Err(error) => {
				tracing::warn!(
					error = ?error,
					cached = cached.is_some(),
					"forge list refresh failed",
				);
				if let Some(cached) = cached {
					Ok(GithubRead { data: cached.body.to_vec(), stale: true })
				} else {
					Err(error)
				}
			},
		}
	}

	async fn fetch_list(&self, target: &Target) -> Result<Vec<u8>, Fault>
	where
		H: ForgeHost,
	{
		let query = list_search_query(target)?;
		let page = self.forge.search(query).await.map_err(forge_fault)?;
		if page.hits.len() > MAX_BODY {
			return Err(invalid("GitHub response exceeds 8 MiB."));
		}
		Ok(render_search_list(target.scheme, &target.repo, &page))
	}

	#[tracing::instrument(
		name = "github_resource_fetch",
		level = "debug",
		skip_all,
		fields(scheme = ?target.scheme, repo = %target.repo.identity(), number = ?target.number),
	)]
	async fn fetch(&self, target: &Target, etag: Option<&str>) -> Result<Fetch, Fault> {
		let mut headers = HeaderMap::new();
		headers.insert(USER_AGENT, HeaderValue::from_static("omp/issue-pr-resolver"));
		headers.insert("x-github-api-version", HeaderValue::from_static("2022-11-28"));
		headers.insert(ACCEPT, HeaderValue::from_static(target.accept()));
		if let Some(lease) = self
			.credentials
			.lease_for_host(target.repo.host(), &self.root)
			.await?
		{
			lease
				.apply_header(&HeaderPlacement::bearer(), &mut headers)
				.map_err(|_| Fault::Source { message: sf!("GitHub credential projection failed.") })?;
		}
		if let Some(etag) = etag {
			headers.insert(
				IF_NONE_MATCH,
				HeaderValue::from_str(etag).map_err(|_| invalid("Invalid cached GitHub ETag."))?,
			);
		}
		let response = self
			.client
			.get(target.api_url())
			.headers(headers)
			.send()
			.await
			.map_err(http_fault)?;
		if response.status().as_u16() == 304 {
			return Ok(Fetch::NotModified);
		}
		let status = response.status().as_u16();
		let etag = response
			.headers()
			.get(ETAG)
			.and_then(|value| value.to_str().ok())
			.map(Str::new);
		let mut stream = response.bytes_stream();
		let mut bytes = BytesMut::new();
		while let Some(chunk) = stream.next().await {
			let chunk = chunk.map_err(http_fault)?;
			if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
				return Err(invalid("GitHub response exceeds 8 MiB."));
			}
			bytes.extend_from_slice(&chunk);
		}
		if !(200..300).contains(&status) {
			if status == 401 {
				self.credentials.reject_host(target.repo.host());
			}
			return Err(Fault::Source {
				message: Str::new(format!("GitHub API returned HTTP {status}.")),
			});
		}
		Ok(Fetch::Body { body: bytes.freeze().to_vec(), etag })
	}

	#[tracing::instrument(
		name = "github_comments_fetch",
		level = "debug",
		skip_all,
		fields(scheme = ?target.scheme, repo = %target.repo.identity(), number = ?target.number),
	)]
	async fn fetch_comments(&self, target: &Target) -> Result<Vec<Value>, Fault> {
		let mut comments = Vec::new();
		let mut retained_bytes = 0usize;
		let lease = self
			.credentials
			.lease_for_host(target.repo.host(), &self.root)
			.await?;
		for page in 1..=100u32 {
			let mut headers = HeaderMap::new();
			headers.insert(USER_AGENT, HeaderValue::from_static("omp/issue-pr-resolver"));
			headers.insert("x-github-api-version", HeaderValue::from_static("2022-11-28"));
			headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github+json"));
			if let Some(lease) = &lease {
				lease
					.apply_header(&HeaderPlacement::bearer(), &mut headers)
					.map_err(|_| Fault::Source {
						message: sf!("GitHub credential projection failed."),
					})?;
			}
			let response = self
				.client
				.get(target.comments_url(page))
				.headers(headers)
				.send()
				.await
				.map_err(http_fault)?;
			let status = response.status().as_u16();
			if !(200..300).contains(&status) {
				return Err(Fault::Source {
					message: Str::new(format!("GitHub comments API returned HTTP {status}.")),
				});
			}
			let mut stream = response.bytes_stream();
			let mut bytes = BytesMut::new();
			while let Some(chunk) = stream.next().await {
				let chunk = chunk.map_err(http_fault)?;
				retained_bytes = retained_bytes.saturating_add(chunk.len());
				if retained_bytes > MAX_BODY {
					return Err(invalid("GitHub comments exceed 8 MiB."));
				}
				bytes.extend_from_slice(&chunk);
			}
			let page_comments: Vec<Value> = serde_json::from_slice(&bytes)
				.map_err(|error| invalid_json("GitHub comments", error))?;
			let complete = page_comments.len() < 100;
			comments.extend(page_comments);
			if complete {
				return Ok(comments);
			}
		}
		Err(invalid("GitHub comments exceed 100 pages."))
	}

	async fn resolve_numbered(&self, parsed: SchemeRef) -> Result<GithubRead, Fault>
	where
		H: ForgeHost,
	{
		let cached_key = target_for_scheme(&parsed)
			.ok()
			.and_then(|target| target.cache_key().ok());
		let now = now_ms();
		let cached = match &cached_key {
			Some(key) => self.cache.get(key, now).map_err(cache_fault)?,
			None => None,
		};
		if cached
			.as_ref()
			.is_some_and(|entry| entry.status == ForgeCacheStatus::Fresh)
		{
			return Ok(GithubRead { data: cached.expect("fresh entry").body.to_vec(), stale: false });
		}
		match self.fetch_numbered(&parsed).await {
			Ok(rendered) => {
				if let Some(key) = &cached_key {
					self
						.cache
						.put(key, &rendered, None, now)
						.map_err(cache_fault)?;
				}
				Ok(GithubRead { data: rendered, stale: false })
			},
			Err(error) => {
				tracing::warn!(
					error = ?error,
					cached = cached.is_some(),
					"forge resource refresh failed",
				);
				if let Some(cached) = cached {
					Ok(GithubRead { data: cached.body.to_vec(), stale: true })
				} else {
					Err(error)
				}
			},
		}
	}

	async fn fetch_numbered(&self, parsed: &SchemeRef) -> Result<Vec<u8>, Fault>
	where
		H: ForgeHost,
	{
		let repo = parsed
			.repo()
			.ok_or_else(|| invalid("Forge resource is missing a host."))?;
		let comments_enabled = parsed.comments.unwrap_or(true);
		match parsed.selector {
			ResourceSelector::Diff(selector) => {
				let text = self
					.forge
					.change_request_diff(&repo, parsed.number)
					.await
					.map_err(forge_fault)?;
				if text.len() > MAX_BODY {
					return Err(invalid("GitHub response exceeds 8 MiB."));
				}
				render_diff(text.as_str(), &diff_mode(selector), &diff_repo_label(&repo), parsed.number)
			},
			ResourceSelector::None => {
				let (title, body) = match parsed.kind {
					ResourceKind::ChangeRequest => {
						let value = self
							.forge
							.view_change_request(&repo, parsed.number)
							.await
							.map_err(forge_fault)?;
						(value.title, value.body)
					},
					ResourceKind::Issue => {
						let value = self
							.forge
							.view_issue(&repo, parsed.number)
							.await
							.map_err(forge_fault)?;
						(value.title, value.body)
					},
				};
				let comments = if comments_enabled {
					Some(
						self
							.forge
							.list_comments(&repo, parsed.number, parsed.kind)
							.await
							.map_err(forge_fault)?,
					)
				} else {
					None
				};
				Ok(render_numbered(
					title.as_str(),
					body.as_str(),
					comments_enabled,
					comments.as_deref(),
				))
			},
		}
	}
}

#[cfg(feature = "cap-forge")]
impl<H: ForgeHost> Resolve for GithubResolver<H> {
	async fn read<'a>(
		&'a self,
		resource: &'a str,
		selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		self
			.read_with_diags(resource, selector)
			.await
			.map(|resolved| resolved.data)
	}

	async fn read_with_diags<'a>(
		&'a self,
		resource: &'a str,
		selector: &'a ParsedSelector,
	) -> Result<ResolvedRead, Fault> {
		let resolved = self.resolve(resource, None).await?;
		selected_read(resolved, resource, selector)
	}

	async fn read_query<'a>(
		&'a self,
		resource: &'a str,
		query: Option<&'a str>,
		selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		self
			.read_query_with_diags(resource, query, selector)
			.await
			.map(|resolved| resolved.data)
	}

	async fn read_query_with_diags<'a>(
		&'a self,
		resource: &'a str,
		query: Option<&'a str>,
		selector: &'a ParsedSelector,
	) -> Result<ResolvedRead, Fault> {
		let resolved = self.resolve(resource, query).await?;
		selected_read(resolved, resource, selector)
	}
}

#[cfg(feature = "cap-forge")]
fn selected_read(
	resolved: GithubRead,
	resource: &str,
	selector: &ParsedSelector,
) -> Result<ResolvedRead, Fault> {
	let data = tool_url::select_bytes(
		&Default::default(),
		resource,
		CowBytes::from(resolved.data),
		selector,
	)?;
	let diags = if resolved.stale {
		smallvec![Diag::warn(
			DiagKind::StaleCache,
			"Live GitHub refresh failed; cached content may be stale.",
		)]
	} else {
		smallvec![]
	};
	Ok(ResolvedRead { data, diags })
}

#[cfg(feature = "cap-forge")]
impl<H> fmt::Debug for GithubResolver<H> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("GithubResolver(..)")
	}
}

#[cfg(feature = "cap-forge")]
struct GithubRead {
	data:  Vec<u8>,
	stale: bool,
}

#[cfg(feature = "cap-forge")]
enum Fetch {
	NotModified,
	Body { body: Vec<u8>, etag: Option<Str> },
}

#[cfg(feature = "cap-forge")]
#[derive(Clone, Debug)]
enum View {
	List { state: Str, limit: u32, author: Option<Str>, label: Option<Str> },
	Detail { comments: bool },
	Diff { mode: DiffMode },
}
#[cfg(feature = "cap-forge")]
#[derive(Clone, Debug)]
enum DiffMode {
	Index,
	All,
	Slice(usize),
}
#[cfg(feature = "cap-forge")]
#[derive(Clone, Debug)]
struct Target {
	scheme: GithubScheme,
	repo:   GithubRepo,
	number: Option<u64>,
	view:   View,
}

#[cfg(feature = "cap-forge")]
impl Target {
	fn parse(
		scheme: GithubScheme,
		resource: &str,
		query: Option<&str>,
		root: &Path,
	) -> Result<Self, Fault> {
		let parts = resource
			.split('/')
			.filter(|part| !part.is_empty())
			.collect::<Vec<_>>();
		if parts.iter().any(|part| matches!(*part, "." | "..")) {
			return Err(invalid("Unsafe GitHub resource path."));
		}
		let (repo, number, trailing) = match parts.as_slice() {
			[] => (infer_repo(root)?, None, &[][..]),
			[number] if number.bytes().all(|b| b.is_ascii_digit()) => {
				(infer_repo(root)?, Some(parse_number(number)?), &[][..])
			},
			[host, owner, repo] if host.contains('.') => {
				(GithubRepo::new(host, owner, repo)?, None, &[][..])
			},
			[host, owner, repo, number, tail @ ..]
				if number.bytes().all(|byte| byte.is_ascii_digit()) =>
			{
				(GithubRepo::new(host, owner, repo)?, Some(parse_number(number)?), tail)
			},
			[owner, repo] => (GithubRepo::new(GITHUB_HOST, owner, repo)?, None, &[][..]),
			[owner, repo, number, tail @ ..] => {
				(GithubRepo::new(GITHUB_HOST, owner, repo)?, Some(parse_number(number)?), tail)
			},
			_ => {
				return Err(invalid(
					"Expected issue://<n>, issue://[host/]owner/repo/<n>, or a repo list.",
				));
			},
		};
		let params = query
			.map(|query| {
				url::form_urlencoded::parse(query.as_bytes())
					.into_owned()
					.collect::<Vec<_>>()
			})
			.unwrap_or_default();
		let get = |name: &str| {
			params
				.iter()
				.find(|(key, _)| key == name)
				.map(|(_, value)| value.as_str())
		};
		let view = if !trailing.is_empty() {
			if scheme != GithubScheme::PullRequest
				|| trailing.first() != Some(&"diff")
				|| trailing.len() > 2
			{
				return Err(invalid("Only pr:// resources support /diff[/all|<index>]."));
			}
			let mode = match trailing.get(1).copied() {
				None => DiffMode::Index,
				Some("all") => DiffMode::All,
				Some(value) => DiffMode::Slice(
					value
						.parse()
						.map_err(|_| invalid("Diff index must be positive."))?,
				),
			};
			View::Diff { mode }
		} else if number.is_some() {
			View::Detail { comments: !matches!(get("comments"), Some("0" | "false")) }
		} else {
			let state = Str::new(get("state").unwrap_or("open"));
			if !matches!(state.as_str(), "open" | "closed" | "merged" | "all") {
				return Err(invalid("Invalid GitHub list state."));
			}
			let limit = get("limit")
				.map_or(Ok(30), str::parse::<u32>)
				.map_err(|_| invalid("Invalid GitHub list limit."))?
				.clamp(1, 100);
			View::List {
				state,
				limit,
				author: get("author").map(Str::new),
				label: get("label").map(Str::new),
			}
		};
		Ok(Self { scheme, repo, number, view })
	}

	fn cache_key(&self) -> Result<ForgeCacheKey, Fault> {
		ForgeCacheKey::new(
			match &self.view {
				View::Diff { .. } => ForgeResourceKind::Diff,
				_ if self.scheme == GithubScheme::Issue => ForgeResourceKind::Issue,
				_ => ForgeResourceKind::PullRequest,
			},
			self.repo.identity(),
			self.number,
			self.view_key(),
		)
		.map_err(cache_fault)
	}

	fn view_key(&self) -> Str {
		Str::new(format!("{:?}", self.view))
	}

	fn accept(&self) -> &'static str {
		if matches!(self.view, View::Diff { .. }) {
			"application/vnd.github.v3.diff"
		} else {
			"application/vnd.github+json"
		}
	}

	fn api_url(&self) -> String {
		let base = self.repo.api_url(&format!("/repos/{}", self.repo.slug()));
		match &self.view {
			View::List { state, limit, author, label } => {
				let family = if self.scheme == GithubScheme::Issue {
					"issues"
				} else {
					"pulls"
				};
				let mut url = format!(
					"{base}/{family}?state={}&per_page={limit}",
					if state == "merged" {
						"closed"
					} else {
						state.as_str()
					}
				);
				if let Some(author) = author {
					url.push_str("&creator=");
					url.push_str(author);
				}
				if let Some(label) = label {
					url.push_str("&labels=");
					url.push_str(label);
				}
				url
			},
			View::Detail { .. } => format!(
				"{base}/{}/{}",
				if self.scheme == GithubScheme::Issue {
					"issues"
				} else {
					"pulls"
				},
				self.number.expect("detail number")
			),
			View::Diff { .. } => format!("{base}/pulls/{}", self.number.expect("diff number")),
		}
	}

	fn comments_enabled(&self) -> bool {
		matches!(self.view, View::Detail { comments: true })
	}

	fn comments_url(&self, page: u32) -> String {
		self.repo.api_url(&format!(
			"/repos/{}/issues/{}/comments?per_page=100&page={page}",
			self.repo.slug(),
			self.number.expect("comments require a detail number"),
		))
	}

	fn render(&self, body: &[u8], comments: Option<&[Value]>) -> Result<Vec<u8>, Fault> {
		if let View::Diff { mode } = &self.view {
			let text = str::from_utf8(body).map_err(|_| invalid("GitHub diff is not UTF-8."))?;
			return render_diff(text, mode, self.repo.identity(), self.number.expect("diff number"));
		}
		let value: Value =
			serde_json::from_slice(body).map_err(|error| invalid_json("GitHub", error))?;
		let mut out = String::new();
		match (&self.view, value) {
			(View::List { .. }, Value::Array(items)) => {
				out.push_str(if self.scheme == GithubScheme::Issue {
					"# Issues\n\n"
				} else {
					"# Pull Requests\n\n"
				});
				for item in items {
					if self.scheme == GithubScheme::Issue && item.get("pull_request").is_some() {
						continue;
					}
					render_item(&mut out, &item, self.repo.identity(), self.scheme);
				}
			},
			(View::Detail { comments: include_comments }, item) => {
				out.push_str("# ");
				out.push_str(
					item
						.get("title")
						.and_then(Value::as_str)
						.unwrap_or("(no title)"),
				);
				out.push_str("\n\n");
				out.push_str(item.get("body").and_then(Value::as_str).unwrap_or(""));
				if !include_comments {
					out.push_str("\n\n_Comments disabled._");
				} else if let Some(comments) = comments {
					render_comments(&mut out, comments);
				}
			},
			_ => return Err(invalid("Unexpected GitHub response shape.")),
		}
		Ok(out.into_bytes())
	}
}

#[cfg(feature = "cap-forge")]
fn render_numbered(
	title: &str,
	body: &str,
	comments_enabled: bool,
	comments: Option<&[Comment]>,
) -> Vec<u8> {
	let mut out = String::new();
	out.push_str("# ");
	out.push_str(if title.is_empty() {
		"(no title)"
	} else {
		title
	});
	out.push_str("\n\n");
	out.push_str(body);
	if !comments_enabled {
		out.push_str("\n\n_Comments disabled._");
	} else if let Some(comments) = comments {
		render_comment_page(&mut out, comments);
	}
	out.into_bytes()
}

#[cfg(feature = "cap-forge")]
fn render_comment_page(out: &mut String, comments: &[Comment]) {
	out.push_str("\n\n## Comments\n");
	if comments.is_empty() {
		out.push_str("\n_No comments._\n");
		return;
	}
	for comment in comments {
		let author = if comment.author.is_empty() {
			"unknown"
		} else {
			comment.author.as_str()
		};
		let created = if comment.created_at.is_empty() {
			"unknown time"
		} else {
			comment.created_at.as_str()
		};
		out.push_str("\n### @");
		out.push_str(author);
		out.push_str(" — ");
		out.push_str(created);
		out.push_str("\n\n");
		out.push_str(comment.body.as_str());
		out.push('\n');
	}
}

#[cfg(feature = "cap-forge")]
fn render_comments(out: &mut String, comments: &[Value]) {
	out.push_str("\n\n## Comments\n");
	if comments.is_empty() {
		out.push_str("\n_No comments._\n");
		return;
	}
	for comment in comments {
		let author = comment
			.get("user")
			.and_then(|user| user.get("login"))
			.and_then(Value::as_str)
			.unwrap_or("unknown");
		let created = comment
			.get("created_at")
			.and_then(Value::as_str)
			.unwrap_or("unknown time");
		out.push_str("\n### @");
		out.push_str(author);
		out.push_str(" — ");
		out.push_str(created);
		out.push_str("\n\n");
		out.push_str(comment.get("body").and_then(Value::as_str).unwrap_or(""));
		out.push('\n');
	}
}

#[cfg(feature = "cap-forge")]
fn render_item(out: &mut String, item: &Value, repo: &str, scheme: GithubScheme) {
	let number = item
		.get("number")
		.and_then(Value::as_u64)
		.unwrap_or_default();
	let state = item.get("state").and_then(Value::as_str).unwrap_or("?");
	let title = item
		.get("title")
		.and_then(Value::as_str)
		.unwrap_or("(no title)");
	out.push_str(&format!(
		"- [{state}] #{number} {title}\n  {}://{repo}/{number}\n",
		if scheme == GithubScheme::Issue {
			"issue"
		} else {
			"pr"
		}
	));
}
#[cfg(feature = "cap-forge")]
fn render_diff(text: &str, mode: &DiffMode, repo: &str, number: u64) -> Result<Vec<u8>, Fault> {
	if matches!(mode, DiffMode::All) {
		return Ok(text.as_bytes().to_vec());
	}
	let starts = text
		.match_indices("diff --git ")
		.map(|(index, _)| index)
		.collect::<Vec<_>>();
	if let DiffMode::Slice(index) = mode {
		if *index == 0 || *index > starts.len() {
			return Err(invalid("Diff file index is out of range."));
		}
		let start = starts[index - 1];
		let end = starts.get(*index).copied().unwrap_or(text.len());
		return Ok(text.as_bytes()[start..end].to_vec());
	}
	let mut out = format!("# Pull Request Diff: {repo}#{number}\n\n");
	for (index, start) in starts.iter().enumerate() {
		let line = text[*start..].lines().next().unwrap_or("diff --git");
		out.push_str(&format!(
			"{}. {}\n   pr://{repo}/{number}/diff/{}\n",
			index + 1,
			line.trim_start_matches("diff --git "),
			index + 1
		));
	}
	Ok(out.into_bytes())
}
#[cfg(feature = "cap-forge")]
fn numbered_ref(
	scheme: GithubScheme,
	resource: &str,
	query: Option<&str>,
	root: &Path,
) -> Result<Option<SchemeRef>, Fault> {
	if is_list_resource(resource) {
		return Ok(None);
	}
	let expanded = expand_shorthand(scheme, resource, root)?;
	let url = append_query(expanded, query);
	match parse_resource_url(&url) {
		Ok(mut parsed) => {
			if parsed.host.is_none() {
				parsed.host = Some(Str::new_static(GITHUB_HOST));
			}
			Ok(Some(parsed))
		},
		Err(SchemeError::MissingNumber) => Ok(None),
		Err(error) => Err(Fault::Invalid { message: Str::new(error.to_string()) }),
	}
}

#[cfg(feature = "cap-forge")]
fn is_list_resource(resource: &str) -> bool {
	let parts = resource
		.split('/')
		.filter(|part| !part.is_empty())
		.collect::<Vec<_>>();
	match parts.as_slice() {
		[] => true,
		[owner, name] => !(all_digits(owner) && *name == "diff"),
		[host, _, name] if host.contains('.') && !all_digits(name) => true,
		_ => false,
	}
}

#[cfg(feature = "cap-forge")]
fn expand_shorthand(scheme: GithubScheme, resource: &str, root: &Path) -> Result<String, Fault> {
	let name = scheme_name(scheme);
	if let Some(tail) = shorthand_tail(resource) {
		let repo = infer_repo(root)?;
		return Ok(format!("{name}://{}/{}/{}", repo.host(), repo.slug(), tail));
	}
	Ok(format!("{name}://{resource}"))
}

#[cfg(feature = "cap-forge")]
fn shorthand_tail(resource: &str) -> Option<String> {
	let parts = resource
		.split('/')
		.filter(|part| !part.is_empty())
		.collect::<Vec<_>>();
	match parts.as_slice() {
		[number] if all_digits(number) => Some((*number).to_owned()),
		[number, "diff"] if all_digits(number) => Some(format!("{number}/diff")),
		[number, "diff", index] if all_digits(number) => Some(format!("{number}/diff/{index}")),
		_ => None,
	}
}

#[cfg(feature = "cap-forge")]
fn append_query(url: String, query: Option<&str>) -> String {
	match query.map(str::trim).filter(|query| !query.is_empty()) {
		Some(query) => format!("{url}?{query}"),
		None => url,
	}
}

#[cfg(feature = "cap-forge")]
const fn scheme_name(scheme: GithubScheme) -> &'static str {
	match scheme {
		GithubScheme::Issue => "issue",
		GithubScheme::PullRequest => "pr",
	}
}

#[cfg(feature = "cap-forge")]
fn all_digits(text: &str) -> bool {
	!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(feature = "cap-forge")]
fn target_for_scheme(parsed: &SchemeRef) -> Result<Target, Fault> {
	let host = parsed
		.host
		.as_deref()
		.ok_or_else(|| invalid("Forge resource is missing a host."))?;
	let repo = GithubRepo::new(host, parsed.owner.as_str(), parsed.name.as_str())?;
	let scheme = match parsed.kind {
		ResourceKind::ChangeRequest => GithubScheme::PullRequest,
		ResourceKind::Issue => GithubScheme::Issue,
	};
	let view = match parsed.selector {
		ResourceSelector::None => View::Detail { comments: parsed.comments.unwrap_or(true) },
		ResourceSelector::Diff(selector) => View::Diff { mode: diff_mode(selector) },
	};
	Ok(Target { scheme, repo, number: Some(parsed.number), view })
}

#[cfg(feature = "cap-forge")]
fn diff_mode(selector: DiffSelector) -> DiffMode {
	match selector {
		DiffSelector::Index => DiffMode::Index,
		DiffSelector::All => DiffMode::All,
		DiffSelector::Slice(index) => DiffMode::Slice(usize::try_from(index).unwrap_or(usize::MAX)),
	}
}

#[cfg(feature = "cap-forge")]
fn diff_repo_label(repo: &RepoRef) -> String {
	if repo.host.eq_ignore_ascii_case(GITHUB_HOST) {
		format!("{}/{}", repo.owner, repo.name)
	} else {
		format!("{}/{}/{}", repo.host, repo.owner, repo.name)
	}
}

#[cfg(feature = "cap-forge")]
fn forge_fault(error: ForgeError) -> Fault {
	Fault::Source { message: Str::new(error.to_string()) }
}

/// Builds a [`SearchQuery`] for a list URL, encoding filters as neutral
/// `state:` / `author:` / `label:` tokens in `text`.
#[cfg(feature = "cap-forge")]
fn list_search_query(target: &Target) -> Result<SearchQuery, Fault> {
	let View::List { state, limit, author, label } = &target.view else {
		return Err(invalid("Expected a forge list view."));
	};
	let repo = repo_ref_from_github(&target.repo)?;
	let mut text = format!("state:{state}");
	if let Some(author) = author {
		text.push_str(" author:");
		text.push_str(author.as_str());
	}
	if let Some(label) = label {
		text.push_str(" label:");
		text.push_str(label.as_str());
	}
	Ok(SearchQuery {
		kind: match target.scheme {
			GithubScheme::Issue => SearchKind::Issues,
			GithubScheme::PullRequest => SearchKind::ChangeRequests,
		},
		repo: Some(repo),
		text: Str::new(text),
		limit: *limit,
	})
}

#[cfg(feature = "cap-forge")]
fn repo_ref_from_github(repo: &GithubRepo) -> Result<RepoRef, Fault> {
	let (owner, name) = repo
		.slug()
		.split_once('/')
		.ok_or_else(|| invalid("GitHub repository must be [host/]owner/repo."))?;
	RepoRef::try_new(repo.host().to_owned(), owner, name)
		.map_err(|_| invalid("GitHub repository must be [host/]owner/repo."))
}

/// Renders a [`SearchPage`] into the historical list-URL markdown contract:
/// `# Issues` / `# Pull Requests` plus `- [state] #N title` / scheme link lines.
#[cfg(feature = "cap-forge")]
fn render_search_list(scheme: GithubScheme, repo: &GithubRepo, page: &SearchPage) -> Vec<u8> {
	let mut out = String::new();
	out.push_str(if scheme == GithubScheme::Issue {
		"# Issues\n\n"
	} else {
		"# Pull Requests\n\n"
	});
	for hit in page.as_slice() {
		render_search_hit(&mut out, hit, repo.identity(), scheme);
	}
	out.into_bytes()
}

#[cfg(feature = "cap-forge")]
fn render_search_hit(out: &mut String, hit: &SearchHit, repo: &str, scheme: GithubScheme) {
	let number = hit.number.unwrap_or_default();
	let state = if hit.state.is_empty() {
		"?"
	} else {
		hit.state.as_str()
	};
	let title = if hit.title.is_empty() {
		"(no title)"
	} else {
		hit.title.as_str()
	};
	out.push_str(&format!(
		"- [{state}] #{number} {title}\n  {}://{repo}/{number}\n",
		if scheme == GithubScheme::Issue {
			"issue"
		} else {
			"pr"
		}
	));
}

pub(super) fn infer_repo(root: &Path) -> Result<GithubRepo, Fault> {
	let repo = GitRepo::require(root)
		.map_err(|_| invalid("Cannot infer GitHub repo; use [host/]owner/repo explicitly."))?;
	let config = std::fs::read_to_string(repo.info().common_dir.join("config"))
		.map_err(|_| invalid("Git repository config is unavailable."))?;
	let mut in_origin = false;
	let mut first_remote = None;
	let mut origin = None;
	for line in config.lines().map(str::trim) {
		if line.starts_with('[') {
			in_origin = line.eq_ignore_ascii_case("[remote \"origin\"]");
		} else if let Some(url) = line.strip_prefix("url = ") {
			if first_remote.is_none() {
				first_remote = Some(url);
			}
			if in_origin {
				origin = Some(url);
				break;
			}
		}
	}
	let remote = origin
		.or(first_remote)
		.ok_or_else(|| invalid("Git origin URL is missing."))?;
	repo_from_remote(remote)
}

pub(super) fn repo_from_remote(remote: &str) -> Result<GithubRepo, Fault> {
	let remote = remote.trim();
	if remote.contains("://") {
		let parsed =
			url::Url::parse(remote).map_err(|_| invalid("Git origin is not a GitHub remote."))?;
		let host = parsed
			.host_str()
			.ok_or_else(|| invalid("Git origin is not a GitHub remote."))?;
		let host = parsed
			.port()
			.map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));
		repo_from_host_path(&host, parsed.path().trim_start_matches('/'))
	} else {
		let (authority, path) = remote
			.split_once(':')
			.ok_or_else(|| invalid("Git origin is not a GitHub remote."))?;
		let host = authority
			.rsplit_once('@')
			.map_or(authority, |(_, host)| host);
		repo_from_host_path(host, path)
	}
}

/// Builds the repo identity from a remote's host and path segments.
fn repo_from_host_path(host: &str, path: &str) -> Result<GithubRepo, Fault> {
	let path = path.trim_end_matches('/').trim_end_matches(".git");
	let mut parts = path.split('/');
	let owner = parts.next().unwrap_or_default();
	let name = parts.next().unwrap_or_default();
	if parts.next().is_some() {
		return Err(invalid("GitHub origin is not owner/repo."));
	}
	GithubRepo::new(host, owner, name)
}
#[cfg(feature = "cap-forge")]
fn parse_number(value: &str) -> Result<u64, Fault> {
	value
		.parse::<u64>()
		.ok()
		.filter(|n| *n > 0)
		.ok_or_else(|| invalid("GitHub number must be positive."))
}
#[cfg(feature = "cap-forge")]
fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
fn invalid(message: &'static str) -> Fault {
	Fault::Invalid { message: Str::new_static(message) }
}
/// Creates an invalid-JSON fault, rendering the cause once at this
/// model-facing fault boundary.
#[cfg(feature = "cap-forge")]
fn invalid_json(what: &'static str, cause: impl Display) -> Fault {
	Fault::Invalid { message: sf!("Invalid {what} JSON: {cause}") }
}
#[cfg(feature = "cap-forge")]
fn cache_fault(error: impl Display) -> Fault {
	Fault::Source { message: Str::new(error.to_string()) }
}
#[cfg(feature = "cap-forge")]
fn http_fault(error: impl Display) -> Fault {
	Fault::Source { message: Str::new(format!("GitHub API request failed: {error}")) }
}
#[cfg(test)]
mod tests {
	#[cfg(feature = "cap-forge")]
	use std::future::Future;
	use std::sync::Arc;

	#[cfg(feature = "cap-forge")]
	use omp_cache::forge_cache::ForgeCachePolicy;
	use omp_core::ExposeSecret as _;
	#[cfg(feature = "cap-forge")]
	use omp_forge::{
		ChangeRequest, ChangeRequestDraft, CheckTarget, CheckoutReceipt, CheckoutRequest, Comment,
		FileBody, FileQuery, ForgeChecks, ForgeError, ForgeHost, ForgeOp, Issue, IssueDraft,
		IssueState, PushReceipt, PushRequest, Repository, RequestState, ResourceKind, SearchHit,
		SearchKind, SearchPage, SearchQuery,
	};
	#[cfg(feature = "cap-forge")]
	use omp_forge_delegated::{Delegated, PermitAll};
	#[cfg(feature = "cap-forge")]
	use parking_lot::Mutex;

	use super::*;

	fn bearer_of(lease: &CredentialLease) -> String {
		let mut headers = HeaderMap::new();
		lease
			.apply_header(&HeaderPlacement::bearer(), &mut headers)
			.expect("bearer header");
		headers["authorization"].to_str().unwrap().to_owned()
	}

	#[test]
	fn ambient_tokens_are_scoped_to_their_host_family() {
		let env = |name: &str| match name {
			"GH_TOKEN" => Some("  ".to_owned()),
			"GITHUB_TOKEN" => Some("dotcom".to_owned()),
			"GITHUB_ENTERPRISE_TOKEN" => Some("ghe".to_owned()),
			_ => None,
		};
		let (variable, token) = ambient_token("github.com", env).expect("github.com token");
		assert_eq!((variable, token.expose_secret()), ("GITHUB_TOKEN", "dotcom"));
		let (variable, token) = ambient_token("ghe.example.com", env).expect("enterprise token");
		assert_eq!((variable, token.expose_secret()), ("GITHUB_ENTERPRISE_TOKEN", "ghe"));
		assert!(
			ambient_token("ghe.example.com", |name| (name == "GH_TOKEN").then(String::new)).is_none()
		);
	}

	/// Authority that would hand every caller the same stored token.
	struct AlwaysStored;

	impl CredentialAuthority for AlwaysStored {
		fn provider_lease(
			&self,
			need: CredentialNeed,
		) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>> {
			omp_ai::auth::credential_ready(Ok(CredentialLease::bearer(
				LeaseMeta {
					account:    need.account.unwrap(),
					principal:  PrincipalId::from("environment"),
					generation: 1,
					expires_at: None,
				},
				SecretString::from("stored-dotcom".to_owned()),
			)))
		}
	}

	#[tokio::test]
	async fn enterprise_hosts_never_receive_the_stored_dotcom_credential() {
		if GITHUB_ENTERPRISE_TOKEN_VARS
			.iter()
			.any(|name| std::env::var_os(name).is_some())
		{
			return;
		}
		let repo = tempfile::tempdir().unwrap();
		let git = |args: &[&str]| {
			assert!(
				std::process::Command::new("git")
					.args(args)
					.current_dir(repo.path())
					.status()
					.unwrap()
					.success()
			);
		};
		git(&["init", "-q"]);
		git(&["config", "--add", "credential.helper", ""]);
		git(&[
			"config",
			"--add",
			"credential.helper",
			"!f() { test \"$1\" = get && echo username=x && echo password=helper-ghe; }; f",
		]);
		let bridge = GithubCredentialBridge::new();
		assert!(bridge.bind(Arc::new(AlwaysStored)).is_ok());

		let dotcom = bridge
			.lease_for_host("GitHub.com", repo.path())
			.await
			.unwrap()
			.expect("stored lease");
		assert_eq!(bearer_of(&dotcom), "Bearer stored-dotcom");

		let ghe = bridge
			.lease_for_host("ghe.example.com", repo.path())
			.await
			.unwrap()
			.expect("helper lease");
		assert_eq!(bearer_of(&ghe), "Bearer helper-ghe");

		bridge.reject_host("GHE.example.com");
		assert!(bridge.cached("ghe.example.com").is_none());
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn stale_cache_is_reported_out_of_band_without_changing_document_bytes() {
		let cached = b"# Cached issue\n\nBody".to_vec();
		let resolved = selected_read(
			GithubRead { data: cached.clone(), stale: true },
			"owner/repo/1",
			&ParsedSelector::None,
		)
		.expect("cached resource");
		assert_eq!(resolved.data.as_ref(), cached.as_slice());
		assert_eq!(resolved.diags.len(), 1);
		assert_eq!(resolved.diags[0].native_kind(), Some(DiagKind::StaleCache));
		assert_eq!(resolved.diags[0].severity, omp_tool::Severity::Warn);
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn target_parses_dotted_and_single_label_enterprise_hosts() {
		let root = Path::new("/unused");
		let dotted =
			Target::parse(GithubScheme::PullRequest, "ghe.example.com/owner/repo/7", None, root)
				.expect("dotted enterprise target");
		assert_eq!(dotted.repo.identity(), "ghe.example.com/owner/repo");
		assert_eq!(dotted.api_url(), "https://ghe.example.com/api/v3/repos/owner/repo/pulls/7",);

		let single = Target::parse(GithubScheme::PullRequest, "ghe/owner/repo/7", None, root)
			.expect("single-label enterprise target");
		assert_eq!(single.repo.identity(), "ghe/owner/repo");
		assert_eq!(single.api_url(), "https://ghe/api/v3/repos/owner/repo/pulls/7");
		let issue = Target::parse(GithubScheme::Issue, "ghe.example.com/owner/repo/8", None, root)
			.expect("enterprise issue target");
		assert_eq!(issue.api_url(), "https://ghe.example.com/api/v3/repos/owner/repo/issues/8",);
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn numeric_tail_disambiguates_host_from_diff_suffix() {
		let root = Path::new("/unused");
		let slice = Target::parse(GithubScheme::PullRequest, "owner/repo/77/diff/1", None, root)
			.expect("default-host diff slice");
		assert_eq!(slice.repo.identity(), "owner/repo");
		assert!(matches!(slice.view, View::Diff { mode: DiffMode::Slice(1) }));

		assert!(
			Target::parse(GithubScheme::PullRequest, "ghe/owner/repo", None, root).is_err(),
			"an unnumbered single-label host is ambiguous and must not be guessed",
		);
	}

	#[test]
	fn remote_urls_preserve_enterprise_hosts_and_normalize_github_dot_com() {
		let enterprise =
			repo_from_remote("git@ghe.example.com:Owner/Repo.git").expect("enterprise SSH remote");
		assert_eq!(enterprise.identity(), "ghe.example.com/Owner/Repo");
		assert_eq!(
			enterprise.api_url("/repos/Owner/Repo"),
			"https://ghe.example.com/api/v3/repos/Owner/Repo",
		);

		let default =
			repo_from_remote("https://github.com/Owner/Repo.git").expect("github.com remote");
		assert_eq!(default.identity(), "Owner/Repo");
		assert_eq!(default.api_url("/repos/Owner/Repo"), "https://api.github.com/repos/Owner/Repo");
		let workspace = tempfile::tempdir().expect("workspace");
		std::fs::create_dir(workspace.path().join(".git")).expect("git directory");
		std::fs::write(
			workspace.path().join(".git/config"),
			"[remote \"upstream\"]\n\turl = https://github.com/other/upstream.git\n[remote \
			 \"origin\"]\n\turl = ssh://git@ghe/Owner/Repo.git\n",
		)
		.expect("git config");
		let inferred = infer_repo(workspace.path()).expect("origin repo");
		assert_eq!(inferred.identity(), "ghe/Owner/Repo");
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn list_urls_stay_outside_the_numbered_parser() {
		let root = Path::new("/unused");
		let list =
			numbered_ref(GithubScheme::Issue, "acme/widget", Some("state=open"), root).expect("list");
		assert!(list.is_none());
		let enterprise = numbered_ref(
			GithubScheme::Issue,
			"ghe.example.com/acme/widget",
			Some("state=closed"),
			root,
		)
		.expect("enterprise list");
		assert!(enterprise.is_none());
		let numbered = numbered_ref(GithubScheme::PullRequest, "acme/widget/7", None, root)
			.expect("numbered")
			.expect("change request");
		assert_eq!(numbered.host.as_deref(), Some(GITHUB_HOST));
		assert_eq!(numbered.owner.as_str(), "acme");
		assert_eq!(numbered.number, 7);
		assert_eq!(numbered.selector, ResourceSelector::None);
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn shorthand_number_uses_the_checkout_and_keeps_a_diff_selector() {
		let workspace = tempfile::tempdir().expect("workspace");
		std::fs::create_dir(workspace.path().join(".git")).expect("git directory");
		std::fs::write(
			workspace.path().join(".git/config"),
			"[remote \"origin\"]\n\turl = https://github.com/acme/widget.git\n",
		)
		.expect("git config");
		let parsed = numbered_ref(GithubScheme::PullRequest, "7/diff/2", None, workspace.path())
			.expect("shorthand")
			.expect("change request");
		assert_eq!(parsed.host.as_deref(), Some(GITHUB_HOST));
		assert_eq!(parsed.owner.as_str(), "acme");
		assert_eq!(parsed.name.as_str(), "widget");
		assert_eq!(parsed.number, 7);
		assert_eq!(parsed.selector, ResourceSelector::Diff(DiffSelector::Slice(2)));
	}

	#[derive(Clone)]
	#[cfg(feature = "cap-forge")]
	struct Memory {
		calls: Arc<Mutex<Vec<&'static str>>>,
		fail:  Arc<Mutex<bool>>,
		repo:  Arc<Mutex<Option<RepoRef>>>,
	}

	#[cfg(feature = "cap-forge")]
	impl Memory {
		fn new() -> Self {
			Self {
				calls: Arc::new(Mutex::new(Vec::new())),
				fail:  Arc::new(Mutex::new(false)),
				repo:  Arc::new(Mutex::new(None)),
			}
		}

		fn calls(&self) -> Vec<&'static str> {
			self.calls.lock().clone()
		}

		fn note(&self, name: &'static str, repo: &RepoRef) {
			self.calls.lock().push(name);
			*self.repo.lock() = Some(repo.clone());
		}
	}

	#[cfg(feature = "cap-forge")]
	impl ForgeHost for Memory {
		fn view_repo(
			&self,
			repo: &RepoRef,
		) -> impl Future<Output = Result<Repository, ForgeError>> + Send {
			let repo = repo.clone();
			async move {
				Ok(Repository {
					repo,
					default_branch: Str::empty(),
					clone_url: Str::empty(),
					web_url: Str::empty(),
					description: Str::empty(),
				})
			}
		}

		fn read_file(
			&self,
			query: &FileQuery,
		) -> impl Future<Output = Result<FileBody, ForgeError>> + Send {
			let query = query.clone();
			async move { Ok(FileBody { query, content: bytes::Bytes::new() }) }
		}

		fn view_change_request(
			&self,
			repo: &RepoRef,
			number: u64,
		) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
			self.note("change", repo);
			let repo = repo.clone();
			let fail = Arc::clone(&self.fail);
			async move {
				if *fail.lock() {
					return Err(ForgeError::Unavailable { op: ForgeOp::ChangeRequestView });
				}
				Ok(ChangeRequest {
					repo,
					number,
					title: Str::new_static("Fix the widget"),
					body: Str::new_static("details"),
					state: RequestState::Open,
					draft: false,
					base_ref: Str::new_static("main"),
					head_ref: Str::new_static("topic"),
					head_commit: Str::empty(),
					base_commit: Str::empty(),
					web_url: Str::empty(),
				})
			}
		}

		fn view_issue(
			&self,
			repo: &RepoRef,
			number: u64,
		) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
			self.note("issue", repo);
			let repo = repo.clone();
			let fail = Arc::clone(&self.fail);
			async move {
				if *fail.lock() {
					return Err(ForgeError::Unavailable { op: ForgeOp::IssueView });
				}
				Ok(Issue {
					repo,
					number,
					title: Str::new_static("Bug"),
					body: Str::new_static("steps"),
					state: IssueState::Open,
					web_url: Str::empty(),
				})
			}
		}

		fn change_request_diff(
			&self,
			repo: &RepoRef,
			_number: u64,
		) -> impl Future<Output = Result<Str, ForgeError>> + Send {
			self.note("diff", repo);
			async move {
				Ok(Str::new_static(
					"diff --git a/a.txt b/a.txt\n--- a/a.txt\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n",
				))
			}
		}

		fn list_comments(
			&self,
			repo: &RepoRef,
			_number: u64,
			kind: ResourceKind,
		) -> impl Future<Output = Result<Arc<[Comment]>, ForgeError>> + Send {
			let name = match kind {
				ResourceKind::ChangeRequest => "pr-comments",
				ResourceKind::Issue => "issue-comments",
			};
			self.note(name, repo);
			async move {
				Ok(Arc::from([Comment {
					author:     Str::new_static("octocat"),
					created_at: Str::new_static("2026-11-10T00:00:00Z"),
					body:       Str::new_static("looks good"),
				}]))
			}
		}

		fn list_checks(
			&self,
			_repo: &RepoRef,
			_target: &CheckTarget,
		) -> impl Future<Output = Result<ForgeChecks, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ListChecks }) }
		}

		fn search(
			&self,
			query: SearchQuery,
		) -> impl Future<Output = Result<SearchPage, ForgeError>> + Send {
			let kind = query.kind;
			let repo = query.repo.clone();
			if let Some(repo) = &repo {
				self.note("search", repo);
			} else {
				self.calls.lock().push("search");
			}
			let fail = Arc::clone(&self.fail);
			let text = query.text.clone();
			async move {
				if *fail.lock() {
					return Err(ForgeError::Unavailable { op: kind.op() });
				}
				let Some(repo) = repo else {
					return Ok(SearchPage::new(kind, []));
				};
				let (number, title, state) = match kind {
					SearchKind::Issues => (3_u64, "Bug", "open"),
					SearchKind::ChangeRequests => (7_u64, "Fix the widget", "open"),
					_ => (1_u64, "hit", "open"),
				};
				// Echo state filter into the hit when present (list-URL parity).
				let state = text
					.as_str()
					.split_whitespace()
					.find_map(|token| token.strip_prefix("state:"))
					.filter(|value| *value != "all")
					.unwrap_or(state);
				Ok(SearchPage::new(
					kind,
					[SearchHit {
						repo,
						title: Str::new_static(title),
						web_url: Str::empty(),
						number: Some(number),
						commit: None,
						state: Str::new(state),
					}],
				))
			}
		}

		fn create_change_request(
			&self,
			_draft: ChangeRequestDraft,
		) -> impl Future<Output = Result<ChangeRequest, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestCreate }) }
		}

		fn checkout_change_request(
			&self,
			_request: &CheckoutRequest,
		) -> impl Future<Output = Result<CheckoutReceipt, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestCheckout }) }
		}

		fn push_change_request(
			&self,
			_request: &PushRequest,
		) -> impl Future<Output = Result<PushReceipt, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::ChangeRequestPush }) }
		}

		fn create_issue(
			&self,
			_draft: IssueDraft,
		) -> impl Future<Output = Result<Issue, ForgeError>> + Send {
			async { Err(ForgeError::Unsupported { op: ForgeOp::IssueCreate }) }
		}
	}

	#[cfg(feature = "cap-forge")]
	fn router(host: Memory) -> Delegated<Memory, PermitAll> {
		let mut builder = Delegated::builder(PermitAll);
		builder.register(GITHUB_HOST, host.clone()).expect("github.com");
		builder.register("gitlab.com", host).expect("gitlab.com");
		builder.build()
	}

	#[cfg(feature = "cap-forge")]
	fn open_cache(dir: &Path, soft_secs: u64) -> Arc<ForgeCache> {
		Arc::new(
			ForgeCache::open(
				dir.join("forge.sqlite3"),
				ForgeCachePolicy::new(
					true,
					std::time::Duration::from_secs(soft_secs),
					std::time::Duration::from_secs(3600),
				),
			)
			.expect("cache"),
		)
	}

	#[cfg(feature = "cap-forge")]
	fn resolver(
		scheme: GithubScheme,
		root: &Path,
		cache: Arc<ForgeCache>,
		host: Memory,
	) -> GithubResolver<Delegated<Memory, PermitAll>> {
		GithubResolver::new(
			scheme,
			root.to_path_buf(),
			cache,
			Arc::new(GithubCredentialBridge::new()),
			router(host),
		)
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn numbered_reads_render_title_and_body_and_cache_without_an_etag() {
		let dir = tempfile::tempdir().expect("dir");
		let host = Memory::new();
		let cache = open_cache(dir.path(), 300);
		let resolver =
			resolver(GithubScheme::PullRequest, dir.path(), Arc::clone(&cache), host.clone());
		let resolved = resolver
			.read_with_diags("acme/widget/7", &ParsedSelector::None)
			.await
			.expect("read");
		let text = std::str::from_utf8(resolved.data.as_ref()).expect("utf-8");
		assert_eq!(
			text,
			"# Fix the widget\n\ndetails\n\n## Comments\n\n### @octocat — \
			 2026-11-10T00:00:00Z\n\nlooks good\n"
		);
		assert!(resolved.diags.is_empty());
		assert_eq!(host.calls(), ["change", "pr-comments"]);
		let seen = host.repo.lock().clone().expect("repo");
		assert_eq!(seen.host.as_str(), GITHUB_HOST);
		assert_eq!(seen.owner.as_str(), "acme");
		assert_eq!(seen.name.as_str(), "widget");

		let key = ForgeCacheKey::new(
			ForgeResourceKind::PullRequest,
			"acme/widget",
			Some(7),
			"Detail { comments: true }",
		)
		.expect("key");
		let entry = cache.get(&key, now_ms()).expect("get").expect("cached");
		assert!(entry.etag.is_none());
		assert_eq!(entry.body.as_ref(), resolved.data.as_ref());

		let again = resolver
			.read_with_diags("acme/widget/7", &ParsedSelector::None)
			.await
			.expect("fresh");
		assert_eq!(again.data.as_ref(), resolved.data.as_ref());
		assert_eq!(host.calls(), ["change", "pr-comments"]);
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn issue_comments_can_be_disabled_and_an_unregistered_host_is_not_github() {
		let dir = tempfile::tempdir().expect("dir");
		let host = Memory::new();
		let cache = open_cache(dir.path(), 300);
		let resolver = resolver(GithubScheme::Issue, dir.path(), cache, host.clone());
		let resolved = resolver
			.read_query_with_diags("acme/widget/3", Some("comments=false"), &ParsedSelector::None)
			.await
			.expect("issue");
		let text = std::str::from_utf8(resolved.data.as_ref()).expect("utf-8");
		assert_eq!(text, "# Bug\n\nsteps\n\n_Comments disabled._");
		assert_eq!(host.calls(), ["issue"]);
		assert!(!text.contains("Open #"));

		let missing = resolver
			.read_with_diags("gitlab.example/acme/widget/4", &ParsedSelector::None)
			.await
			.expect_err("unregistered");
		assert!(missing.message().as_str().contains("gitlab.example"), "{}", missing.message());
		assert_eq!(host.calls(), ["issue"]);
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn diff_selectors_slice_the_host_text() {
		let dir = tempfile::tempdir().expect("dir");
		let host = Memory::new();
		let cache = open_cache(dir.path(), 300);
		let resolver = resolver(GithubScheme::PullRequest, dir.path(), cache, host.clone());
		let index = resolver
			.read_with_diags("acme/widget/7/diff", &ParsedSelector::None)
			.await
			.expect("index");
		let index_text = std::str::from_utf8(index.data.as_ref()).expect("utf-8");
		assert!(index_text.contains("pr://acme/widget/7/diff/1"), "{index_text}");
		assert!(index_text.contains("pr://acme/widget/7/diff/2"), "{index_text}");
		let slice = resolver
			.read_with_diags("acme/widget/7/diff/2", &ParsedSelector::None)
			.await
			.expect("slice");
		let slice_text = std::str::from_utf8(slice.data.as_ref()).expect("utf-8");
		assert!(slice_text.contains("b.txt"), "{slice_text}");
		assert!(!slice_text.contains("a.txt"), "{slice_text}");
		let all = resolver
			.read_with_diags("acme/widget/7/diff/all", &ParsedSelector::None)
			.await
			.expect("all");
		let all_text = std::str::from_utf8(all.data.as_ref()).expect("utf-8");
		assert!(all_text.starts_with("diff --git a/a.txt"), "{all_text}");
		assert!(host.calls().iter().all(|call| *call == "diff"));
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn list_urls_resolve_through_forge_search_and_cache_without_an_etag() {
		let dir = tempfile::tempdir().expect("dir");
		let host = Memory::new();
		let cache = open_cache(dir.path(), 300);
		let resolver =
			resolver(GithubScheme::Issue, dir.path(), Arc::clone(&cache), host.clone());
		let resolved = resolver
			.read_query_with_diags("acme/widget", Some("state=open"), &ParsedSelector::None)
			.await
			.expect("list");
		let text = std::str::from_utf8(resolved.data.as_ref()).expect("utf-8");
		assert_eq!(
			text,
			"# Issues\n\n- [open] #3 Bug\n  issue://acme/widget/3\n"
		);
		assert_eq!(host.calls(), ["search"]);
		let seen = host.repo.lock().clone().expect("repo");
		assert_eq!(seen.host.as_str(), GITHUB_HOST);
		assert_eq!(seen.owner.as_str(), "acme");
		assert_eq!(seen.name.as_str(), "widget");

		let key = ForgeCacheKey::new(
			ForgeResourceKind::Issue,
			"acme/widget",
			None,
			"List { state: \"open\", limit: 30, author: None, label: None }",
		)
		.expect("key");
		let entry = cache.get(&key, now_ms()).expect("get").expect("cached");
		assert!(entry.etag.is_none());
		assert_eq!(entry.body.as_ref(), resolved.data.as_ref());

		let again = resolver
			.read_query_with_diags("acme/widget", Some("state=open"), &ParsedSelector::None)
			.await
			.expect("fresh");
		assert_eq!(again.data.as_ref(), resolved.data.as_ref());
		assert_eq!(host.calls(), ["search"]);
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn gitlab_list_urls_route_through_the_registered_forge_host() {
		let dir = tempfile::tempdir().expect("dir");
		let host = Memory::new();
		let cache = open_cache(dir.path(), 300);
		let resolver =
			resolver(GithubScheme::PullRequest, dir.path(), cache, host.clone());
		let resolved = resolver
			.read_query_with_diags(
				"gitlab.com/acme/widget",
				Some("state=open&limit=10"),
				&ParsedSelector::None,
			)
			.await
			.expect("gitlab list");
		let text = std::str::from_utf8(resolved.data.as_ref()).expect("utf-8");
		assert_eq!(
			text,
			"# Pull Requests\n\n- [open] #7 Fix the widget\n  pr://gitlab.com/acme/widget/7\n"
		);
		assert_eq!(host.calls(), ["search"]);
		let seen = host.repo.lock().clone().expect("repo");
		assert_eq!(seen.host.as_str(), "gitlab.com");
		assert_eq!(seen.owner.as_str(), "acme");
		assert_eq!(seen.name.as_str(), "widget");
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn a_failed_refresh_returns_the_cached_body_and_a_stale_diag() {
		let dir = tempfile::tempdir().expect("dir");
		let host = Memory::new();
		let cache = open_cache(dir.path(), 1);
		let key = ForgeCacheKey::new(
			ForgeResourceKind::Issue,
			"acme/widget",
			Some(3),
			"Detail { comments: true }",
		)
		.expect("key");
		let cached =
			"# Bug\n\nsteps\n\n## Comments\n\n### @octocat — 2026-11-10T00:00:00Z\n\nlooks good\n";
		cache
			.put(&key, cached.as_bytes(), Some("W/\"old\""), now_ms().saturating_sub(5_000))
			.expect("seed");
		*host.fail.lock() = true;
		let resolver = resolver(GithubScheme::Issue, dir.path(), cache, host.clone());
		let resolved = resolver
			.read_with_diags("acme/widget/3", &ParsedSelector::None)
			.await
			.expect("stale");
		assert_eq!(resolved.data.as_ref(), cached.as_bytes());
		assert_eq!(resolved.diags.len(), 1);
		assert_eq!(resolved.diags[0].native_kind(), Some(DiagKind::StaleCache));
		assert_eq!(host.calls(), ["issue"]);
	}
}
