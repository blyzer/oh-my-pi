//! Process-level secret policy and composition.

use std::{collections::BTreeMap, sync::Arc};

use omp_ai::auth::AuthControlHandle;
use omp_core::Str;
use omp_envd::worker::ExtHostSpec;
use omp_secrets::SecretMaskingAuthority;

use crate::auth_backend::{CredentialControlGrant, CredentialSecretControlFactory};

/// Global/project rule loading.
pub mod config;
/// Credential-shaped environment collection.
pub mod env;
/// Persistent placeholder-key resolution with a process-stable ephemeral
/// fallback.
pub mod key;
/// Immutable per-session snapshot composition.
pub mod session;
use session::SecretSessionSnapshot;

/// Lowers the host-held credential grants into the exact Core-side grants
/// consumed by the credential CONTROL factory.
///
/// The scopes come only from each spec's `credential_grants`, the operator's
/// record in `grants.toml` (ADR 0075 D3). A manifest's own `credentials.*`
/// declarations are never read: an extension that asks for a provider holds
/// none until an operator grants it, so wiring this up cannot let an extension
/// grant itself. `import` and `reveal` have no host-held source and stay empty
/// (ADR 0075 D9, O5).
pub fn credential_control_grants(
	extensions: &[ExtHostSpec],
) -> BTreeMap<Str, CredentialControlGrant> {
	use omp_ai::auth::{CredentialGrants, CredentialScope};

	extensions
		.iter()
		.map(|extension| {
			let providers: Arc<[Str]> = extension.credential_grants.providers().into();
			(extension.key.extension().clone(), CredentialControlGrant {
				grants: CredentialGrants {
					allow:  CredentialScope::new(Arc::clone(&providers)),
					import: CredentialScope::new(Arc::from([])),
					reveal: CredentialScope::new(Arc::from([])),
				},
				providers,
			})
		})
		.collect()
}

/// Builds one Core-owned masking authority over the immutable session rules.
pub fn core_secret_masking_authority(
	snapshot: &SecretSessionSnapshot,
	extension: impl Into<Str>,
	host_generation: u64,
) -> Result<Arc<SecretMaskingAuthority>, omp_secrets::SecretMaskingError> {
	SecretMaskingAuthority::new(
		extension,
		host_generation,
		snapshot.rules().iter().cloned(),
		key::placeholder_key(),
	)
	.map(Arc::new)
}

/// Composes the live auth handle and Core masking snapshot into the CONTROL
/// domain factory consumed by Environment authority wiring.
pub fn credential_secret_control_factory(
	control: AuthControlHandle,
	grants: BTreeMap<Str, CredentialControlGrant>,
	snapshot: &SecretSessionSnapshot,
) -> CredentialSecretControlFactory {
	CredentialSecretControlFactory::new(
		control,
		grants,
		Arc::from(snapshot.rules().to_vec()),
		Arc::<str>::from(key::placeholder_key()),
	)
}

#[cfg(test)]
mod tests {
	use omp_core::{ArtifactDigest, Hash32, Provenance, sf};
	use omp_envd::{
		exthost::{DeclarationSet, ExtensionManifest, ServiceManifest},
		worker::HostKey,
	};
	use omp_ext::{
		Layer,
		config::StaticDeclarations,
		credential_grant::{
			CredentialGrant, CredentialGrants, CredentialOperation, CredentialOrigin,
		},
	};

	use super::*;

	/// A spec whose manifest asks for `buildbuddy` through `credentials.allow`,
	/// `credentials.import` and `credentials.reveal`, with no host grant.
	fn requesting_spec() -> ExtHostSpec {
		let mut properties = BTreeMap::new();
		properties.insert(
			Str::new_static("capabilities"),
			serde_json::json!({
				"credentials.allow": ["buildbuddy"],
				"credentials.import": ["buildbuddy"],
				"credentials.reveal": ["buildbuddy"],
			}),
		);
		let declarations = StaticDeclarations::from_properties(&properties).expect("declarations");
		let provenance = Provenance::new(
			sf!("path:/ext/buildbuddy"),
			sf!("omp.buildbuddy"),
			sf!("local"),
			ArtifactDigest::new(Hash32::sum(b"manifest").into_bytes()),
			sf!("client"),
			sf!("sandboxed"),
			1,
		);
		let manifest = ExtensionManifest::new_with_static(
			provenance,
			sf!("omp_buildbuddy"),
			[],
			DeclarationSet::new([], []),
			ServiceManifest::default(),
			declarations,
			[],
			[],
		);
		ExtHostSpec::new(HostKey::new("client", "sandboxed", "omp.buildbuddy"), manifest)
	}

	fn host_grant(provider: &str) -> CredentialGrant {
		CredentialGrant {
			id:                sf!("omp.buildbuddy"),
			publisher:         sf!("path:/ext/buildbuddy"),
			layer:             Layer::Client,
			capability_digest: sf!("digest"),
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

	#[test]
	fn manifest_request_without_host_grant_is_denied() {
		let spec = requesting_spec();
		assert!(
			spec
				.manifest
				.static_declarations()
				.capability_grants
				.contains_key("credentials.allow"),
			"the fixture really asks for the provider"
		);

		let grants = credential_control_grants(&[spec]);
		let held = grants.get("omp.buildbuddy").expect("extension row");

		assert!(held.providers.is_empty());
		assert!(!held.grants.allow.allows("buildbuddy"), "scopes are never read from the manifest");
		assert!(!held.grants.import.allows("buildbuddy"));
		assert!(!held.grants.reveal.allows("buildbuddy"));
	}

	#[test]
	fn scopes_come_from_the_host_held_grant() {
		let mut spec = requesting_spec();
		spec.credential_grants = CredentialGrants::new([host_grant("buildbuddy")]);

		let grants = credential_control_grants(&[spec]);
		let held = grants.get("omp.buildbuddy").expect("extension row");

		assert_eq!(&*held.providers, [sf!("buildbuddy")]);
		assert!(held.grants.allow.allows("buildbuddy"));
		assert!(!held.grants.import.allows("buildbuddy"), "import has no host-held source");
		assert!(!held.grants.reveal.allows("buildbuddy"), "reveal has no host-held source");
	}
}
