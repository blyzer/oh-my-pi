//! Content-addressed artifact resolver backed directly by the journal blob CAS.
//!
//! `artifact://sha256/<digest>` reads the project blob store. A bare number
//! (`artifact://3`) is an omp v1 artifact id, meaningful only inside a session
//! imported from v1: the importer copied v1's artifact files into this store
//! and journaled each one's id and digest as a `<meta><foreign-artifact>`
//! ([`omp_session::import`]), leaving v1's URIs in the journaled text as they
//! were. The invoking session's journal is folded once into an id → digest
//! map ([`V1Ids`]).

use std::{
	fmt, fs, io,
	ops::Range,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_core::{CowBytes, FastHashMap, Hash32, Str, sf};
use omp_journal::{
	Journal,
	blob::{BlobRef, BlobStore},
};
use omp_session::import;
use omp_tool::ArtifactLifetime;
use omp_tools::read::{
	Fault,
	resolver::{
		ArtifactCatalog, ArtifactRecord, ArtifactResolver, BlobAuthority, BlobStat, Resolve,
		ResourceCompletion, ResourceList,
	},
	selector::ParsedSelector,
};
use parking_lot::Mutex;
use url::Url;

const MAX_INLINE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
struct BlobStoreAuthority {
	store: BlobStore,
}

impl BlobStoreAuthority {
	fn reference(&self, digest: &str) -> Result<BlobRef, Fault> {
		let probe = BlobRef::parse_hex(digest, 0).map_err(storage_fault)?;
		let size = fs::metadata(self.store.path(&probe))
			.map_err(io_fault)?
			.len();
		BlobRef::parse_hex(digest, size).map_err(storage_fault)
	}
}

/// omp v1 artifact ids of the sessions imported from v1, per invocation
/// principal (the hex SHA-256 of the session's journal path).
///
/// A disposable runtime index over the journals: an imported journal's
/// mapping is complete before the session first runs and never changes, so a
/// folded map is cached for the host's lifetime.
#[derive(Debug)]
struct V1Ids {
	/// The project's journal directory.
	sessions_dir: PathBuf,
	by_principal: Mutex<FastHashMap<Str, Arc<FastHashMap<u64, BlobRef>>>>,
}

impl V1Ids {
	/// The journal among `sessions_dir`'s whose path hashes to `principal`.
	fn journal(&self, principal: &str) -> Result<Option<PathBuf>, Fault> {
		let entries = match fs::read_dir(&self.sessions_dir) {
			Ok(entries) => entries,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
			Err(error) => return Err(io_fault(error)),
		};
		let principal_of = |path: &Path| Hash32::sum(path.as_os_str().as_encoded_bytes()).to_hex();
		for entry in entries {
			let path = entry.map_err(io_fault)?.path();
			if path.extension().and_then(|extension| extension.to_str())
				!= Some(omp_journal::FILE_EXTENSION)
			{
				continue;
			}
			if principal_of(&path).as_str() == principal
				|| fs::canonicalize(&path)
					.is_ok_and(|canonical| principal_of(&canonical).as_str() == principal)
			{
				return Ok(Some(path));
			}
		}
		Ok(None)
	}

	/// The v1 id → project blob map of the session `principal` names, or
	/// `None` when no journal of this project is that session's.
	fn ids(&self, principal: &str) -> Result<Option<Arc<FastHashMap<u64, BlobRef>>>, Fault> {
		if let Some(ids) = self.by_principal.lock().get(principal) {
			return Ok(Some(Arc::clone(ids)));
		}
		let Some(journal) = self.journal(principal)? else {
			return Ok(None);
		};
		let mut ids = FastHashMap::default();
		// Only a v1 import's journal is read whole; its provenance leads it.
		let head =
			Journal::scan_prefix(&journal, import::PROVENANCE_PREFIX_BYTES).map_err(storage_fault)?;
		if import::import_origin(&head).is_some_and(|origin| origin.format == import::OMP1_FORMAT) {
			let entries = Journal::scan(&journal).map_err(storage_fault)?;
			for (id, blob) in import::v1_artifacts(&entries) {
				ids.entry(id).or_insert(blob);
			}
		}
		let ids = Arc::new(ids);
		self
			.by_principal
			.lock()
			.insert(Str::new(principal), Arc::clone(&ids));
		Ok(Some(ids))
	}
}

#[derive(Clone, Debug)]
struct DigestCatalog {
	blobs: BlobStoreAuthority,
	v1:    Arc<V1Ids>,
}

impl DigestCatalog {
	/// An omp v1 artifact id of the invoking session, when it was imported
	/// from v1.
	fn v1_record(&self, id: u64) -> Result<Option<ArtifactRecord>, Fault> {
		let Some(principal) = crate::tools::invocation_session_id() else {
			return Ok(None);
		};
		let Some(ids) = self.v1.ids(&principal)? else {
			return Ok(None);
		};
		Ok(ids.get(&id).map(|blob| ArtifactRecord {
			digest:   Str::new(blob.to_hex().as_str()),
			lifetime: ArtifactLifetime::Durable,
		}))
	}
}

impl ArtifactCatalog for DigestCatalog {
	fn by_ordinal(
		&self,
		ordinal: u64,
	) -> impl Future<Output = Result<Option<ArtifactRecord>, Fault>> + Send + '_ {
		std::future::ready(self.v1_record(ordinal))
	}

	async fn by_digest<'a>(&'a self, digest: &'a str) -> Result<Option<ArtifactRecord>, Fault> {
		match self.blobs.reference(digest) {
			Ok(_) => Ok(Some(ArtifactRecord {
				digest:   Str::new(digest),
				lifetime: ArtifactLifetime::Durable,
			})),
			Err(Fault::Source { .. }) => Ok(None),
			Err(error) => Err(error),
		}
	}
}

impl BlobAuthority for BlobStoreAuthority {
	async fn stat<'a>(&'a self, digest: &'a str) -> Result<BlobStat, Fault> {
		Ok(BlobStat { byte_len: self.reference(digest)?.size })
	}

	async fn read_range<'a>(
		&'a self,
		digest: &'a str,
		range: Range<u64>,
	) -> Result<CowBytes<'static>, Fault> {
		let reference = self.reference(digest)?;
		let bytes = self.store.get(&reference).map_err(storage_fault)?;
		let start = usize::try_from(range.start).map_err(|_| Fault::Invalid {
			message: Str::new_static("Artifact range exceeds host address limits."),
		})?;
		let end = usize::try_from(range.end).map_err(|_| Fault::Invalid {
			message: Str::new_static("Artifact range exceeds host address limits."),
		})?;
		if start > end || end > bytes.len() {
			return Err(Fault::Invalid {
				message: Str::new_static("Artifact range exceeds stored content."),
			});
		}
		Ok(CowBytes::from(bytes.slice(start..end)))
	}
}

/// An omp v1 artifact id (`artifact://3`), which only a session imported
/// from v1 resolves.
fn v1_id(resource: &str) -> Option<u64> {
	let id = resource.trim_matches('/');
	(!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
		.then(|| id.parse().ok())
		.flatten()
}

/// Production artifact resolver for durable `artifact://sha256/<digest>` data
/// and, inside a session imported from omp v1, v1's `artifact://<id>`.
pub(crate) struct ArtifactUrlResolver {
	inner:   ArtifactResolver<DigestCatalog, BlobStoreAuthority>,
	catalog: DigestCatalog,
	blobs:   BlobStoreAuthority,
}

impl ArtifactUrlResolver {
	/// Resolves digests in `store`, and v1 ids through the imported journals
	/// in `sessions_dir`.
	pub(super) fn open(store: BlobStore, sessions_dir: PathBuf) -> Self {
		let blobs = BlobStoreAuthority { store };
		let catalog = DigestCatalog {
			blobs: blobs.clone(),
			v1:    Arc::new(V1Ids { sessions_dir, by_principal: Mutex::default() }),
		};
		Self { inner: ArtifactResolver::new(catalog.clone(), blobs.clone()), catalog, blobs }
	}

	fn digest<'a>(&self, resource: &'a str) -> Result<&'a str, Fault> {
		let digest = resource.strip_prefix("sha256/").unwrap_or(resource);
		if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
			Ok(digest)
		} else {
			Err(Fault::Invalid {
				message: Str::new_static(
					"Artifact addresses must be artifact://sha256/<64-hex-digest> (or, in a session \
					 imported from omp v1, v1's artifact://<id>).",
				),
			})
		}
	}
}

impl Resolve for ArtifactUrlResolver {
	async fn read<'a>(
		&'a self,
		resource: &'a str,
		selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		if v1_id(resource).is_some() {
			// The inner resolver maps the id through the session's imported
			// journal and bounds unselected reads of large artifacts itself.
			return self.inner.read(resource.trim_matches('/'), selector).await;
		}
		let digest = self.digest(resource)?;
		let reference = self.blobs.reference(digest)?;
		if reference.size > MAX_INLINE_BYTES
			&& matches!(selector, ParsedSelector::None | ParsedSelector::Raw)
		{
			return Err(Fault::Invalid {
				message: Str::new(format!(
					"Artifact {digest} is {} bytes; use a line selector or path-only mode.",
					reference.size
				)),
			});
		}
		self.inner.read(digest, selector).await
	}

	async fn list(
		&self,
		resource: &str,
		_max_entries: usize,
		_max_bytes: usize,
	) -> Result<ResourceList, Fault> {
		if resource.trim_matches('/').is_empty() {
			Ok(ResourceList { entries: Vec::new(), truncated: false })
		} else {
			Err(Fault::Invalid {
				message: Str::new_static(
					"Content-addressed artifacts do not expose a directory listing.",
				),
			})
		}
	}

	async fn path(&self, resource: &str) -> Result<Option<Str>, Fault> {
		let reference = match v1_id(resource) {
			Some(id) => {
				let record = self
					.catalog
					.v1_record(id)?
					.ok_or_else(|| Fault::Source { message: sf!("Artifact '{id}' not found") })?;
				self.blobs.reference(&record.digest)?
			},
			None => self.blobs.reference(self.digest(resource)?)?,
		};
		let url =
			Url::from_file_path(self.blobs.store.path(&reference)).map_err(|()| Fault::Invalid {
				message: Str::new_static("Artifact path cannot be represented as a file URI."),
			})?;
		Ok(Some(Str::new(url.as_str())))
	}

	async fn complete(
		&self,
		_query: &str,
		_max_results: usize,
	) -> Result<Vec<ResourceCompletion>, Fault> {
		Ok(Vec::new())
	}
}

impl fmt::Debug for ArtifactUrlResolver {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("ArtifactUrlResolver(..)")
	}
}

fn storage_fault(error: impl fmt::Display) -> Fault {
	Fault::Source { message: Str::new(format!("Artifact storage failed: {error}")) }
}

fn io_fault(source: io::Error) -> Fault {
	Fault::Source { message: Str::new(format!("Artifact storage I/O failed: {source}")) }
}

#[cfg(test)]
mod tests {
	use omp_dom::{Op, PropKey, Txn, Value};
	use omp_session::{
		ComponentRegistry, Session,
		import::{IMPORT_FORMAT, OMP1_FORMAT, foreign_artifact},
	};
	use omp_tools::read::selector::parse_selector;

	use super::*;
	use crate::tools::with_invocation_session_scope;

	/// The invocation principal the kernel's dispatcher sends for a journal.
	fn principal(journal: &Path) -> Str {
		Str::new(
			Hash32::sum(journal.as_os_str().as_encoded_bytes())
				.to_hex()
				.as_str(),
		)
	}

	/// A journal imported from `format` whose `<meta>` names `artifacts` as
	/// the v1 importer does.
	fn journal(
		path: &Path,
		format: Option<&'static str>,
		artifacts: &[(&'static str, BlobRef, Option<u64>)],
	) {
		let mut session = Session::create(path, ComponentRegistry::standard()).expect("journal");
		if let Some(format) = format {
			let meta = session.dom().meta();
			let cause = session.head().expect("genesis");
			session
				.patch(Txn {
					cause,
					label: None,
					ops: vec![Op::Set {
						h:     meta,
						prop:  PropKey::Custom(Str::new_static(IMPORT_FORMAT)),
						value: Value::Str(Str::new_static(format)),
					}],
				})
				.expect("provenance");
		}
		for (name, blob, id) in artifacts {
			let meta = session.dom().meta();
			let cause = session.head().expect("head");
			session
				.patch(Txn {
					cause,
					label: None,
					ops: vec![Op::Ins {
						parent: meta,
						after:  session.dom().children(meta).last().copied(),
						node:   foreign_artifact(
							Str::new_static(name),
							*blob,
							Str::new_static("text/plain"),
							*id,
						),
					}],
				})
				.expect("artifact");
		}
	}

	#[tokio::test]
	async fn v1_ids_resolve_through_the_invoking_sessions_imported_journal() {
		let root = tempfile::tempdir().expect("state dir");
		let store =
			BlobStore::open(omp_env::project_state::blob_store(root.path())).expect("blob store");
		let bytes = b"first line\nsecond line\n";
		let spilled = store.put(bytes).expect("copied v1 artifact");
		let notes = store.put(b"agent notes").expect("unnumbered artifact");
		let sessions = root.path().join("sessions");
		let imported = sessions.join("01JIMPORTED.oms");
		journal(&imported, Some(OMP1_FORMAT), &[
			("3.bash.log", spilled, Some(3)),
			("0-Scout.md", notes, None),
		]);
		let native = sessions.join("01JNATIVE.oms");
		journal(&native, None, &[]);
		// Only a v1 import's mapping counts.
		let claude = sessions.join("01JCLAUDE.oms");
		journal(&claude, Some("claude"), &[("3.bash.log", spilled, Some(3))]);
		let resolver = ArtifactUrlResolver::open(store, sessions);
		let none = parse_selector(None).expect("no selector");

		let read =
			with_invocation_session_scope(Some(principal(&imported)), resolver.read("3", &none))
				.await
				.expect("v1 id resolves in its imported session");
		assert_eq!(read.as_ref(), bytes);
		// Line selectors work on the mapped bytes (`artifact://3:2`).
		let lines = parse_selector(Some("2")).expect("line selector");
		let numbered =
			with_invocation_session_scope(Some(principal(&imported)), resolver.read("3", &lines))
				.await
				.expect("line selection");
		assert!(String::from_utf8_lossy(&numbered).contains("2:second line"), "{numbered:?}");
		let path = with_invocation_session_scope(Some(principal(&imported)), resolver.path("3"))
			.await
			.expect("path")
			.expect("a file");
		assert!(path.ends_with(spilled.to_hex().as_str()), "{path}");
		// The digest form is unchanged, in any session.
		let digest = spilled.to_hex();
		let by_digest = resolver
			.read(digest.as_str(), &none)
			.await
			.expect("digest address");
		assert_eq!(by_digest.as_ref(), bytes);

		// An id means nothing outside the session that imported it, or with no
		// such file, or without a session principal.
		for (session, id) in [
			(Some(principal(&native)), "3"),
			(Some(principal(&claude)), "3"),
			(Some(principal(&imported)), "4"),
			(None, "3"),
		] {
			let error = with_invocation_session_scope(session, resolver.read(id, &none))
				.await
				.expect_err("unresolved v1 id");
			assert!(matches!(error, Fault::Source { .. }), "{error:?}");
		}
	}
}
