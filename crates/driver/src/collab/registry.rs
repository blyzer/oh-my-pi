//! Local collaboration host registry.
//!
//! Every process that hosts a room publishes a private IPC endpoint (a Unix
//! domain socket, never TCP) so a separate local process can discover live
//! hosts and, on explicit request, retrieve a shareable link. Two operations
//! travel over the endpoint as newline-delimited JSON, each authenticated by a
//! per-publication bearer token:
//!
//! - `snapshot`: non-capability metadata (identity, session, cwd, model,
//!   participants, relay and activity state) suitable for listing;
//! - `link`: one link for one exact host generation and access level, refused
//!   when the room rotated or the level is not published.
//!
//! Room keys and write tokens stay in host memory. Disk carries only
//! ephemeral discovery metadata (protocol version, instance id, pid, endpoint,
//! creation time, bearer token) in an owner-only directory. Endpoints die with
//! the host process, so a crash leaves at most stale metadata that the next
//! listing prunes.

use std::{
	io,
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use futures::{StreamExt as _, stream};
use omp_core::{Str, hex};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Discovery metadata and IPC protocol version. Mixed versions fail safely.
pub const REGISTRY_VERSION: u32 = 1;

/// Requests beyond this size are dropped; a valid request is a few hundred
/// bytes.
const MAX_REQUEST_BYTES: usize = 4 * 1024;
/// Responses beyond this size are ignored by the client.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// Longest free-form snapshot string sent on the wire. Bounding each field
/// keeps every snapshot inside [`MAX_RESPONSE_BYTES`] however the session is
/// named.
const MAX_FIELD_CHARS: usize = 1024;
/// Per-entry connect and response deadline while listing.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Server-side deadline for a client to send its request line.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Discovery entries queried concurrently.
const LIST_CONCURRENCY: usize = 8;
/// Socket paths at or beyond this length move to a short private directory;
/// `sun_path` holds 104 (macOS) or 108 (Linux) bytes.
#[cfg(unix)]
const SOCKET_PATH_LIMIT: usize = 100;

/// The access a link grants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Access {
	/// Bare room key: read-only.
	View,
	/// Room key plus write token: guests may prompt and interrupt.
	Control,
}

/// The model a hosted session currently uses.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelRef {
	/// Provider name.
	pub provider: Str,
	/// Model id.
	pub id:       Str,
}

/// Non-capability host state, computed by the host at query time.
///
/// Free-form strings are bounded to 1024 characters on the wire.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSnapshot {
	/// Random per-process identity, stable across the host's room generations.
	pub instance_id:     Str,
	/// Increments each time this process starts a new room.
	pub generation:      u64,
	/// Host process id.
	pub pid:             u32,
	/// Session id of the hosted conversation.
	pub session_id:      Str,
	/// Human-readable session name, when one is set.
	pub session_name:    Option<Str>,
	/// Host working directory.
	pub cwd:             Str,
	/// Model the host session uses, when one is selected.
	pub model:           Option<ModelRef>,
	/// Epoch milliseconds when the room started.
	pub started_at:      u64,
	/// Participant count, including the host.
	pub participants:    u32,
	/// Whether the host currently holds an open relay connection.
	pub relay_connected: bool,
	/// Whether a host-side question waits for a writable guest's answer.
	pub input_required:  bool,
	/// Whether the session is running a turn.
	pub busy:            bool,
	/// Highest access this host will hand out.
	pub access:          Access,
}

impl HostSnapshot {
	fn bounded(mut self) -> Self {
		self.session_id = bound_field(self.session_id);
		self.session_name = self.session_name.map(bound_field);
		self.cwd = bound_field(self.cwd);
		self.model = self.model.map(|model| ModelRef {
			provider: bound_field(model.provider),
			id:       bound_field(model.id),
		});
		self
	}
}

fn bound_field(value: Str) -> Str {
	match value.char_indices().nth(MAX_FIELD_CHARS) {
		Some((end, _)) => Str::new(&value[..end]),
		None => value,
	}
}

/// Live host state served over the IPC endpoint.
pub trait HostRegistrySource: Send + Sync + 'static {
	/// Current metadata, or `None` when the host can no longer vouch for its
	/// session.
	fn snapshot(&self) -> Option<HostSnapshot>;
	/// The link for `access`, or `None` when that access is not published.
	fn link(&self, access: Access) -> Option<Str>;
}

/// One resolved capability returned by [`resolve_link`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedLink {
	/// Host instance id.
	pub instance_id: Str,
	/// Room generation the link belongs to.
	pub generation:  u64,
	/// Access the link grants.
	pub access:      Access,
	/// The link, joinable with `omp join`.
	pub url:         Str,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
	version:     u32,
	instance_id: Str,
	pid:         u32,
	endpoint:    PathBuf,
	created_at:  u64,
	token:       Str,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Operation {
	Snapshot,
	Link,
}

#[derive(Debug, Deserialize, Serialize)]
struct Request {
	v:          u32,
	token:      Str,
	op:         Operation,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	access:     Option<Access>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	generation: Option<u64>,
}

/// Stable wire failure codes; they never carry links or paths.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireError {
	MalformedRequest,
	UnsupportedProtocol,
	AuthenticationFailed,
	SnapshotUnavailable,
	InvalidOperation,
	InvalidAccess,
	StaleGeneration,
	AccessUnavailable,
}

#[derive(Debug, Deserialize, Serialize)]
struct Response {
	ok:       bool,
	v:        u32,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	snapshot: Option<HostSnapshot>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	url:      Option<Str>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	error:    Option<WireError>,
}

impl Response {
	const fn failure(error: WireError) -> Self {
		Self {
			ok:       false,
			v:        REGISTRY_VERSION,
			snapshot: None,
			url:      None,
			error:    Some(error),
		}
	}

	const fn success() -> Self {
		Self {
			ok:       true,
			v:        REGISTRY_VERSION,
			snapshot: None,
			url:      None,
			error:    None,
		}
	}
}

/// Registry publication, listing, or link failure.
#[derive(Debug, Error)]
pub enum RegistryError {
	/// A filesystem operation on registry state failed.
	#[error("collab registry {operation} failed at {}", .path.display())]
	Io {
		/// Operation that failed.
		operation: &'static str,
		/// Path involved.
		path:      PathBuf,
		/// Underlying failure.
		#[source]
		source:    io::Error,
	},
	/// The registry directory is a symlink.
	#[error("collab registry directory is a symlink: {}", .path.display())]
	SymlinkDirectory {
		/// The offending path.
		path: PathBuf,
	},
	/// The registry path is not a directory.
	#[error("collab registry path is not a directory: {}", .path.display())]
	NotDirectory {
		/// The offending path.
		path: PathBuf,
	},
	/// The registry directory belongs to another user.
	#[error("collab registry directory is not owned by the current user: {}", .path.display())]
	ForeignOwner {
		/// The offending path.
		path: PathBuf,
	},
	/// The system random source failed.
	#[error("system random source failed while naming a collab registry entry")]
	Random,
	/// Discovery metadata could not be encoded.
	#[error("collab registry metadata could not be encoded")]
	Encode(#[source] serde_json::Error),
	/// Local host discovery needs Unix domain sockets.
	#[error("the collab host registry requires Unix domain sockets")]
	Unsupported,
}

/// Failure to resolve a link for one host.
#[derive(Debug, Error)]
pub enum LinkError {
	/// No live host matches the selector.
	#[error("no active collab host matches {selector}")]
	NotFound {
		/// Instance id or pid the caller gave.
		selector: Str,
	},
	/// The selector matches more than one live host.
	#[error("{selector} matches {count} collab hosts; use an instance id")]
	Ambiguous {
		/// Instance id or pid the caller gave.
		selector: Str,
		/// Number of matching hosts.
		count:    usize,
	},
	/// The room rotated since the caller listed it.
	#[error("host {instance_id} started a new room since it was listed; list again and retry")]
	StaleGeneration {
		/// Host instance id.
		instance_id: Str,
	},
	/// The host does not publish the requested access.
	#[error("host {instance_id} does not publish {access} access")]
	AccessUnavailable {
		/// Host instance id.
		instance_id: Str,
		/// Requested access.
		access:      Access,
	},
	/// The host did not answer, or answered malformed data.
	#[error("host {instance_id} did not answer the link request")]
	Unreachable {
		/// Host instance id.
		instance_id: Str,
	},
	/// The registry itself could not be read.
	#[error(transparent)]
	Registry(#[from] RegistryError),
}

fn random_hex(bytes: usize) -> Result<Str, RegistryError> {
	use ring::rand::{SecureRandom as _, SystemRandom};
	let mut buffer = vec![0_u8; bytes];
	SystemRandom::new()
		.fill(&mut buffer)
		.map_err(|_| RegistryError::Random)?;
	Ok(Str::from(hex::encode(&buffer).into_string()))
}

/// Returns the profile-independent discovery directory
/// (`<data root>/run/collab-hosts`), so hosts started under any profile are
/// discoverable from any other.
pub fn default_registry_dir() -> Result<PathBuf, omp_core::dirs::DataDirError> {
	let base = if let Some(path) = std::env::var_os("OMP_DATA_DIR").filter(|value| !value.is_empty())
	{
		PathBuf::from(path)
	} else {
		let home = omp_core::dirs::home_dir().ok_or(omp_core::dirs::DataDirError::HomeUnset)?;
		omp_core::dirs::native_directories(&home).data
	};
	Ok(base.join("run").join("collab-hosts"))
}

pub(super) fn now_ms() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// Bearer comparison without a data-dependent early exit.
fn token_matches(expected: &str, presented: &str) -> bool {
	let (expected, presented) = (expected.as_bytes(), presented.as_bytes());
	let mut difference = expected.len() ^ presented.len();
	for (index, byte) in expected.iter().copied().enumerate() {
		difference |= usize::from(byte ^ presented.get(index).copied().unwrap_or(0));
	}
	difference == 0
}

/// Answers one authenticated request against the live source.
fn respond<S: HostRegistrySource>(line: &[u8], token: &str, source: &S) -> Response {
	let Ok(request) = serde_json::from_slice::<Request>(line) else {
		return Response::failure(WireError::MalformedRequest);
	};
	if request.v != REGISTRY_VERSION {
		return Response::failure(WireError::UnsupportedProtocol);
	}
	if !token_matches(token, request.token.as_str()) {
		return Response::failure(WireError::AuthenticationFailed);
	}
	let Some(snapshot) = source.snapshot() else {
		return Response::failure(WireError::SnapshotUnavailable);
	};
	match request.op {
		Operation::Snapshot => Response { snapshot: Some(snapshot.bounded()), ..Response::success() },
		Operation::Link => {
			let Some(access) = request.access else {
				return Response::failure(WireError::InvalidAccess);
			};
			// A capability is bound to the exact generation the caller listed: a
			// room that rotated underneath a stale listing must not hand out its
			// successor.
			if request.generation != Some(snapshot.generation) {
				return Response::failure(WireError::StaleGeneration);
			}
			if access == Access::Control && snapshot.access != Access::Control {
				return Response::failure(WireError::AccessUnavailable);
			}
			match source.link(access) {
				Some(url) => Response { url: Some(url), ..Response::success() },
				None => Response::failure(WireError::AccessUnavailable),
			}
		},
	}
}

#[cfg(unix)]
mod unix {
	//! Unix-domain-socket transport for the registry.

	use std::{
		fs,
		os::unix::fs::{
			DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
		},
	};

	use tokio::{
		io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
		net::{UnixListener, UnixStream},
		task::JoinHandle,
		time::timeout,
	};

	use super::*;

	fn io_error(operation: &'static str, path: &Path) -> impl FnOnce(io::Error) -> RegistryError {
		let path = path.to_path_buf();
		move |source| RegistryError::Io { operation, path, source }
	}

	/// The registry must be a real directory owned by the caller. Publication
	/// and listing both check this: listing prunes malformed entries, so
	/// following a symlink into an unrelated directory would let a planted
	/// link turn a listing into a deletion tool.
	fn assert_private_dir(dir: &Path) -> Result<fs::Metadata, RegistryError> {
		let metadata = fs::symlink_metadata(dir).map_err(io_error("stat", dir))?;
		if metadata.file_type().is_symlink() {
			return Err(RegistryError::SymlinkDirectory { path: dir.to_path_buf() });
		}
		if !metadata.is_dir() {
			return Err(RegistryError::NotDirectory { path: dir.to_path_buf() });
		}
		// SAFETY: `geteuid` has no preconditions and cannot fail.
		let euid = unsafe { libc::geteuid() };
		if metadata.uid() != euid {
			return Err(RegistryError::ForeignOwner { path: dir.to_path_buf() });
		}
		Ok(metadata)
	}

	/// Creates the directory owner-only and tightens an existing one.
	fn ensure_private_dir(dir: &Path) -> Result<(), RegistryError> {
		fs::DirBuilder::new()
			.recursive(true)
			.mode(0o700)
			.create(dir)
			.map_err(io_error("create directory", dir))?;
		let metadata = assert_private_dir(dir)?;
		if metadata.permissions().mode() & 0o077 != 0 {
			fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
				.map_err(io_error("restrict directory", dir))?;
		}
		Ok(())
	}

	/// Short owner-private socket directory for registries whose canonical path
	/// would overflow `sun_path`, keyed by the canonical registry directory.
	fn fallback_socket_dir(dir: &Path) -> PathBuf {
		let digest = omp_core::Hash32::sum(dir.as_os_str().as_encoded_bytes());
		let key = digest.to_hex();
		std::env::temp_dir().join(format!("omp-collab-{}", &key.as_str()[..20]))
	}

	fn resolve_endpoint(dir: &Path, entry: &str) -> Result<PathBuf, RegistryError> {
		let canonical = dir.join(format!("{entry}.sock"));
		if canonical.as_os_str().len() < SOCKET_PATH_LIMIT {
			return Ok(canonical);
		}
		let short = fallback_socket_dir(dir);
		ensure_private_dir(&short)?;
		Ok(short.join(format!("{entry}.sock")))
	}

	/// A live registry entry; dropping it withdraws the host.
	pub struct Publication {
		endpoint: PathBuf,
		metadata: PathBuf,
		server:   JoinHandle<()>,
	}

	impl Publication {
		/// Publishes `source` under a fresh entry in `dir`.
		///
		/// Creates the owner-only directory, starts a private endpoint backed by
		/// `source`, and writes discovery metadata that never carries links or
		/// room secrets.
		pub async fn publish<S: HostRegistrySource>(
			dir: &Path,
			instance_id: &str,
			source: Arc<S>,
		) -> Result<Self, RegistryError> {
			ensure_private_dir(dir)?;
			// An unpredictable per-publication id names the endpoint and metadata:
			// pid reuse cannot attach stale metadata to another process, and a room
			// that rotates never shares artifact names with its predecessor.
			let entry = random_hex(8)?;
			let token = random_hex(32)?;
			let endpoint = resolve_endpoint(dir, entry.as_str())?;
			let metadata_path = dir.join(format!("{entry}.json"));
			let listener =
				UnixListener::bind(&endpoint).map_err(io_error("bind endpoint", &endpoint))?;
			let cleanup = |error: RegistryError| {
				let _ = fs::remove_file(&endpoint);
				error
			};
			fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600))
				.map_err(io_error("restrict endpoint", &endpoint))
				.map_err(cleanup)?;
			let metadata = Metadata {
				version:     REGISTRY_VERSION,
				instance_id: Str::new(instance_id),
				pid:         std::process::id(),
				endpoint:    endpoint.clone(),
				created_at:  now_ms(),
				token:       token.clone(),
			};
			let encoded = serde_json::to_vec(&metadata)
				.map_err(RegistryError::Encode)
				.map_err(cleanup)?;
			// Write-then-rename so a concurrent listing never observes a partial
			// file (it would classify the entry as malformed and prune it).
			let temporary = dir.join(format!("{entry}.json.tmp"));
			let written = (|| {
				use std::io::Write as _;
				let mut file = fs::OpenOptions::new()
					.write(true)
					.create_new(true)
					.mode(0o600)
					.open(&temporary)?;
				file.write_all(&encoded)?;
				drop(file);
				fs::rename(&temporary, &metadata_path)
			})();
			if let Err(source) = written {
				let _ = fs::remove_file(&temporary);
				return Err(cleanup(RegistryError::Io {
					operation: "write metadata",
					path: metadata_path,
					source,
				}));
			}
			let server = tokio::spawn(serve(listener, token, source));
			Ok(Self { endpoint, metadata: metadata_path, server })
		}

		/// The endpoint this publication listens on; not secret.
		#[must_use]
		pub fn endpoint(&self) -> &Path {
			&self.endpoint
		}
	}

	impl Drop for Publication {
		fn drop(&mut self) {
			self.server.abort();
			let _ = fs::remove_file(&self.metadata);
			let _ = fs::remove_file(&self.endpoint);
		}
	}

	async fn serve<S: HostRegistrySource>(listener: UnixListener, token: Str, source: Arc<S>) {
		while let Ok((stream, _)) = listener.accept().await {
			tokio::spawn(handle_connection(stream, token.clone(), Arc::clone(&source)));
		}
	}

	/// One request per connection: authenticate, dispatch, respond, close.
	async fn handle_connection<S: HostRegistrySource>(
		stream: UnixStream,
		token: Str,
		source: Arc<S>,
	) {
		let (reader, mut writer) = stream.into_split();
		let mut reader = BufReader::new(reader.take(MAX_REQUEST_BYTES as u64 + 1));
		let mut line = Vec::new();
		let read = timeout(REQUEST_TIMEOUT, reader.read_until(b'\n', &mut line)).await;
		if !matches!(read, Ok(Ok(bytes)) if bytes > 0 && line.ends_with(b"\n") && line.len() <= MAX_REQUEST_BYTES)
		{
			return;
		}
		let response = respond(&line, token.as_str(), &*source);
		let Ok(mut encoded) = serde_json::to_vec(&response) else {
			return;
		};
		encoded.push(b'\n');
		let _ = writer.write_all(&encoded).await;
		let _ = writer.shutdown().await;
	}

	enum Query {
		Answer(Box<Response>),
		/// The endpoint is gone: the host died.
		Dead,
		/// Unreachable or refused for a reason that says nothing about liveness.
		Skip(Option<WireError>),
	}

	/// Connects, sends one request, and reads one bounded response line.
	async fn query(metadata: &Metadata, request: &Request, deadline: Duration) -> Query {
		let exchange = async {
			let mut stream = match UnixStream::connect(&metadata.endpoint).await {
				Ok(stream) => stream,
				// Endpoints die with their host. Any other error (EMFILE, EACCES,
				// ...) says nothing about liveness and must not prune a live host.
				Err(error)
					if matches!(
						error.kind(),
						io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
					) =>
				{
					return Query::Dead;
				},
				Err(_) => return Query::Skip(None),
			};
			let Ok(mut encoded) = serde_json::to_vec(request) else {
				return Query::Skip(None);
			};
			encoded.push(b'\n');
			if stream.write_all(&encoded).await.is_err() {
				return Query::Skip(None);
			}
			let mut reader = BufReader::new((&mut stream).take(MAX_RESPONSE_BYTES as u64 + 1));
			let mut line = Vec::new();
			if !matches!(reader.read_until(b'\n', &mut line).await, Ok(bytes) if bytes > 0)
				|| line.len() > MAX_RESPONSE_BYTES
			{
				return Query::Skip(None);
			}
			match serde_json::from_slice::<Response>(&line) {
				Ok(response) if response.ok && response.v == REGISTRY_VERSION => {
					Query::Answer(Box::new(response))
				},
				Ok(response) => Query::Skip(response.error),
				Err(_) => Query::Skip(None),
			}
		};
		timeout(deadline, exchange)
			.await
			.unwrap_or(Query::Skip(None))
	}

	fn pid_alive(pid: u32) -> bool {
		let Ok(pid) = libc::pid_t::try_from(pid) else {
			return false;
		};
		// SAFETY: signal 0 only probes for existence.
		let result = unsafe { libc::kill(pid, 0) };
		result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
	}

	/// Removes one stale entry. Artifact names are unique per publication, so
	/// the metadata and endpoint observed dead can only belong to that
	/// publication.
	fn prune(dir: &Path, name: &Path, metadata: Option<&Metadata>) {
		let _ = fs::remove_file(dir.join(name));
		if let Some(metadata) = metadata {
			let parent = metadata.endpoint.parent();
			if parent == Some(dir) || parent == Some(fallback_socket_dir(dir).as_path()) {
				let _ = fs::remove_file(&metadata.endpoint);
			}
		}
	}

	struct LiveEntry {
		snapshot: HostSnapshot,
		metadata: Metadata,
	}

	async fn list_entry(dir: &Path, name: PathBuf, deadline: Duration) -> Option<LiveEntry> {
		let text = fs::read(dir.join(&name)).ok()?;
		let Ok(metadata) = serde_json::from_slice::<Metadata>(&text) else {
			// Malformed metadata can never become listable.
			prune(dir, &name, None);
			return None;
		};
		if metadata.version != REGISTRY_VERSION {
			// Another omp version owns this entry: never show it, and prune it
			// only once its process is gone.
			if !pid_alive(metadata.pid) {
				prune(dir, &name, Some(&metadata));
			}
			return None;
		}
		let request = Request {
			v:          REGISTRY_VERSION,
			token:      metadata.token.clone(),
			op:         Operation::Snapshot,
			access:     None,
			generation: None,
		};
		match query(&metadata, &request, deadline).await {
			Query::Answer(response) => {
				let snapshot = response.snapshot?;
				Some(LiveEntry { snapshot, metadata })
			},
			Query::Dead => {
				prune(dir, &name, Some(&metadata));
				None
			},
			Query::Skip(_) => None,
		}
	}

	async fn live_entries(dir: &Path, deadline: Duration) -> Result<Vec<LiveEntry>, RegistryError> {
		let names = match assert_private_dir(dir) {
			Ok(_) => fs::read_dir(dir).map_err(io_error("read directory", dir))?,
			Err(RegistryError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
				return Ok(Vec::new());
			},
			Err(error) => return Err(error),
		};
		let mut entries = names
			.filter_map(Result::ok)
			.map(|entry| PathBuf::from(entry.file_name()))
			.filter(|name| {
				name
					.extension()
					.is_some_and(|extension| extension == "json")
			})
			.collect::<Vec<_>>();
		entries.sort();
		let mut live = stream::iter(entries)
			.map(|name| list_entry(dir, name, deadline))
			.buffer_unordered(LIST_CONCURRENCY)
			.filter_map(std::future::ready)
			.collect::<Vec<_>>()
			.await;
		live.sort_by(|left, right| {
			(left.snapshot.started_at, left.snapshot.pid, &left.snapshot.instance_id).cmp(&(
				right.snapshot.started_at,
				right.snapshot.pid,
				&right.snapshot.instance_id,
			))
		});
		Ok(live)
	}

	/// Lists live hosts under `dir`, oldest first.
	///
	/// Queries every entry concurrently with a short independent deadline,
	/// prunes entries left by crashed hosts, and omits unresponsive, foreign
	/// version, or malformed entries without failing the listing. The result
	/// carries no links.
	pub async fn list_hosts(
		dir: &Path,
		deadline: Duration,
	) -> Result<Vec<HostSnapshot>, RegistryError> {
		Ok(live_entries(dir, deadline)
			.await?
			.into_iter()
			.map(|entry| entry.snapshot)
			.collect())
	}

	/// Resolves one link for the host `selector` names: an exact instance id, or
	/// a pid when no instance matches.
	///
	/// The request carries the generation observed while listing, so a host
	/// that rotated rooms in between answers `stale_generation` instead of
	/// leaking its successor's capability.
	pub async fn resolve_link(
		dir: &Path,
		selector: &str,
		access: Access,
		deadline: Duration,
	) -> Result<ResolvedLink, LinkError> {
		let wanted = selector.trim();
		let live = live_entries(dir, deadline).await?;
		let mut matches = live
			.iter()
			.filter(|entry| entry.snapshot.instance_id.as_str() == wanted)
			.collect::<Vec<_>>();
		if matches.is_empty()
			&& let Ok(pid) = wanted.parse::<u32>()
			&& pid > 0
		{
			matches = live
				.iter()
				.filter(|entry| entry.snapshot.pid == pid)
				.collect();
		}
		let entry = match matches.as_slice() {
			[] => return Err(LinkError::NotFound { selector: Str::new(wanted) }),
			[entry] => *entry,
			many => {
				return Err(LinkError::Ambiguous { selector: Str::new(wanted), count: many.len() });
			},
		};
		let instance_id = entry.snapshot.instance_id.clone();
		let request = Request {
			v:          REGISTRY_VERSION,
			token:      entry.metadata.token.clone(),
			op:         Operation::Link,
			access:     Some(access),
			generation: Some(entry.snapshot.generation),
		};
		match query(&entry.metadata, &request, deadline).await {
			Query::Answer(response) => match response.url {
				Some(url) if !url.is_empty() => {
					Ok(ResolvedLink { instance_id, generation: entry.snapshot.generation, access, url })
				},
				_ => Err(LinkError::Unreachable { instance_id }),
			},
			Query::Skip(Some(WireError::StaleGeneration)) => {
				Err(LinkError::StaleGeneration { instance_id })
			},
			Query::Skip(Some(WireError::AccessUnavailable)) => {
				Err(LinkError::AccessUnavailable { instance_id, access })
			},
			Query::Dead => {
				// Every room generation has its own endpoint, so a host that rotated
				// since the listing is gone from this one rather than answering
				// `stale_generation`. Look the instance up again before giving up.
				let rotated = live_entries(dir, deadline).await?.iter().any(|other| {
					other.snapshot.instance_id == instance_id
						&& other.snapshot.generation > entry.snapshot.generation
				});
				if rotated {
					Err(LinkError::StaleGeneration { instance_id })
				} else {
					Err(LinkError::Unreachable { instance_id })
				}
			},
			Query::Skip(_) => Err(LinkError::Unreachable { instance_id }),
		}
	}
}

#[cfg(unix)]
pub use unix::{Publication, list_hosts, resolve_link};

/// A live registry entry; dropping it withdraws the host.
#[cfg(not(unix))]
pub struct Publication;

#[cfg(not(unix))]
impl Publication {
	/// Local host discovery needs Unix domain sockets.
	pub async fn publish<S: HostRegistrySource>(
		_dir: &Path,
		_instance_id: &str,
		_source: Arc<S>,
	) -> Result<Self, RegistryError> {
		Err(RegistryError::Unsupported)
	}
}

/// Lists live hosts; without Unix domain sockets there is nothing to list.
#[cfg(not(unix))]
pub async fn list_hosts(
	_dir: &Path,
	_deadline: Duration,
) -> Result<Vec<HostSnapshot>, RegistryError> {
	Ok(Vec::new())
}

/// Resolves a link; without Unix domain sockets no host is discoverable.
#[cfg(not(unix))]
pub async fn resolve_link(
	_dir: &Path,
	selector: &str,
	_access: Access,
	_deadline: Duration,
) -> Result<ResolvedLink, LinkError> {
	Err(LinkError::NotFound { selector: Str::new(selector) })
}

/// Returns a fresh random per-process instance id.
pub fn new_instance_id() -> Result<Str, RegistryError> {
	random_hex(8)
}

#[cfg(all(test, unix))]
mod tests {
	use std::{
		fs,
		os::unix::fs::PermissionsExt as _,
		sync::atomic::{AtomicU64, Ordering},
	};

	use super::*;

	struct Fixed {
		generation: AtomicU64,
		access:     Access,
		name:       Str,
	}

	impl Fixed {
		fn new(access: Access) -> Arc<Self> {
			Arc::new(Self { generation: AtomicU64::new(1), access, name: Str::new("session") })
		}
	}

	impl HostRegistrySource for Fixed {
		fn snapshot(&self) -> Option<HostSnapshot> {
			Some(HostSnapshot {
				instance_id:     Str::new("0123456789abcdef"),
				generation:      self.generation.load(Ordering::SeqCst),
				pid:             std::process::id(),
				session_id:      Str::new("01SESSION"),
				session_name:    Some(self.name.clone()),
				cwd:             Str::new("/work"),
				model:           Some(ModelRef { provider: Str::new("p"), id: Str::new("m") }),
				started_at:      1,
				participants:    2,
				relay_connected: true,
				input_required:  false,
				busy:            true,
				access:          self.access,
			})
		}

		fn link(&self, access: Access) -> Option<Str> {
			Some(Str::new(format!("link-{access}")))
		}
	}

	const DEADLINE: Duration = Duration::from_secs(5);

	#[tokio::test]
	async fn publish_list_and_link_round_trip_with_owner_only_permissions() {
		let root = tempfile::tempdir().expect("scratch");
		let dir = root.path().join("collab-hosts");
		let publication = Publication::publish(&dir, "0123456789abcdef", Fixed::new(Access::Control))
			.await
			.expect("publish");
		assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
		assert_eq!(
			fs::metadata(publication.endpoint())
				.unwrap()
				.permissions()
				.mode() & 0o777,
			0o600
		);

		let hosts = list_hosts(&dir, DEADLINE).await.expect("list");
		assert_eq!(hosts.len(), 1);
		assert_eq!(hosts[0].session_name.as_deref(), Some("session"));
		assert!(hosts[0].busy);

		for access in [Access::View, Access::Control] {
			let link = resolve_link(&dir, "0123456789abcdef", access, DEADLINE)
				.await
				.expect("link");
			assert_eq!(link.url.as_str(), format!("link-{access}"));
			assert_eq!(link.generation, 1);
		}
		// A pid selector resolves the same host when no instance id matches.
		let by_pid = resolve_link(&dir, &std::process::id().to_string(), Access::View, DEADLINE)
			.await
			.expect("pid selector");
		assert_eq!(by_pid.instance_id.as_str(), "0123456789abcdef");
	}

	#[tokio::test]
	async fn view_only_hosts_refuse_control_links() {
		let root = tempfile::tempdir().expect("scratch");
		let dir = root.path().join("hosts");
		let _publication = Publication::publish(&dir, "0123456789abcdef", Fixed::new(Access::View))
			.await
			.expect("publish");
		let error = resolve_link(&dir, "0123456789abcdef", Access::Control, DEADLINE)
			.await
			.expect_err("control refused");
		assert!(matches!(error, LinkError::AccessUnavailable { access: Access::Control, .. }));
		resolve_link(&dir, "0123456789abcdef", Access::View, DEADLINE)
			.await
			.expect("view link");
	}

	#[tokio::test]
	async fn rotated_generation_never_leaks_the_successor_link() {
		let root = tempfile::tempdir().expect("scratch");
		let source = Fixed::new(Access::Control);
		let request_for = |generation, token: &Str| Request {
			v:          REGISTRY_VERSION,
			token:      token.clone(),
			op:         Operation::Link,
			access:     Some(Access::Control),
			generation: Some(generation),
		};
		let token = Str::new("secret");
		assert_eq!(
			respond(&serde_json::to_vec(&request_for(1, &token)).unwrap(), "secret", source.as_ref())
				.url
				.as_deref(),
			Some("link-control")
		);
		source.generation.store(2, Ordering::SeqCst);
		let response =
			respond(&serde_json::to_vec(&request_for(1, &token)).unwrap(), "secret", source.as_ref());
		assert_eq!(response.error, Some(WireError::StaleGeneration));
		assert!(response.url.is_none());
		drop(root);
	}

	#[test]
	fn wrong_token_and_malformed_requests_are_refused_without_a_snapshot() {
		let source = Fixed::new(Access::Control);
		let request = Request {
			v:          REGISTRY_VERSION,
			token:      Str::new("wrong"),
			op:         Operation::Snapshot,
			access:     None,
			generation: None,
		};
		let response = respond(&serde_json::to_vec(&request).unwrap(), "right", source.as_ref());
		assert_eq!(response.error, Some(WireError::AuthenticationFailed));
		assert!(response.snapshot.is_none());
		assert_eq!(
			respond(b"not json", "right", source.as_ref()).error,
			Some(WireError::MalformedRequest)
		);
		let old = Request { v: 0, ..request };
		assert_eq!(
			respond(&serde_json::to_vec(&old).unwrap(), "right", source.as_ref()).error,
			Some(WireError::UnsupportedProtocol)
		);
	}

	#[tokio::test]
	async fn dropping_a_publication_withdraws_it_and_crash_leftovers_are_pruned() {
		let root = tempfile::tempdir().expect("scratch");
		let dir = root.path().join("hosts");
		let publication = Publication::publish(&dir, "0123456789abcdef", Fixed::new(Access::Control))
			.await
			.expect("publish");
		let endpoint = publication.endpoint().to_path_buf();
		assert_eq!(list_hosts(&dir, DEADLINE).await.unwrap().len(), 1);
		drop(publication);
		assert!(!endpoint.exists(), "drop removes the endpoint");
		assert!(list_hosts(&dir, DEADLINE).await.unwrap().is_empty());
		assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "drop removes the metadata");

		// A crashed host leaves metadata pointing at a dead endpoint.
		let leftover = Metadata {
			version:     REGISTRY_VERSION,
			instance_id: Str::new("deadbeefdeadbeef"),
			pid:         1,
			endpoint:    dir.join("gone.sock"),
			created_at:  0,
			token:       Str::new("t"),
		};
		let path = dir.join("gone.json");
		fs::write(&path, serde_json::to_vec(&leftover).unwrap()).unwrap();
		fs::write(dir.join("junk.json"), b"{").unwrap();
		assert!(list_hosts(&dir, DEADLINE).await.unwrap().is_empty());
		assert!(!path.exists(), "dead entry pruned");
		assert!(!dir.join("junk.json").exists(), "malformed entry pruned");
	}

	#[tokio::test]
	async fn missing_and_symlinked_directories_are_handled() {
		let root = tempfile::tempdir().expect("scratch");
		assert!(
			list_hosts(&root.path().join("absent"), DEADLINE)
				.await
				.unwrap()
				.is_empty()
		);
		let target = root.path().join("real");
		fs::create_dir(&target).unwrap();
		let link = root.path().join("link");
		std::os::unix::fs::symlink(&target, &link).unwrap();
		assert!(matches!(
			list_hosts(&link, DEADLINE).await,
			Err(RegistryError::SymlinkDirectory { .. })
		));
		assert!(matches!(
			Publication::publish(&link, "0123456789abcdef", Fixed::new(Access::View)).await,
			Err(RegistryError::SymlinkDirectory { .. })
		));
	}

	#[test]
	fn snapshot_strings_are_bounded_on_the_wire() {
		let long = Str::new("x".repeat(MAX_FIELD_CHARS * 3));
		let snapshot = HostSnapshot {
			session_name: Some(long.clone()),
			cwd: long.clone(),
			..Fixed::new(Access::View).snapshot().unwrap()
		}
		.bounded();
		assert_eq!(snapshot.cwd.chars().count(), MAX_FIELD_CHARS);
		assert_eq!(snapshot.session_name.unwrap().chars().count(), MAX_FIELD_CHARS);
		assert!(
			serde_json::to_vec(&Response {
				snapshot: Some(Fixed::new(Access::View).snapshot().unwrap()),
				..Response::success()
			})
			.unwrap()
			.len() < MAX_RESPONSE_BYTES
		);
	}
}
