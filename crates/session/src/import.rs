//! Journal vocabulary of sessions imported from foreign transcripts, and the
//! pure projections that read it back from journal entries.
//!
//! An importer records where a journal came from as `<meta>` properties in
//! the journal's first patches ([`IMPORT_FORMAT`], [`IMPORT_SOURCE`],
//! [`IMPORT_SOURCE_ID`], …), before any transcript entry. It records every
//! file it retained from the source's artifact directory as a
//! `<foreign-artifact>` child of `<meta>` ([`foreign_artifact`]) naming the
//! file's copy in the project blob store; an omp v1 artifact also carries the
//! numeric id v1 addressed it by (`artifact://<id>`, [`V1_ARTIFACT`]).
//!
//! Nothing here is a second store: the journal is the only record, and
//! [`import_origin`] and [`v1_artifacts`] fold it on demand. Journaled text is
//! never rewritten; a v1 `artifact://<id>` stays as v1 wrote it and resolves
//! through [`v1_artifacts`].

use omp_core::{Hash32, Str};
use omp_dom::{Dom, NodeSpec, Op, PropId, PropKey, Tag, Value};
use omp_journal::{Entry, Kind, KindName, blob::BlobRef, data::Patch};

/// `<meta>` property: the importer's source format (`claude`, `codex`,
/// [`OMP1_FORMAT`]).
pub const IMPORT_FORMAT: &str = "import-format";
/// The [`IMPORT_FORMAT`] of a journal imported from an omp v1 transcript.
pub const OMP1_FORMAT: &str = "omp1";
/// Journal bytes that hold an imported journal's provenance: importers write
/// it before any transcript entry, so [`import_origin`] over
/// [`omp_journal::Journal::scan_prefix`] of this many bytes finds it.
pub const PROVENANCE_PREFIX_BYTES: u64 = 64 * 1024;
/// `<meta>` property: the source transcript's path when it was imported.
pub const IMPORT_SOURCE: &str = "import-source";
/// `<meta>` property: the source's own session id.
pub const IMPORT_SOURCE_ID: &str = "import-source-id";
/// `<meta>` property: the source's recorded working directory.
pub const IMPORT_SOURCE_CWD: &str = "import-source-cwd";
/// `<meta>` property: `artifact://sha256/<hex>` of the source transcript's
/// exact bytes at import.
///
/// The hex is the bytes' SHA-256 digest ([`omp_core::Hash32::sum`]), which an
/// importer compares a later read of the source against to tell that the
/// source changed since ([`ImportOrigin::source_digest`]).
pub const IMPORT_SOURCE_BLOB: &str = "import-source-blob";
/// `<meta>` child naming one retained source artifact.
pub const FOREIGN_ARTIFACT_TAG: &str = "foreign-artifact";
/// `<foreign-artifact>` (and `<foreign-import>`) property: the byte length
/// of the retained copy.
pub const ARTIFACT_SIZE: &str = "size";
/// `<foreign-artifact>` property: the numeric id omp v1 resolved
/// `artifact://<id>` to.
pub const V1_ARTIFACT: &str = "v1-artifact";

/// Content-addressed artifact URI prefix.
const ARTIFACT_PREFIX: &str = "artifact://sha256/";

/// Where an imported journal came from, as its first patches recorded it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportOrigin {
	/// Source format ([`IMPORT_FORMAT`]).
	pub format:        Str,
	/// Source transcript path ([`IMPORT_SOURCE`]).
	pub source:        Option<Str>,
	/// Source session id ([`IMPORT_SOURCE_ID`]).
	pub source_id:     Option<Str>,
	/// Digest of the source's exact bytes at import ([`IMPORT_SOURCE_BLOB`]),
	/// when recorded as an `artifact://sha256/<hex>` address.
	pub source_digest: Option<Hash32>,
}

/// Decodes the DOM operations of one `patch@1` entry; other kinds and
/// undecodable payloads yield none.
fn patch_ops(entry: &Entry) -> Option<Vec<Op>> {
	if entry.kind != Kind::known(KindName::Patch) {
		return None;
	}
	let payload = serde_json::from_str::<Patch>(entry.data.as_str()).ok()?;
	serde_json::from_str::<Vec<Op>>(payload.ops.get()).ok()
}

/// The import provenance `entries` record on `<meta>`, or `None` for a
/// journal no importer wrote.
///
/// Importers write provenance before any transcript entry, so a journal
/// prefix of [`PROVENANCE_PREFIX_BYTES`] is enough.
#[must_use]
pub fn import_origin(entries: &[Entry]) -> Option<ImportOrigin> {
	let meta = Dom::new().meta();
	let mut origin = ImportOrigin::default();
	for op in entries.iter().filter_map(patch_ops).flatten() {
		let Op::Set { h, prop: PropKey::Custom(name), value: Value::Str(value) } = op else {
			continue;
		};
		if h != meta {
			continue;
		}
		match name.as_str() {
			IMPORT_FORMAT => origin.format = value,
			IMPORT_SOURCE => origin.source = Some(value),
			IMPORT_SOURCE_ID => origin.source_id = Some(value),
			IMPORT_SOURCE_BLOB => {
				origin.source_digest = value
					.strip_prefix(ARTIFACT_PREFIX)
					.and_then(|hex| hex.parse().ok());
			},
			_ => {},
		}
	}
	(!origin.format.is_empty()).then_some(origin)
}

/// A `<foreign-artifact>` node naming one retained source file.
///
/// It records `name` in the source's artifact directory, its copy `blob` in
/// the project blob store (as `artifact://sha256/<hex>`), and, for an omp v1
/// artifact, the id v1 addressed it by.
#[must_use]
pub fn foreign_artifact(name: Str, blob: BlobRef, mime: Str, v1_id: Option<u64>) -> NodeSpec {
	let mut address = String::with_capacity(ARTIFACT_PREFIX.len() + 64);
	address.push_str(ARTIFACT_PREFIX);
	address.push_str(blob.to_hex().as_str());
	let node = NodeSpec::new(Tag::Custom(Str::new_static(FOREIGN_ARTIFACT_TAG)))
		.with_prop(PropId::Name, Value::Str(name))
		.with_prop(PropId::Blob, Value::Str(Str::new(address)))
		.with_prop(PropId::Mime, Value::Str(mime))
		.with_prop(
			PropKey::Custom(Str::new_static(ARTIFACT_SIZE)),
			Value::Int(i64::try_from(blob.size).unwrap_or(i64::MAX)),
		);
	match v1_id.and_then(|id| i64::try_from(id).ok()) {
		Some(id) => node.with_prop(PropKey::Custom(Str::new_static(V1_ARTIFACT)), Value::Int(id)),
		None => node,
	}
}

fn prop<'n>(node: &'n NodeSpec, key: &PropKey) -> Option<&'n Value> {
	node
		.props
		.iter()
		.find_map(|(candidate, value)| (candidate == key).then_some(value))
}

/// The v1 id and project-store blob of one `<foreign-artifact>` insertion.
fn v1_artifact(op: Op, meta: omp_dom::Handle) -> Option<(u64, BlobRef)> {
	let Op::Ins { parent, node, .. } = op else {
		return None;
	};
	if parent != meta || !matches!(&node.tag, Tag::Custom(tag) if tag == FOREIGN_ARTIFACT_TAG) {
		return None;
	}
	let Some(Value::Int(id)) = prop(&node, &PropKey::Custom(Str::new_static(V1_ARTIFACT))) else {
		return None;
	};
	let Some(Value::Int(size)) = prop(&node, &PropKey::Custom(Str::new_static(ARTIFACT_SIZE)))
	else {
		return None;
	};
	let hex = prop(&node, &PropId::Blob.into())?
		.as_str()?
		.strip_prefix(ARTIFACT_PREFIX)?;
	let blob = BlobRef::parse_hex(hex, u64::try_from(*size).ok()?).ok()?;
	Some((u64::try_from(*id).ok()?, blob))
}

/// Every omp v1 artifact an imported journal retained, as (v1 id, project
/// blob) in journal order: what `artifact://<id>` means inside that session.
///
/// Every entry is read, abandoned branches included: the mapping is a fact
/// about the import, not about the live branch.
pub fn v1_artifacts(entries: &[Entry]) -> impl Iterator<Item = (u64, BlobRef)> + '_ {
	let meta = Dom::new().meta();
	entries
		.iter()
		.filter_map(patch_ops)
		.flat_map(move |ops| ops.into_iter().filter_map(move |op| v1_artifact(op, meta)))
}

#[cfg(test)]
mod tests {
	use omp_core::Hash32;
	use omp_dom::Txn;

	use super::*;
	use crate::{ComponentRegistry, Session};

	fn blob(bytes: &[u8]) -> BlobRef {
		BlobRef { hash: Hash32::sum(bytes), size: bytes.len() as u64 }
	}

	#[test]
	fn origin_and_v1_artifacts_fold_back_from_the_journal() {
		let directory = tempfile::tempdir().expect("scratch");
		let path = directory.path().join("imported.oms");
		let mut session = Session::create(&path, ComponentRegistry::standard()).expect("create");
		let meta = session.dom().meta();
		let cause = session.head().expect("genesis");
		let set = |prop: &'static str, value: &str| Op::Set {
			h:     meta,
			prop:  PropKey::Custom(Str::new_static(prop)),
			value: Value::Str(Str::new(value)),
		};
		let source = blob(b"{\"type\":\"session\"}\n");
		let mut address = String::from(ARTIFACT_PREFIX);
		address.push_str(source.to_hex().as_str());
		session
			.patch(Txn {
				cause,
				label: None,
				ops: vec![
					set(IMPORT_FORMAT, "omp1"),
					set(IMPORT_SOURCE, "/v1/sessions/a.jsonl"),
					set(IMPORT_SOURCE_ID, "v1-session"),
					set(IMPORT_SOURCE_BLOB, &address),
				],
			})
			.expect("provenance");
		for (name, bytes, id) in [
			("3.bash.log", b"three".as_slice(), Some(3)),
			("0-Scout.md", b"agent output".as_slice(), None),
			("7.read.log", b"seven".as_slice(), Some(7)),
		] {
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
							blob(bytes),
							Str::new_static("text/plain"),
							id,
						),
					}],
				})
				.expect("artifact");
		}
		drop(session);

		let entries = omp_journal::Journal::scan(&path).expect("scan");
		assert_eq!(
			import_origin(&entries),
			Some(ImportOrigin {
				format:        Str::new_static("omp1"),
				source:        Some(Str::new_static("/v1/sessions/a.jsonl")),
				source_id:     Some(Str::new_static("v1-session")),
				source_digest: Some(source.hash),
			})
		);
		assert_eq!(v1_artifacts(&entries).collect::<Vec<_>>(), [
			(3, blob(b"three")),
			(7, blob(b"seven"))
		]);
		// An address that is no `artifact://sha256/<hex>` records no digest.
		let mut odd =
			Session::create(directory.path().join("odd.oms"), ComponentRegistry::standard())
				.expect("create");
		let cause = odd.head().expect("genesis");
		odd.patch(Txn {
			cause,
			label: None,
			ops: vec![set(IMPORT_FORMAT, "omp1"), set(IMPORT_SOURCE_BLOB, "blob:sha256:zz")],
		})
		.expect("provenance");
		drop(odd);
		let odd = omp_journal::Journal::scan(directory.path().join("odd.oms")).expect("scan");
		assert_eq!(import_origin(&odd).and_then(|origin| origin.source_digest), None);
		// The genesis alone is no import.
		assert_eq!(import_origin(&entries[..1]), None);
		assert_eq!(v1_artifacts(&entries[..1]).count(), 0);
	}
}
