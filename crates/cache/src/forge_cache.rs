//! Rebuildable SQLite cache for forge issue and change-request reads.

use std::{fmt, path::Path, time::Duration};

use bytes::Bytes;
use omp_core::Str;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension as _, params};
use strum::{Display, EnumString, IntoStaticStr};

/// Forge resource family represented by one cache entry.
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum ForgeResourceKind {
	/// Issue detail or listing.
	Issue,
	/// Change-request (pull-request) detail or listing.
	PullRequest,
	/// Change-request unified diff or file index.
	Diff,
}

/// Stable cache identity independent of HTTP pagination and credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeCacheKey {
	/// Resource family.
	pub kind:   ForgeResourceKind,
	/// Lowercase `[host/]owner/repo` identity; github.com remains unqualified.
	pub repo:   Str,
	/// Item number; `None` identifies a list.
	pub number: Option<u64>,
	/// Canonical view including filters or comment mode.
	pub view:   Str,
}

impl ForgeCacheKey {
	/// Constructs and validates a cache identity.
	pub fn new(
		kind: ForgeResourceKind,
		repo: impl Into<Str>,
		number: Option<u64>,
		view: impl Into<Str>,
	) -> Result<Self, ForgeCacheError> {
		let repo = repo.into();
		let normalized = normalize_repo(&repo)?;
		let view = view.into();
		if view.is_empty() || view.len() > 512 || view.bytes().any(|byte| byte.is_ascii_control()) {
			return Err(ForgeCacheError::InvalidView);
		}
		if number == Some(0) {
			return Err(ForgeCacheError::InvalidNumber);
		}
		Ok(Self { kind, repo: normalized, number, view })
	}
}

/// Freshness state of one cached response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeCacheStatus {
	/// Entry remains inside the configured freshness window.
	Fresh,
	/// Entry is retained for stale-on-refresh-failure fallback.
	Stale,
}

/// Cached rendered or wire payload and validator metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeCacheEntry {
	/// Exact cached bytes.
	pub body:          Bytes,
	/// Conditional-refresh `ETag` from the upstream forge response.
	pub etag:          Option<Str>,
	/// Successful fetch time in Unix milliseconds.
	pub fetched_at_ms: u64,
	/// Freshness relative to the caller-provided current time.
	pub status:        ForgeCacheStatus,
}

/// Cache open, validation, or transaction failure.
#[derive(Debug, thiserror::Error)]
pub enum ForgeCacheError {
	/// Repository must be a safe `[host/]owner/repo` identity.
	#[error("forge repository must be a safe [host/]owner/repo identity")]
	InvalidRepo,
	/// Item numbers are positive.
	#[error("forge item number must be positive")]
	InvalidNumber,
	/// Cache view must be nonempty, bounded text.
	#[error("forge cache view is invalid")]
	InvalidView,
	/// Fetch timestamp exceeded SQLite integer bounds.
	#[error("forge cache timestamp exceeds SQLite bounds")]
	TimestampOverflow,
	/// SQLite cache operation failed.
	#[error("forge cache database operation failed")]
	Database(#[from] rusqlite::Error),
}

/// Runtime policy for the rebuildable forge response cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForgeCachePolicy {
	enabled:  bool,
	soft_ttl: Duration,
	hard_ttl: Duration,
}

impl ForgeCachePolicy {
	/// Creates a cache policy with independent freshness and retention windows.
	#[must_use]
	pub const fn new(enabled: bool, soft_ttl: Duration, hard_ttl: Duration) -> Self {
		Self { enabled, soft_ttl, hard_ttl }
	}
}

/// Thread-safe rebuildable forge response cache.
pub struct ForgeCache {
	connection: Mutex<Connection>,
	policy:     ForgeCachePolicy,
}

impl ForgeCache {
	/// Opens a cache database and creates its schema when absent.
	pub fn open(
		path: impl AsRef<Path>,
		policy: ForgeCachePolicy,
	) -> Result<Self, ForgeCacheError> {
		let connection = Connection::open(path)?;
		connection.execute_batch(
			"PRAGMA journal_mode=WAL;
			 PRAGMA synchronous=NORMAL;
			 CREATE TABLE IF NOT EXISTS forge_cache (
			   kind TEXT NOT NULL,
			   repo TEXT NOT NULL,
			   number INTEGER,
			   view TEXT NOT NULL,
			   etag TEXT,
			   fetched_at_ms INTEGER NOT NULL,
			   body BLOB NOT NULL,
			   PRIMARY KEY(kind, repo, number, view)
			 );
			 CREATE INDEX IF NOT EXISTS forge_cache_repo
			   ON forge_cache(repo);",
		)?;
		Ok(Self { connection: Mutex::new(connection), policy })
	}

	/// Returns an entry, retaining soft-expired bytes for refresh-failure
	/// fallback.
	///
	/// Disabled caches always miss. Entries older than the hard TTL are deleted
	/// before returning a miss, even when the soft TTL is longer.
	pub fn get(
		&self,
		key: &ForgeCacheKey,
		now_ms: u64,
	) -> Result<Option<ForgeCacheEntry>, ForgeCacheError> {
		if !self.policy.enabled {
			return Ok(None);
		}
		let number = key.number.map(sql_integer).transpose()?;
		let connection = self.connection.lock();
		let row = connection
			.query_row(
				"SELECT etag, fetched_at_ms, body FROM forge_cache
				 WHERE kind = ?1 AND repo = ?2 AND number IS ?3 AND view = ?4",
				params![kind_text(key.kind), key.repo.as_str(), number, key.view.as_str()],
				|row| {
					Ok((
						row.get::<_, Option<String>>(0)?,
						row.get::<_, i64>(1)?,
						row.get::<_, Vec<u8>>(2)?,
					))
				},
			)
			.optional()?;
		let Some((etag, fetched_at_ms, body)) = row else {
			return Ok(None);
		};
		let fetched_at_ms = u64::try_from(fetched_at_ms).unwrap_or_default();
		let age = now_ms.saturating_sub(fetched_at_ms);
		let hard_ms = u64::try_from(self.policy.hard_ttl.as_millis()).unwrap_or(u64::MAX);
		if age > hard_ms {
			connection.execute(
				"DELETE FROM forge_cache
				 WHERE kind = ?1 AND repo = ?2 AND number IS ?3 AND view = ?4",
				params![kind_text(key.kind), key.repo.as_str(), number, key.view.as_str()],
			)?;
			return Ok(None);
		}
		let fresh_ms = u64::try_from(self.policy.soft_ttl.as_millis()).unwrap_or(u64::MAX);
		Ok(Some(ForgeCacheEntry {
			body: Bytes::from(body),
			etag: etag.map(Str::new),
			fetched_at_ms,
			status: if age <= fresh_ms {
				ForgeCacheStatus::Fresh
			} else {
				ForgeCacheStatus::Stale
			},
		}))
	}

	/// Atomically inserts or replaces one successful direct-API response.
	pub fn put(
		&self,
		key: &ForgeCacheKey,
		body: &[u8],
		etag: Option<&str>,
		fetched_at_ms: u64,
	) -> Result<(), ForgeCacheError> {
		if !self.policy.enabled {
			return Ok(());
		}
		let number = key.number.map(sql_integer).transpose()?;
		let fetched_at_ms = sql_integer(fetched_at_ms)?;
		self.connection.lock().execute(
			"INSERT INTO forge_cache(kind, repo, number, view, etag, fetched_at_ms, body)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
			 ON CONFLICT(kind, repo, number, view) DO UPDATE SET
			   etag = excluded.etag,
			   fetched_at_ms = excluded.fetched_at_ms,
			   body = excluded.body",
			params![
				kind_text(key.kind),
				key.repo.as_str(),
				number,
				key.view.as_str(),
				etag,
				fetched_at_ms,
				body,
			],
		)?;
		Ok(())
	}

	/// Refreshes freshness after a direct API `304 Not Modified` response.
	pub fn touch(&self, key: &ForgeCacheKey, fetched_at_ms: u64) -> Result<bool, ForgeCacheError> {
		if !self.policy.enabled {
			return Ok(false);
		}
		let number = key.number.map(sql_integer).transpose()?;
		let fetched_at_ms = sql_integer(fetched_at_ms)?;
		let changed = self.connection.lock().execute(
			"UPDATE forge_cache SET fetched_at_ms = ?5
			 WHERE kind = ?1 AND repo = ?2 AND number IS ?3 AND view = ?4",
			params![kind_text(key.kind), key.repo.as_str(), number, key.view.as_str(), fetched_at_ms,],
		)?;
		Ok(changed != 0)
	}

	/// Invalidates every cached view for one repository.
	pub fn invalidate_repo(&self, repo: &str) -> Result<usize, ForgeCacheError> {
		let repo = normalize_repo(repo)?;
		self
			.connection
			.lock()
			.execute("DELETE FROM forge_cache WHERE repo = ?1", [repo.as_str()])
			.map_err(ForgeCacheError::from)
	}
}

impl fmt::Debug for ForgeCache {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("ForgeCache")
			.field("policy", &self.policy)
			.finish_non_exhaustive()
	}
}

fn normalize_repo(repo: &str) -> Result<Str, ForgeCacheError> {
	let mut parts = repo.trim().trim_end_matches(".git").split('/');
	let first = parts.next().unwrap_or_default();
	let second = parts.next().unwrap_or_default();
	let third = parts.next();
	if parts.next().is_some() {
		return Err(ForgeCacheError::InvalidRepo);
	}
	let (host, owner, name) = match third {
		Some(name) => (Some(first), second, name),
		None => (None, first, second),
	};
	if host.is_some_and(|host| !valid_host(host))
		|| !valid_component(owner)
		|| !valid_component(name)
	{
		return Err(ForgeCacheError::InvalidRepo);
	}
	let slug = format!("{}/{}", owner.to_ascii_lowercase(), name.to_ascii_lowercase());
	match host {
		Some(host) if !host.eq_ignore_ascii_case("github.com") => {
			Ok(Str::new(format!("{}/{slug}", host.to_ascii_lowercase())))
		},
		_ => Ok(Str::new(slug)),
	}
}

fn valid_component(component: &str) -> bool {
	!component.is_empty()
		&& component.len() <= 100
		&& component
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
fn valid_host(host: &str) -> bool {
	!host.is_empty()
		&& host.len() <= 255
		&& host
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn sql_integer(value: u64) -> Result<i64, ForgeCacheError> {
	i64::try_from(value).map_err(|_| ForgeCacheError::TimestampOverflow)
}

fn kind_text(kind: ForgeResourceKind) -> &'static str {
	kind.into()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn policy(enabled: bool, soft_secs: u64, hard_secs: u64) -> ForgeCachePolicy {
		ForgeCachePolicy::new(
			enabled,
			Duration::from_secs(soft_secs),
			Duration::from_secs(hard_secs),
		)
	}

	#[test]
	fn cache_tracks_fresh_stale_conditional_refresh_and_repo_invalidation() {
		let directory = tempfile::tempdir().expect("cache directory");
		let cache = ForgeCache::open(directory.path().join("forge.sqlite3"), policy(true, 60, 600))
			.expect("cache");
		let key = ForgeCacheKey::new(
			ForgeResourceKind::PullRequest,
			"Owner/Repo",
			Some(42),
			"detail?comments=1",
		)
		.expect("key");
		cache
			.put(&key, b"first", Some("W/\"etag-1\""), 1_000)
			.expect("put");
		let fresh = cache.get(&key, 60_999).expect("read").expect("entry");
		assert_eq!(fresh.status, ForgeCacheStatus::Fresh);
		assert_eq!(fresh.etag.as_deref(), Some("W/\"etag-1\""));
		assert_eq!(fresh.body.as_ref(), b"first");
		assert_eq!(
			cache
				.get(&key, 61_001)
				.expect("read")
				.expect("entry")
				.status,
			ForgeCacheStatus::Stale
		);
		assert!(cache.touch(&key, 70_000).expect("touch"));
		assert_eq!(
			cache
				.get(&key, 70_001)
				.expect("read")
				.expect("entry")
				.status,
			ForgeCacheStatus::Fresh
		);
		assert_eq!(cache.invalidate_repo("owner/repo").expect("invalidate"), 1);
		assert!(cache.get(&key, 70_001).expect("read").is_none());
	}

	#[test]
	fn cache_keys_namespace_enterprise_hosts_without_changing_github_dot_com() {
		let default =
			ForgeCacheKey::new(ForgeResourceKind::PullRequest, "Owner/Repo", Some(7), "detail")
				.expect("default key");
		let explicit_default = ForgeCacheKey::new(
			ForgeResourceKind::PullRequest,
			"github.com/OWNER/REPO",
			Some(7),
			"detail",
		)
		.expect("explicit default key");
		let enterprise = ForgeCacheKey::new(
			ForgeResourceKind::PullRequest,
			"GHE.Example.com/Owner/Repo",
			Some(7),
			"detail",
		)
		.expect("enterprise key");
		assert_eq!(default.repo, "owner/repo");
		assert_eq!(explicit_default.repo, default.repo);
		assert_eq!(enterprise.repo, "ghe.example.com/owner/repo");
		assert_ne!(enterprise.repo, default.repo);
	}

	#[test]
	fn list_filters_and_comment_modes_have_distinct_keys() {
		let directory = tempfile::tempdir().expect("cache directory");
		let cache = ForgeCache::open(directory.path().join("forge.sqlite3"), policy(true, 60, 600))
			.expect("cache");
		let open = ForgeCacheKey::new(
			ForgeResourceKind::Issue,
			"owner/repo",
			None,
			"list?state=open&limit=30",
		)
		.expect("open key");
		let closed = ForgeCacheKey::new(
			ForgeResourceKind::Issue,
			"owner/repo",
			None,
			"list?state=closed&limit=30",
		)
		.expect("closed key");
		let comments =
			ForgeCacheKey::new(ForgeResourceKind::Issue, "owner/repo", Some(7), "detail?comments=0")
				.expect("comments key");
		cache.put(&open, b"open", None, 1).expect("put open");
		cache.put(&closed, b"closed", None, 1).expect("put closed");
		cache
			.put(&comments, b"detail", None, 1)
			.expect("put detail");
		assert_eq!(cache.get(&open, 1).unwrap().unwrap().body.as_ref(), b"open");
		assert_eq!(cache.get(&closed, 1).unwrap().unwrap().body.as_ref(), b"closed");
		assert_eq!(cache.get(&comments, 1).unwrap().unwrap().body.as_ref(), b"detail");
	}

	#[test]
	fn policy_bypasses_disabled_cache_and_hard_ttl_dominates_soft_ttl() {
		let directory = tempfile::tempdir().expect("cache directory");
		let key = ForgeCacheKey::new(ForgeResourceKind::Issue, "owner/repo", Some(9), "detail")
			.expect("key");
		let enabled_path = directory.path().join("enabled.sqlite3");
		let cache = ForgeCache::open(&enabled_path, policy(true, 300, 10)).expect("enabled cache");
		cache.put(&key, b"expired", None, 1_000).expect("put");
		assert!(
			cache
				.get(&key, 11_001)
				.expect("hard-expired read")
				.is_none()
		);
		assert!(
			cache.get(&key, 1_000).expect("deleted-row read").is_none(),
			"hard-expired rows are removed from storage"
		);

		let disabled =
			ForgeCache::open(directory.path().join("disabled.sqlite3"), policy(false, 300, 600))
				.expect("disabled cache");
		disabled
			.put(&key, b"ignored", None, 1_000)
			.expect("disabled put");
		assert!(disabled.get(&key, 1_000).expect("disabled get").is_none());
		assert!(!disabled.touch(&key, 2_000).expect("disabled touch"));
	}
}
