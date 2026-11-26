//! Host-side credential placement for extension HTTP egress (ADR 0075 D4 to
//! D8).
//!
//! An extension names a provider; the host attaches that provider's stored
//! credential to the request header the catalog's auth spec names, on a granted
//! `(https, host, port)` and operation only. The key never reaches the
//! extension: it is leased here, written into a sensitive header just before
//! the hop is sent, and never logged, journalled or returned.
//!
//! Nothing here is reachable without a host-held grant record
//! ([`omp_ext::credential_grant`]) and a bound authority
//! ([`PlacementAuthority`]).

use std::{
	io,
	path::PathBuf,
	sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use http::{HeaderMap, HeaderName, header::HOST};
use omp_ai::auth::{
	AuthSpec, CredentialError, CredentialLease, CredentialNeed, HeaderPlacement, KeyPlacement,
};
use omp_catalog::{AuthSpecId, Catalog, ProviderId};
use omp_core::Str;
use omp_ext::credential_grant::CredentialRefusal;
use serde::Serialize;
use thiserror::Error;
use tokio::{io::AsyncWriteExt as _, sync::Mutex};

/// Why the host refuses to place a credential. The class is all the extension
/// learns; no variant carries a secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub(crate) enum PlacementRefusal {
	/// The extension's host-held grant record does not cover the request: no
	/// grant, another provider, a non-https URL, an origin or an operation
	/// outside the grant. The class names which.
	#[error("credential placement refused: {0}")]
	Grant(#[from] CredentialRefusal),
	/// A redirect hop left the grant. No connection was opened to it.
	#[error("credential placement refused on a redirect hop: {0}")]
	RedirectHop(CredentialRefusal),
	/// No credential authority is bound to this host, so nothing can place a
	/// credential. This is the answer in an Environment that has none, such as
	/// a remote one (D8).
	#[error("credential placement is unavailable: no credential authority is bound")]
	NoAuthority,
	/// The provider is not in the catalog.
	#[error("credential placement refused: the provider is not in the catalog")]
	UnknownProvider,
	/// The provider does not authenticate with a key in a request header.
	#[error("credential placement refused: the provider does not use a header key")]
	NotPlaceable,
	/// The request sets the placed header itself.
	#[error("credential placement refused: the request sets the placed header itself")]
	PlacedHeaderSetByRequest,
	/// The request sets a `Host` header.
	#[error("credential placement refused: the request sets a Host header")]
	CallerHostHeader,
	/// The store holds no usable credential for the provider.
	#[error("credential placement refused: no stored credential for the provider")]
	NoStoredCredential,
	/// The credential store could not supply the credential.
	#[error("credential placement refused: the credential store could not supply the credential")]
	CredentialStoreFailed,
	/// The placement could not be recorded, so it is not sent.
	#[error("credential placement refused: the placement could not be recorded")]
	AuditFailed,
	/// The response held the placed value, so it is not returned (D10).
	#[error("credential placement refused: the response contains the placed credential")]
	ResponseEchoesCredential,
}

/// Supplies the stored credential for one provider. The composition root
/// implements it over the encrypted credential store.
#[async_trait]
pub trait PlacementCredentials: Send + Sync + 'static {
	/// Leases the stored credential for `provider`, whose catalog auth spec is
	/// `spec`. [`CredentialError::Unavailable`] means none is stored.
	async fn lease(
		&self,
		provider: &ProviderId,
		spec: &AuthSpecId,
	) -> Result<CredentialLease, CredentialError>;
}

/// One secret-free audit row, written before a placed hop is sent (D7).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlacementAuditRow {
	/// Connection identity of the extension, `extension:{layer}:{tier}:{id}`.
	pub extension:  Str,
	/// Provider whose credential is placed.
	pub provider:   Str,
	/// Destination host of the hop.
	pub host:       Str,
	/// Destination port of the hop.
	pub port:       u16,
	/// Method of the hop.
	pub method:     Str,
	/// Protocol request id of the extension's request.
	pub request_id: u64,
	/// Zero for the first hop, then one per followed redirect.
	pub hop:        u32,
}

/// Sink for [`PlacementAuditRow`]s. A failed write blocks the send.
#[async_trait]
pub trait PlacementAudit: Send + Sync + 'static {
	/// Durably records `row`.
	async fn record(&self, row: &PlacementAuditRow) -> io::Result<()>;
}

/// Append-only JSON-lines audit file, flushed before the hop is sent.
pub struct JsonlPlacementAudit {
	path:   PathBuf,
	append: Mutex<()>,
}

impl JsonlPlacementAudit {
	/// Writes rows to `path`, creating its directory on first use.
	#[must_use]
	pub fn new(path: PathBuf) -> Self {
		Self { path, append: Mutex::new(()) }
	}
}

#[async_trait]
impl PlacementAudit for JsonlPlacementAudit {
	async fn record(&self, row: &PlacementAuditRow) -> io::Result<()> {
		#[derive(Serialize)]
		struct Line<'a> {
			kind:  &'static str,
			ts_ms: u64,
			#[serde(flatten)]
			row:   &'a PlacementAuditRow,
		}
		let ts_ms = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
		let mut bytes = serde_json::to_vec(&Line { kind: "credential_placed", ts_ms, row })
			.map_err(io::Error::other)?;
		bytes.push(b'\n');
		let _guard = self.append.lock().await;
		if let Some(parent) = self.path.parent() {
			tokio::fs::create_dir_all(parent).await?;
		}
		let mut file = tokio::fs::OpenOptions::new()
			.create(true)
			.append(true)
			.open(&self.path)
			.await?;
		file.write_all(&bytes).await?;
		file.sync_all().await
	}
}

/// What the composition root binds to make placement available.
pub struct PlacementBinding {
	/// Where the stored credential comes from.
	pub credentials: Arc<dyn PlacementCredentials>,
	/// Where each placement is recorded.
	pub audit:       Arc<dyn PlacementAudit>,
	/// Transport for placed requests; `None` uses the host's own client. Only
	/// composition code sets it, never an extension.
	pub client:      Option<omp_http::Client>,
}

/// The credential authority of one Environment host, bound at composition.
///
/// Unbound by default and in any Environment that is not composed with one:
/// a placed request then fails with a typed "placement unavailable" error and
/// the key never crosses a wire (D8).
#[derive(Default)]
pub struct PlacementAuthority {
	bound: OnceLock<PlacementBinding>,
}

impl PlacementAuthority {
	/// Binds the authority once; a second bind is refused and returned.
	///
	/// # Errors
	///
	/// The rejected `binding` when one is already bound.
	pub fn bind(&self, binding: PlacementBinding) -> Result<(), PlacementBinding> {
		self.bound.set(binding)
	}

	pub(crate) fn binding(&self) -> Option<&PlacementBinding> {
		self.bound.get()
	}
}

impl std::fmt::Debug for PlacementAuthority {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("PlacementAuthority")
			.field("bound", &self.bound.get().is_some())
			.finish()
	}
}

/// A provider resolved from the catalog: where its key goes comes from the
/// auth spec, never from the request.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedProvider {
	pub(crate) provider: ProviderId,
	pub(crate) spec:     AuthSpecId,
	pub(crate) header:   HeaderPlacement,
	pub(crate) name:     HeaderName,
}

/// Resolves `provider` to its single API-key-in-a-header auth spec.
pub(crate) fn resolve_provider(provider: &str) -> Result<ResolvedProvider, PlacementRefusal> {
	let catalog = Catalog::embedded();
	let id = ProviderId::from(provider);
	let entry = catalog
		.provider(&id)
		.ok_or(PlacementRefusal::UnknownProvider)?;
	let [spec] = entry.auth.as_ref() else {
		return Err(PlacementRefusal::NotPlaceable);
	};
	let catalog_spec = catalog
		.auth_spec(spec)
		.ok_or(PlacementRefusal::NotPlaceable)?;
	let Ok(AuthSpec::ApiKey { placement: KeyPlacement::Header(header), .. }) =
		AuthSpec::from_catalog(catalog_spec, None, None)
	else {
		return Err(PlacementRefusal::NotPlaceable);
	};
	let name =
		HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| PlacementRefusal::NotPlaceable)?;
	Ok(ResolvedProvider { provider: id, spec: spec.clone(), header, name })
}

/// Refuses a request that sets the placed header or a `Host` header itself
/// (D2). The authority is derived only from the validated URL.
pub(crate) fn check_caller_headers(
	headers: &HeaderMap,
	placed: &HeaderName,
) -> Result<(), PlacementRefusal> {
	if headers.contains_key(placed) {
		return Err(PlacementRefusal::PlacedHeaderSetByRequest);
	}
	if headers.contains_key(HOST) {
		return Err(PlacementRefusal::CallerHostHeader);
	}
	Ok(())
}

/// Leases the stored credential and maps the failure to a refusal class.
pub(crate) async fn lease_credential(
	binding: &PlacementBinding,
	resolved: &ResolvedProvider,
) -> Result<CredentialLease, PlacementRefusal> {
	binding
		.credentials
		.lease(&resolved.provider, &resolved.spec)
		.await
		.map_err(|error| match error {
			CredentialError::Unavailable => PlacementRefusal::NoStoredCredential,
			_ => PlacementRefusal::CredentialStoreFailed,
		})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn buildbuddy_resolves_to_its_catalog_header() {
		let resolved = resolve_provider("buildbuddy").expect("buildbuddy places by header");
		assert_eq!(resolved.name.as_str(), "x-buildbuddy-api-key");
		assert!(resolved.header.prefix.is_empty());
	}

	#[test]
	fn unknown_and_unplaceable_providers_are_refused() {
		assert_eq!(
			resolve_provider("no-such-provider").map(|_| ()),
			Err(PlacementRefusal::UnknownProvider)
		);
		assert_eq!(resolve_provider("").map(|_| ()), Err(PlacementRefusal::UnknownProvider));
	}

	#[test]
	fn caller_headers_may_not_set_the_placed_header_or_host() {
		let placed = HeaderName::from_static("x-buildbuddy-api-key");
		let mut headers = HeaderMap::new();
		headers.insert("x-other", "v".parse().expect("value"));
		assert_eq!(check_caller_headers(&headers, &placed), Ok(()));
		headers.insert("X-BuildBuddy-Api-Key", "caller".parse().expect("value"));
		assert_eq!(
			check_caller_headers(&headers, &placed),
			Err(PlacementRefusal::PlacedHeaderSetByRequest)
		);
		headers.remove("x-buildbuddy-api-key");
		headers.insert(HOST, "evil.example".parse().expect("value"));
		assert_eq!(check_caller_headers(&headers, &placed), Err(PlacementRefusal::CallerHostHeader));
	}
}
