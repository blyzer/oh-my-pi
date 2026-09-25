//! Strongly typed catalog identifiers.

use omp_core::{sf, string_id};

string_id!(/// Identifies a commercial, hosted, or local provider domain.
	ProviderId);
string_id!(/// Identifies one concrete provider route.
	RouteId);
string_id!(/// Identifies one wire codec implementation.
	CodecId);
string_id!(/// Identifies one normalized selectable model deployment.
	ModelKey);
string_id!(/// Identifies a normalized model class (vendor lineage).
	ClassId);
string_id!(/// Identifies a product family within a class (for example flash, sonnet, or opus).
	FamilyId);
string_id!(/// Identifies an interned authentication specification.
	AuthSpecId);
string_id!(/// Identifies an interned public OAuth flow specification.
	OAuthSpecId);
string_id!(/// Identifies an interned static header profile.
	HeaderProfileId);
string_id!(/// Identifies an interned model-discovery specification.
	DiscoverySpecId);
string_id!(/// Identifies an interned wire-lowering policy.
	WirePolicyId);
string_id!(/// Identifies an interned reasoning policy.
	ThinkingPolicyId);
string_id!(/// Carries the opaque model identifier expected by a wire endpoint.
	WireModelId);
string_id!(/// Identifies an immutable catalog revision.
	CatalogRevision);

impl ModelKey {
	/// Names `model` inside `provider`'s namespace: `<provider>/<model>`.
	///
	/// The one key scheme every routed model uses — bundled records, runtime
	/// discovery rows, and `models.toml` entries — so two providers serving
	/// the same model id always keep one model each.
	pub fn provider_scoped(provider: &ProviderId<str>, model: &str) -> Self {
		Self::new(sf!("{provider}/{model}"))
	}
}

impl ModelKey<str> {
	/// The provider's own model id when this key lies in `provider`'s
	/// namespace; the inverse of [`ModelKey::provider_scoped`].
	pub fn scoped_model(&self, provider: &ProviderId<str>) -> Option<&str> {
		self
			.as_str()
			.strip_prefix(provider.as_str())
			.and_then(|rest| rest.strip_prefix('/'))
	}
}
