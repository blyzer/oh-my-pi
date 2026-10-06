//! Session account pins: the journaled convar that records them and the
//! resolved form the account selector consumes.
//!
//! A pin is recorded as the opaque [`CredentialAffinityDigest`] of the pinned
//! account, never its id or principal, so the session journal carries no
//! identity. The digest resolves against the live account pool at request
//! time ([`AuthManager::session_pins`](crate::auth::AuthManager::session_pins)),
//! which keeps a pin honest across logins and removals: a pin whose account no
//! longer exists resolves to [`AccountPin::Unresolved`] and fails closed.

use std::sync::Arc;

use omp_con::{Kv, Value};
use omp_core::Str;

use super::{AccountPin, Eligibility, SelectionReceipt};
use crate::{catalog::ProviderId, session::CredentialAffinityDigest};

omp_con::var! {
	/// Stored accounts this session is exclusively pinned to, keyed by provider
	/// with the opaque credential-affinity digest of the account as the value.
	/// Session-scoped: journaled with the session and restored on resume, never
	/// archived to config, and unreachable from project cfg.
	pub static AI_ACCOUNT_PINS = ai_account_pins: Kv {
		default: Kv::new(),
		flags: session,
	};
}

/// Resolved session pins, one optional pin per provider.
///
/// Clone-cheap: it rides every call's affinity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionAccountPins(Option<Arc<[(ProviderId, AccountPin)]>>);

impl SessionAccountPins {
	/// No provider is pinned.
	pub const NONE: Self = Self(None);

	/// Wraps resolved pins; a later entry for the same provider is ignored.
	pub fn new(pins: impl IntoIterator<Item = (ProviderId, AccountPin)>) -> Self {
		let pins: Arc<[_]> = pins.into_iter().collect();
		Self((!pins.is_empty()).then_some(pins))
	}

	/// The pin `provider` is constrained to, if any.
	pub fn for_provider(&self, provider: &ProviderId<str>) -> Option<&AccountPin> {
		self
			.0
			.iter()
			.flat_map(|pins| pins.iter())
			.find_map(|(pinned, pin)| (pinned.as_str() == provider.as_str()).then_some(pin))
	}

	/// Whether no provider is pinned.
	pub const fn is_empty(&self) -> bool {
		self.0.is_none()
	}
}

/// The recorded pins in `pins`: each provider with its digest, or `None` when
/// the recorded value is not a canonical digest (hand-edited), which resolves
/// closed rather than being dropped.
pub fn recorded_pins(
	pins: &Kv,
) -> impl DoubleEndedIterator<Item = (ProviderId, Option<CredentialAffinityDigest>)> + '_ {
	pins.iter().map(|(provider, value)| {
		(ProviderId::new(provider.clone()), value.as_str().and_then(CredentialAffinityDigest::parse))
	})
}

/// `pins` with `provider` pinned to `digest`, replacing any earlier pin for
/// that provider.
#[must_use]
pub fn with_pin(pins: &Kv, provider: &ProviderId<str>, digest: &CredentialAffinityDigest) -> Kv {
	let mut next = without_pin(pins, provider);
	next
		.0
		.push((Str::new(provider.as_str()), Value::Str(Str::new(digest.as_str()))));
	next
}

/// `pins` without any pin for `provider`.
#[must_use]
pub fn without_pin(pins: &Kv, provider: &ProviderId<str>) -> Kv {
	Kv(pins
		.iter()
		.filter(|(pinned, _)| pinned.as_str() != provider.as_str())
		.map(|(pinned, value)| (pinned.clone(), value.clone()))
		.collect())
}

/// Why an exclusively pinned account could not serve a request.
///
/// The snake-case variant name is the stable notice code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::IntoStaticStr, thiserror::Error)]
#[strum(serialize_all = "snake_case")]
pub enum PinFailure {
	/// The pinned account is no longer stored.
	#[error("the account is no longer stored")]
	Removed,
	/// The account is administratively disabled.
	#[error("the account is disabled")]
	Disabled,
	/// The account does not serve the requested route.
	#[error("the account does not serve this route")]
	RouteIneligible,
	/// The account's credential was rejected.
	#[error("the account's credential was rejected")]
	CredentialRejected,
	/// The account is in a health cooldown.
	#[error("the account is cooling down")]
	CoolingDown,
	/// The account is rate limited.
	#[error("the account is rate limited")]
	RateLimited,
	/// The account's quota is exhausted or reserved.
	#[error("the account's quota is exhausted")]
	QuotaExhausted,
	/// The account already failed this request, and a pinned session may not
	/// rotate to another one.
	#[error("the account already failed this request and a pin forbids rotating")]
	Rotated,
	/// The account could not be selected for another reason.
	#[error("the account could not be selected")]
	Unselectable,
}

impl PinFailure {
	/// Why `pin` could not serve the selection `receipt` describes.
	#[must_use]
	pub fn diagnose(pin: &AccountPin, receipt: &SelectionReceipt) -> Self {
		let AccountPin::Account(account) = pin else {
			return Self::Removed;
		};
		let Some(candidate) = receipt
			.candidates
			.iter()
			.find(|candidate| &candidate.account == account)
		else {
			return Self::Removed;
		};
		match candidate.eligibility {
			Eligibility::Disabled => Self::Disabled,
			Eligibility::RouteIneligible => Self::RouteIneligible,
			Eligibility::CredentialRejected { .. } => Self::CredentialRejected,
			Eligibility::Cooldown { .. } => Self::CoolingDown,
			Eligibility::RateLimited { .. } => Self::RateLimited,
			Eligibility::QuotaExhausted { .. } | Eligibility::QuotaReserved { .. } => {
				Self::QuotaExhausted
			},
			Eligibility::PreviousAccount => Self::Rotated,
			Eligibility::Eligible
			| Eligibility::RotationForbidden
			| Eligibility::PrincipalMismatch
			| Eligibility::NotPinned => Self::Unselectable,
		}
	}
}
