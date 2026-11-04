//! Authenticated actor identity and extension provenance carried by durable
//! records.

use std::{
	cmp,
	fmt::{self, Display},
	hash::{Hash, Hasher},
	str::FromStr,
	sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::{Hash32, Str, hex};

/// Authorization tier derived from a timing-safe collaboration credential
/// check.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::EnumString, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum CredentialTier {
	/// The peer may observe public collaboration state but cannot mutate it.
	ReadOnly,
	/// The peer presented the room's writable credential.
	FullAccess,
}

/// The credential that admitted a remote peer.
///
/// Neither the room credential nor a device secret is ever retained: a room
/// peer carries an audit-linkage digest of the write token it presented, and
/// a device peer the id of the enrolled key that signed its challenge.
#[derive(Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
	/// A collaboration room guest.
	Room {
		/// Stable room identifier.
		room_id:      Str,
		/// Digest of the accepted write token, absent for read-only peers.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		token_digest: Option<Hash32>,
	},
	/// An enrolled device.
	Device {
		/// Identifier of the enrolled key that signed the device's challenge.
		key_id: Str,
	},
}

impl fmt::Debug for Origin {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Room { token_digest, .. } => formatter
				.debug_struct("Room")
				.field("room_id", &"[redacted]")
				.field("token_digest", &token_digest.map(|_| "[redacted]"))
				.finish(),
			Self::Device { .. } => formatter
				.debug_struct("Device")
				.field("key_id", &"[redacted]")
				.finish(),
		}
	}
}

/// Authenticated remote peer, a collaboration guest or an enrolled device,
/// stamped onto every admitted remote mutation.
///
/// The peer's [`Origin`] names the credential that admitted it; the
/// credential itself is never retained. The handle is Arc-backed because it
/// crosses the Core, Environment, approval, and journal boundaries with every
/// remote operation.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct RemotePrincipal(Arc<RemotePrincipalData>);

#[derive(Eq, Hash, PartialEq)]
struct RemotePrincipalData {
	peer_id:         u32,
	display_name:    Str,
	credential_tier: CredentialTier,
	origin:          Origin,
}

const _: () =
	assert!(std::mem::size_of::<RemotePrincipal>() <= 16, "RemotePrincipal must remain clone-cheap");

impl RemotePrincipal {
	/// Creates a principal from host-authenticated relay and credential facts.
	pub fn new(
		peer_id: u32,
		display_name: Str,
		credential_tier: CredentialTier,
		origin: Origin,
	) -> Self {
		Self(Arc::new(RemotePrincipalData { peer_id, display_name, credential_tier, origin }))
	}

	/// Returns the relay-assigned peer identifier.
	pub fn peer_id(&self) -> u32 {
		self.0.peer_id
	}

	/// Returns the sanitized, human-readable peer name.
	pub fn display_name(&self) -> &str {
		self.0.display_name.as_str()
	}

	/// Returns the host-verified collaboration credential tier.
	pub fn credential_tier(&self) -> CredentialTier {
		self.0.credential_tier
	}

	/// Returns the credential that admitted this peer.
	pub fn origin(&self) -> &Origin {
		&self.0.origin
	}

	/// Returns whether this peer may submit host mutations.
	pub fn may_mutate(&self) -> bool {
		matches!(self.0.credential_tier, CredentialTier::FullAccess)
	}
}

impl fmt::Debug for RemotePrincipal {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("RemotePrincipal")
			.field("peer_id", &self.0.peer_id)
			.field("display_name", &self.0.display_name)
			.field("credential_tier", &self.0.credential_tier)
			.field("origin", &self.0.origin)
			.finish()
	}
}

/// The authenticated person acting through an omp daemon.
///
/// A principal is derived by the core from the authenticated connection. Its
/// identifier is intentionally redacted from [`Debug`] output so durable error
/// paths cannot accidentally disclose account identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Principal {
	id:      Str,
	display: Str,
}

impl Principal {
	/// Creates an authenticated principal from its stable identifier and safe
	/// human-readable display name.
	pub const fn new(id: Str, display: Str) -> Self {
		Self { id, display }
	}

	/// Returns the stable principal identifier.
	pub fn id(&self) -> &str {
		self.id.as_str()
	}

	/// Returns the human-readable principal name intended for UI surfaces.
	pub fn display(&self) -> &str {
		self.display.as_str()
	}
}

impl fmt::Debug for Principal {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Principal")
			.field("id", &"[redacted]")
			.field("display", &self.display)
			.finish()
	}
}

/// A SHA-256 digest identifying the exact extension artifact that acted.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactDigest([u8; 32]);

impl ArtifactDigest {
	/// Creates an artifact digest from its raw SHA-256 bytes.
	pub const fn new(bytes: [u8; 32]) -> Self {
		Self(bytes)
	}

	/// Returns the raw SHA-256 digest bytes.
	pub const fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}

	/// Consumes the digest and returns its raw bytes.
	pub const fn into_bytes(self) -> [u8; 32] {
		self.0
	}
}

impl Display for ArtifactDigest {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("sha256:")?;
		Display::fmt(&hex::encode(&self.0), formatter)
	}
}

impl fmt::Debug for ArtifactDigest {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		Display::fmt(self, formatter)
	}
}

/// Failure to parse an extension artifact digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactDigestError {
	/// The digest did not use the canonical `sha256:` prefix.
	#[error("artifact digest must start with `sha256:`")]
	MissingPrefix,
	/// The hexadecimal payload was not exactly 32 bytes.
	#[error("artifact digest must contain exactly 64 lowercase hexadecimal digits")]
	InvalidLength,
	/// The hexadecimal payload was not canonical lowercase hexadecimal.
	#[error("artifact digest contains a non-lowercase-hexadecimal character")]
	InvalidHex,
}

impl FromStr for ArtifactDigest {
	type Err = ArtifactDigestError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let encoded = value
			.strip_prefix("sha256:")
			.ok_or(ArtifactDigestError::MissingPrefix)?;
		if encoded.len() != 64 {
			return Err(ArtifactDigestError::InvalidLength);
		}
		if !encoded
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
		{
			return Err(ArtifactDigestError::InvalidHex);
		}
		let bytes = <[u8; 32]>::try_from(hex::decode(encoded.as_bytes()))
			.map_err(|_| ArtifactDigestError::InvalidHex)?;
		Ok(Self(bytes))
	}
}

impl Serialize for ArtifactDigest {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for ArtifactDigest {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = Str::deserialize(deserializer)?;
		value.as_str().parse().map_err(de::Error::custom)
	}
}

/// Core-stamped identity of the exact extension incarnation that acted.
///
/// The seven fields are the publisher, extension id, version, artifact digest,
/// installation layer, trust tier, and host generation. Workers may observe
/// this value but must never be trusted to author it.
#[derive(Clone)]
pub struct Provenance(Arc<ProvenanceData>);

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
struct ProvenanceData {
	publisher:       Str,
	extension_id:    Str,
	version:         Str,
	artifact_digest: ArtifactDigest,
	layer:           Str,
	tier:            Str,
	generation:      u64,
}

const _: () = assert!(std::mem::size_of::<Provenance>() <= 16, "Provenance must stay compact");

impl Provenance {
	/// Creates provenance from core-authenticated extension installation facts.
	pub fn new(
		publisher: Str,
		extension_id: Str,
		version: Str,
		artifact_digest: ArtifactDigest,
		layer: Str,
		tier: Str,
		generation: u64,
	) -> Self {
		Self(Arc::new(ProvenanceData {
			publisher,
			extension_id,
			version,
			artifact_digest,
			layer,
			tier,
			generation,
		}))
	}

	/// Returns the publisher key fingerprint.
	pub fn publisher(&self) -> &str {
		self.0.publisher.as_str()
	}

	/// Returns the dotted extension identifier.
	pub fn extension_id(&self) -> &str {
		self.0.extension_id.as_str()
	}

	/// Returns the exact extension version.
	pub fn version(&self) -> &str {
		self.0.version.as_str()
	}

	/// Returns the exact extension artifact digest.
	pub fn artifact_digest(&self) -> ArtifactDigest {
		self.0.artifact_digest
	}

	/// Returns the installation layer.
	pub fn layer(&self) -> &str {
		self.0.layer.as_str()
	}

	/// Returns the conferred trust tier.
	pub fn tier(&self) -> &str {
		self.0.tier.as_str()
	}

	/// Returns the host incarnation generation.
	pub fn generation(&self) -> u64 {
		self.0.generation
	}
}

impl fmt::Debug for Provenance {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Provenance")
			.field("publisher", &self.0.publisher)
			.field("extension_id", &self.0.extension_id)
			.field("version", &self.0.version)
			.field("artifact_digest", &self.0.artifact_digest)
			.field("layer", &self.0.layer)
			.field("tier", &self.0.tier)
			.field("generation", &self.0.generation)
			.finish()
	}
}

impl PartialEq for Provenance {
	fn eq(&self, other: &Self) -> bool {
		self.0 == other.0
	}
}

impl Eq for Provenance {}

impl PartialOrd for Provenance {
	fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Provenance {
	fn cmp(&self, other: &Self) -> cmp::Ordering {
		self.0.cmp(&other.0)
	}
}

impl Hash for Provenance {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.0.hash(state);
	}
}

impl Serialize for Provenance {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		self.0.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Provenance {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		ProvenanceData::deserialize(deserializer).map(|data| Self(Arc::new(data)))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn room(token_digest: Option<Hash32>) -> Origin {
		Origin::Room { room_id: Str::new_static("room-1"), token_digest }
	}

	fn device() -> Origin {
		Origin::Device { key_id: Str::new_static("key-9") }
	}

	fn admit(origin: Origin) -> RemotePrincipal {
		RemotePrincipal::new(7, Str::new_static("guest"), CredentialTier::FullAccess, origin)
	}

	#[test]
	fn room_origin_round_trips_with_its_token_digest() {
		let origin = room(Some(Hash32::new([0xab; 32])));
		let json = serde_json::to_string(&origin).unwrap();
		assert_eq!(
			json,
			format!(r#"{{"kind":"room","room_id":"room-1","token_digest":"{}"}}"#, "ab".repeat(32))
		);
		assert_eq!(serde_json::from_str::<Origin>(&json).unwrap(), origin);
	}

	#[test]
	fn read_only_room_origin_omits_the_digest() {
		let origin = room(None);
		let json = serde_json::to_string(&origin).unwrap();
		assert_eq!(json, r#"{"kind":"room","room_id":"room-1"}"#);
		assert_eq!(serde_json::from_str::<Origin>(&json).unwrap(), origin);
	}

	#[test]
	fn device_origin_round_trips() {
		let json = serde_json::to_string(&device()).unwrap();
		assert_eq!(json, r#"{"kind":"device","key_id":"key-9"}"#);
		assert_eq!(serde_json::from_str::<Origin>(&json).unwrap(), device());
	}

	#[test]
	fn origin_refuses_unknown_kinds_and_missing_fields() {
		assert!(serde_json::from_str::<Origin>(r#"{"kind":"bridge","room_id":"r"}"#).is_err());
		assert!(serde_json::from_str::<Origin>(r#"{"kind":"room"}"#).is_err());
		assert!(serde_json::from_str::<Origin>(r#"{"kind":"device"}"#).is_err());
	}

	#[test]
	fn a_principal_reports_the_origin_that_admitted_it() {
		for origin in [room(Some(Hash32::new([1; 32]))), room(None), device()] {
			let principal = admit(origin.clone());
			assert_eq!(principal.origin(), &origin);
			assert_eq!(principal.clone().origin(), &origin);
			assert_eq!(principal.peer_id(), 7);
			assert!(principal.may_mutate());
		}
	}

	#[test]
	fn principals_with_different_origins_are_different_peers() {
		assert_eq!(admit(room(None)), admit(room(None)));
		assert_ne!(admit(room(None)), admit(room(Some(Hash32::new([1; 32])))));
		assert_ne!(admit(room(None)), admit(device()));
	}

	#[test]
	fn debug_never_discloses_the_origin() {
		let shown = format!("{:?} {:?}", admit(room(Some(Hash32::new([0xcd; 32])))), device());
		for secret in ["room-1", "cdcd", "key-9"] {
			assert!(!shown.contains(secret), "{secret} leaked: {shown}");
		}
	}

	#[test]
	fn the_handle_stays_clone_cheap() {
		assert!(std::mem::size_of::<RemotePrincipal>() <= 16);
	}
}
