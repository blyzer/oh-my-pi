//! Host-held credential placement grants (ADR 0075 D3).
//!
//! A [`CredentialGrant`] is an operator record in `<data>/ext/grants.toml`. It
//! names one extension identity, one exact provider id, the canonical
//! `(https, host, port)` origins the credential may reach, and the operations
//! (method and path prefix) that may carry it. An extension manifest never
//! creates or widens one: scopes come from this record only.
//!
//! Nothing here places a credential. [`CredentialGrants::authorize`] is a pure
//! decision over a request's provider, method and URL, so the egress host can
//! ask it again for every hop.

use std::sync::Arc;

use omp_core::{Str, sf};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::Layer;

/// The port an `https` origin has when it names none.
const HTTPS_PORT: u16 = 443;

const fn https_port() -> u16 {
	HTTPS_PORT
}

/// Longest provider id a grant may name.
const PROVIDER_MAX: usize = 64;

/// Why a grant record is malformed. Every class refuses the record; none is
/// ever repaired.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum CredentialGrantError {
	/// The extension id, publisher or digest is empty.
	#[error("a credential grant must bind an extension id, publisher and digest")]
	Identity,
	/// Only a user-layer extension may hold a grant (ADR 0075 D13).
	#[error("only a user-layer extension may hold a credential grant")]
	Layer,
	/// The provider is empty, a glob, or not a plain provider id.
	#[error("a credential grant must name one exact provider id")]
	Provider,
	/// The grant names no origin.
	#[error("a credential grant must name at least one origin")]
	NoOrigins,
	/// An origin is not an exact canonical `https` host and port.
	#[error("a credential grant origin must be an exact https host and port")]
	Origin,
	/// The grant names no operation.
	#[error("a credential grant must name at least one operation")]
	NoOperations,
	/// An operation is not an exact method and path prefix.
	#[error("a credential grant operation must be an exact method and path prefix")]
	Operation,
}

/// Why the host refuses to place a credential for a request. The class is
/// all the extension learns; it never carries a secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum CredentialRefusal {
	/// The extension holds no credential grant at all.
	#[error("the extension holds no credential grant")]
	NoGrant,
	/// The extension holds grants, none for this provider.
	#[error("the extension holds no credential grant for this provider")]
	ProviderNotGranted,
	/// The URL is not an https URL.
	#[error("the request URL is not an https URL")]
	NotHttps,
	/// The scheme, host and port are not in the grant.
	#[error("the credential grant does not cover this scheme, host and port")]
	OriginNotGranted,
	/// The method and path are not in the grant.
	#[error("the credential grant does not cover this method and path")]
	OperationNotGranted,
}

/// One canonical origin a credential may reach.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CredentialOrigin {
	/// Always `https`.
	pub scheme: Str,
	/// Lowercase ASCII host with IDNA applied.
	pub host:   Str,
	/// Effective port; 443 when the record names none.
	#[serde(default = "https_port")]
	pub port:   u16,
}

impl CredentialOrigin {
	/// Canonicalizes an operator-supplied origin. A second port on the same
	/// host is a different origin.
	///
	/// # Errors
	///
	/// [`CredentialGrantError::Origin`] for any non-https scheme, a port of 0,
	/// or a host that is empty, carries a port, userinfo, a path or a glob, or
	/// does not parse as a host.
	pub fn new(scheme: &str, host: &str, port: Option<u16>) -> Result<Self, CredentialGrantError> {
		if !scheme.eq_ignore_ascii_case("https") {
			return Err(CredentialGrantError::Origin);
		}
		let port = port.unwrap_or(HTTPS_PORT);
		if port == 0 {
			return Err(CredentialGrantError::Origin);
		}
		Ok(Self { scheme: sf!("https"), host: canonical_host(host)?, port })
	}

	/// Re-checks a record read from disk: it must already be canonical.
	fn validate(&self) -> Result<(), CredentialGrantError> {
		if self.scheme.as_str() != "https" || self.port == 0 {
			return Err(CredentialGrantError::Origin);
		}
		if canonical_host(self.host.as_str())? != self.host {
			return Err(CredentialGrantError::Origin);
		}
		Ok(())
	}

	fn covers(&self, url: &Url) -> bool {
		url.host_str() == Some(self.host.as_str()) && url.port_or_known_default() == Some(self.port)
	}
}

fn canonical_host(host: &str) -> Result<Str, CredentialGrantError> {
	let forbidden = |byte: u8| {
		byte.is_ascii_control()
			|| matches!(byte, b' ' | b'*' | b'?' | b'/' | b'\\' | b'@' | b'#' | b'%' | b'{' | b'}')
	};
	if host.is_empty()
		|| host.bytes().any(forbidden)
		|| (!host.starts_with('[') && host.contains(':'))
	{
		return Err(CredentialGrantError::Origin);
	}
	let url = Url::parse(&format!("https://{host}/")).map_err(|_| CredentialGrantError::Origin)?;
	if url.port().is_some()
		|| !url.username().is_empty()
		|| url.password().is_some()
		|| url.path() != "/"
		|| url.query().is_some()
		|| url.fragment().is_some()
	{
		return Err(CredentialGrantError::Origin);
	}
	url.host_str()
		.map(Str::new)
		.ok_or(CredentialGrantError::Origin)
}

/// One operation that may carry the credential: an exact method and a path
/// prefix that matches on a segment boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CredentialOperation {
	/// Uppercase HTTP method.
	pub method:      Str,
	/// Absolute path prefix, for example `/api/v1/Run`.
	pub path_prefix: Str,
}

impl CredentialOperation {
	/// Validates an operator-supplied operation.
	///
	/// # Errors
	///
	/// [`CredentialGrantError::Operation`] for a method that is not uppercase
	/// ASCII letters, or a prefix that is not an absolute path made of plain
	/// segments (no glob, query, fragment, percent escape, backslash or dot
	/// segment).
	pub fn new(method: &str, path_prefix: &str) -> Result<Self, CredentialGrantError> {
		let operation = Self { method: Str::new(method), path_prefix: Str::new(path_prefix) };
		operation.validate()?;
		Ok(operation)
	}

	fn validate(&self) -> Result<(), CredentialGrantError> {
		let method = self.method.as_str();
		let prefix = self.path_prefix.as_str();
		let plain =
			|byte: u8| byte.is_ascii_graphic() && !matches!(byte, b'*' | b'?' | b'#' | b'\\' | b'%');
		if method.is_empty()
			|| method.len() > 16
			|| !method.bytes().all(|byte| byte.is_ascii_uppercase())
			|| !prefix.starts_with('/')
			|| !prefix.bytes().all(plain)
			|| prefix
				.split('/')
				.any(|segment| matches!(segment, "." | ".."))
		{
			return Err(CredentialGrantError::Operation);
		}
		Ok(())
	}

	fn covers(&self, method: &str, path: &str) -> bool {
		if self.method.as_str() != method || !path_is_unambiguous(path) {
			return false;
		}
		let prefix = self.path_prefix.as_str();
		path
			.strip_prefix(prefix)
			.is_some_and(|rest| prefix.ends_with('/') || rest.is_empty() || rest.starts_with('/'))
	}
}

/// A request path that a server could read differently from the host: an
/// escaped slash, backslash or dot, or a dot segment. Such a path never
/// matches a prefix.
fn path_is_unambiguous(path: &str) -> bool {
	let lower = path.to_ascii_lowercase();
	!path.contains('\\')
		&& !["%2e", "%2f", "%5c"]
			.iter()
			.any(|escape| lower.contains(escape))
		&& !path.split('/').any(|segment| matches!(segment, "." | ".."))
}

/// An operator's approval of one provider's credential for one extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CredentialGrant {
	/// Extension identity.
	pub id:                Str,
	/// TOFU-pinned publisher fingerprint; for a path extension, derived from
	/// its canonical root.
	pub publisher:         Str,
	/// Layer of the extension. Only [`Layer::Client`] may hold a grant.
	pub layer:             Layer,
	/// Digest of the declared capability set and the manifest text the
	/// operator saw. A changed manifest no longer matches.
	pub capability_digest: Str,
	/// Exact provider id.
	pub provider:          Str,
	/// Origins the credential may reach.
	#[serde(default)]
	pub origins:           Vec<CredentialOrigin>,
	/// Operations that may carry the credential.
	#[serde(default)]
	pub operations:        Vec<CredentialOperation>,
	/// RFC 3339 timestamp.
	pub granted_at:        Str,
	/// Operator channel.
	pub granted_by:        Str,
}

impl CredentialGrant {
	/// Checks every rule of the record. Records read from disk pass through
	/// here again before they are honoured, so a hand-edited glob or empty
	/// provider never holds.
	///
	/// # Errors
	///
	/// The first [`CredentialGrantError`] class that applies.
	pub fn validate(&self) -> Result<(), CredentialGrantError> {
		if self.id.is_empty() || self.publisher.is_empty() || self.capability_digest.is_empty() {
			return Err(CredentialGrantError::Identity);
		}
		if self.layer != Layer::Client {
			return Err(CredentialGrantError::Layer);
		}
		validate_provider(self.provider.as_str())?;
		if self.origins.is_empty() {
			return Err(CredentialGrantError::NoOrigins);
		}
		self
			.origins
			.iter()
			.try_for_each(CredentialOrigin::validate)?;
		if self.operations.is_empty() {
			return Err(CredentialGrantError::NoOperations);
		}
		self
			.operations
			.iter()
			.try_for_each(CredentialOperation::validate)
	}

	/// Whether this row belongs to the extension identity: all four fields
	/// match, and the layer is the user layer.
	#[must_use]
	pub fn binds(&self, id: &Str, publisher: &Str, layer: Layer, capability_digest: &Str) -> bool {
		layer == Layer::Client
			&& self.layer == layer
			&& self.id == *id
			&& self.publisher == *publisher
			&& self.capability_digest == *capability_digest
	}
}

/// A provider id is a plain lowercase name: never empty, never a glob.
fn validate_provider(provider: &str) -> Result<(), CredentialGrantError> {
	let mut bytes = provider.bytes();
	let first_ok = bytes
		.next()
		.is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
	if provider.len() > PROVIDER_MAX
		|| !first_ok
		|| !bytes.all(|byte| {
			byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
		}) {
		return Err(CredentialGrantError::Provider);
	}
	Ok(())
}

/// The validated, identity-bound grants one extension holds for its session.
///
/// Built only from host-held records. Rows that fail
/// [`CredentialGrant::validate`] are dropped, so an invalid row holds nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CredentialGrants(Arc<[CredentialGrant]>);

impl CredentialGrants {
	/// Keeps the valid rows of `rows`.
	pub fn new(rows: impl IntoIterator<Item = CredentialGrant>) -> Self {
		Self(
			rows
				.into_iter()
				.filter(|row| row.validate().is_ok())
				.collect(),
		)
	}

	/// Whether no grant is held.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// The granted rows.
	#[must_use]
	pub fn rows(&self) -> &[CredentialGrant] {
		&self.0
	}

	/// The distinct granted provider ids, in lexical order.
	#[must_use]
	pub fn providers(&self) -> Vec<Str> {
		let mut providers: Vec<Str> = self.0.iter().map(|row| row.provider.clone()).collect();
		providers.sort_unstable();
		providers.dedup();
		providers
	}

	/// Decides whether `provider`'s credential may be placed on a request.
	/// The decision depends only on the host-held rows and the URL, so it is
	/// asked again for every redirect hop.
	///
	/// # Errors
	///
	/// The [`CredentialRefusal`] class that applies. A request is refused
	/// unless one row covers its provider, origin and operation together.
	pub fn authorize(
		&self,
		provider: &str,
		method: &str,
		url: &Url,
	) -> Result<&CredentialGrant, CredentialRefusal> {
		if self.0.is_empty() {
			return Err(CredentialRefusal::NoGrant);
		}
		let mut rows = self
			.0
			.iter()
			.filter(|row| row.provider.as_str() == provider)
			.peekable();
		if rows.peek().is_none() {
			return Err(CredentialRefusal::ProviderNotGranted);
		}
		if url.scheme() != "https" {
			return Err(CredentialRefusal::NotHttps);
		}
		if !url.username().is_empty() || url.password().is_some() {
			return Err(CredentialRefusal::OriginNotGranted);
		}
		let mut origin_granted = false;
		for row in rows {
			if !row.origins.iter().any(|origin| origin.covers(url)) {
				continue;
			}
			origin_granted = true;
			if row
				.operations
				.iter()
				.any(|operation| operation.covers(method, url.path()))
			{
				return Ok(row);
			}
		}
		Err(if origin_granted {
			CredentialRefusal::OperationNotGranted
		} else {
			CredentialRefusal::OriginNotGranted
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn row(provider: &str) -> CredentialGrant {
		CredentialGrant {
			id:                sf!("omp.buildbuddy"),
			publisher:         sf!("path:/ext/buildbuddy"),
			layer:             Layer::Client,
			capability_digest: sf!("digest-1"),
			provider:          Str::new(provider),
			origins:           vec![
				CredentialOrigin::new("https", "remote.buildbuddy.io", None).expect("origin"),
			],
			operations:        vec![
				CredentialOperation::new("POST", "/api/v1/Run").expect("operation"),
			],
			granted_at:        sf!("2026-11-26T00:00:00Z"),
			granted_by:        sf!("test"),
		}
	}

	fn url(text: &str) -> Url {
		Url::parse(text).expect("url")
	}

	#[test]
	fn credential_grant_refuses_glob_and_empty_provider() {
		for provider in ["", "*", "build*", "?", "a b", "Buildbuddy", "-x", "a/b", "buildbuddy\n"] {
			assert_eq!(
				row(provider).validate(),
				Err(CredentialGrantError::Provider),
				"provider {provider:?} must be refused"
			);
		}
		assert_eq!(row("buildbuddy").validate(), Ok(()));
		// Globs and empty fields are refused on every other axis too.
		for host in ["*.buildbuddy.io", "", "a.io/path", "user@a.io", "a.io:8443", "a.io?x", "[*]"] {
			assert_eq!(
				CredentialOrigin::new("https", host, None),
				Err(CredentialGrantError::Origin),
				"host {host:?}"
			);
		}
		for (method, prefix) in [
			("*", "/a"),
			("", "/a"),
			("post", "/a"),
			("POST", ""),
			("POST", "a"),
			("POST", "/a/*"),
			("POST", "/a/../b"),
			("POST", "/a?x=1"),
			("POST", "/a%2fb"),
		] {
			assert_eq!(
				CredentialOperation::new(method, prefix),
				Err(CredentialGrantError::Operation),
				"{method:?} {prefix:?}"
			);
		}
		let mut empty = row("buildbuddy");
		empty.origins.clear();
		assert_eq!(empty.validate(), Err(CredentialGrantError::NoOrigins));
		let mut empty = row("buildbuddy");
		empty.operations.clear();
		assert_eq!(empty.validate(), Err(CredentialGrantError::NoOperations));
	}

	#[test]
	fn a_hand_edited_invalid_row_holds_nothing() {
		let grants = CredentialGrants::new([row("*"), row("")]);
		assert!(grants.is_empty());
		assert_eq!(
			grants.authorize("*", "POST", &url("https://remote.buildbuddy.io/api/v1/Run")),
			Err(CredentialRefusal::NoGrant)
		);
	}

	#[test]
	fn only_the_user_layer_may_hold_a_grant() {
		let mut project = row("buildbuddy");
		project.layer = Layer::Workspace;
		assert_eq!(project.validate(), Err(CredentialGrantError::Layer));
		let digest = sf!("digest-1");
		let (id, publisher) = (sf!("omp.buildbuddy"), sf!("path:/ext/buildbuddy"));
		assert!(row("buildbuddy").binds(&id, &publisher, Layer::Client, &digest));
		assert!(!project.binds(&id, &publisher, Layer::Workspace, &digest));
	}

	#[test]
	fn grant_binds_scheme_host_and_port() {
		let grants = CredentialGrants::new([row("buildbuddy")]);
		let run = "/api/v1/Run";
		let ok = |text: &str| {
			grants
				.authorize("buildbuddy", "POST", &url(text))
				.map(|_| ())
		};
		assert_eq!(ok(&format!("https://remote.buildbuddy.io{run}")), Ok(()));
		assert_eq!(ok(&format!("https://remote.buildbuddy.io:443{run}")), Ok(()));
		assert_eq!(ok(&format!("https://REMOTE.BuildBuddy.io{run}")), Ok(()), "host is case-folded");
		assert_eq!(
			ok(&format!("https://remote.buildbuddy.io:8443{run}")),
			Err(CredentialRefusal::OriginNotGranted),
			"a second port on the same host is a different origin"
		);
		assert_eq!(
			ok(&format!("http://remote.buildbuddy.io{run}")),
			Err(CredentialRefusal::NotHttps)
		);
		assert_eq!(
			ok(&format!("https://evil.example{run}")),
			Err(CredentialRefusal::OriginNotGranted)
		);
		assert_eq!(
			ok(&format!("https://remote.buildbuddy.io.evil.example{run}")),
			Err(CredentialRefusal::OriginNotGranted)
		);
		assert_eq!(
			ok(&format!("https://user:pw@remote.buildbuddy.io{run}")),
			Err(CredentialRefusal::OriginNotGranted),
			"userinfo is never part of the granted authority"
		);
		// A granted non-default port is exact, and IDNA is applied to the grant.
		let custom = CredentialOrigin::new("HTTPS", "BÜCHER.example", Some(8443)).expect("origin");
		assert_eq!(custom.host.as_str(), "xn--bcher-kva.example");
		let mut wide = row("buildbuddy");
		wide.origins = vec![custom];
		let grants = CredentialGrants::new([wide]);
		let ok = |text: &str| {
			grants
				.authorize("buildbuddy", "POST", &url(text))
				.map(|_| ())
		};
		assert_eq!(ok("https://bücher.example:8443/api/v1/Run"), Ok(()));
		assert_eq!(ok("https://bücher.example/api/v1/Run"), Err(CredentialRefusal::OriginNotGranted));
	}

	#[test]
	fn operations_match_whole_segments_of_an_unambiguous_path() {
		let grants = CredentialGrants::new([row("buildbuddy")]);
		let check = |method: &str, path: &str| {
			grants
				.authorize("buildbuddy", method, &url(&format!("https://remote.buildbuddy.io{path}")))
				.map(|_| ())
		};
		assert_eq!(check("POST", "/api/v1/Run"), Ok(()));
		assert_eq!(check("POST", "/api/v1/Run/status"), Ok(()));
		assert_eq!(check("POST", "/api/v1/Runner"), Err(CredentialRefusal::OperationNotGranted));
		assert_eq!(check("GET", "/api/v1/Run"), Err(CredentialRefusal::OperationNotGranted));
		assert_eq!(
			check("POST", "/api/v1/Run/%2e%2e/admin"),
			Err(CredentialRefusal::OperationNotGranted)
		);
		assert_eq!(check("POST", "/api/v1/Run%2Fx"), Err(CredentialRefusal::OperationNotGranted));
		assert_eq!(check("POST", "/other"), Err(CredentialRefusal::OperationNotGranted));
	}

	#[test]
	fn refusal_classes_name_what_applied() {
		let url = url("https://remote.buildbuddy.io/api/v1/Run");
		assert_eq!(
			CredentialGrants::default().authorize("buildbuddy", "POST", &url),
			Err(CredentialRefusal::NoGrant)
		);
		assert_eq!(
			CredentialGrants::new([row("buildbuddy")]).authorize("other", "POST", &url),
			Err(CredentialRefusal::ProviderNotGranted)
		);
	}

	fn file_with(rows: Vec<CredentialGrant>) -> crate::trust::GrantsFile {
		crate::trust::GrantsFile { version: 1, credential_grants: rows, ..Default::default() }
	}

	#[test]
	fn grant_binds_extension_identity() {
		let file = file_with(vec![row("buildbuddy")]);
		let (id, publisher, digest) =
			(sf!("omp.buildbuddy"), sf!("path:/ext/buildbuddy"), sf!("digest-1"));
		assert_eq!(
			file
				.credential_grants_for(&id, &publisher, Layer::Client, &digest)
				.providers(),
			vec![sf!("buildbuddy")]
		);
		let other_id = sf!("omp.other");
		assert!(
			file
				.credential_grants_for(&other_id, &publisher, Layer::Client, &digest)
				.is_empty()
		);
		let other_root = sf!("path:/elsewhere/buildbuddy");
		assert!(
			file
				.credential_grants_for(&id, &other_root, Layer::Client, &digest)
				.is_empty()
		);
		assert!(
			file
				.credential_grants_for(&id, &publisher, Layer::Workspace, &digest)
				.is_empty(),
			"the project layer never holds a grant (D13)"
		);
	}

	#[test]
	fn changed_credential_request_invalidates_grant() {
		let file = file_with(vec![row("buildbuddy")]);
		let (id, publisher) = (sf!("omp.buildbuddy"), sf!("path:/ext/buildbuddy"));
		assert!(
			!file
				.credential_grants_for(&id, &publisher, Layer::Client, &sf!("digest-1"))
				.is_empty()
		);
		assert!(
			file
				.credential_grants_for(&id, &publisher, Layer::Client, &sf!("digest-2"))
				.is_empty(),
			"a different manifest digest no longer matches the grant"
		);
	}

	#[test]
	fn persisted_credential_grants_round_trip_beside_other_rows() {
		use crate::trust::GrantsFile;

		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().join("grants.toml");
		GrantsFile::persist_credential_grant(&path, row("buildbuddy"))
			.expect("persist")
			.expect("valid grant");
		let mut second = row("buildbuddy");
		second.operations = vec![CredentialOperation::new("GET", "/api/v1/Run").expect("operation")];
		GrantsFile::persist_credential_grant(&path, second.clone())
			.expect("persist")
			.expect("valid grant");
		let read = GrantsFile::read(&path).expect("read");
		assert_eq!(
			read.credential_grants,
			vec![second],
			"the same identity and provider is replaced"
		);
		// A later write of an unrelated table keeps the credential rows.
		GrantsFile::update(&path, |grants| grants.version = 1).expect("rewrite");
		assert_eq!(
			GrantsFile::read(&path)
				.expect("read")
				.credential_grants
				.len(),
			1
		);
		// A malformed grant is refused and the file is untouched.
		let before = std::fs::read(&path).expect("bytes");
		assert_eq!(
			GrantsFile::persist_credential_grant(&path, row("*")).expect("persist"),
			Err(CredentialGrantError::Provider)
		);
		assert_eq!(std::fs::read(&path).expect("bytes"), before);
		let id = sf!("omp.buildbuddy");
		assert_eq!(
			GrantsFile::revoke_credential_grants(&path, &id, Some("other")).expect("revoke"),
			0
		);
		assert_eq!(GrantsFile::revoke_credential_grants(&path, &id, None).expect("revoke"), 1);
		assert!(
			GrantsFile::read(&path)
				.expect("read")
				.credential_grants
				.is_empty()
		);
	}
}
