//! Catalog-aware credential acquisition across typed source engines.

use std::{collections::BTreeMap, env, fmt, sync::Arc};

use futures::future::{Either, FutureExt as _};
use omp_catalog::{
	AuthSpecId, Catalog, ProviderId,
	provider::{AuthSpec, AuthSpecKind, CredentialSourceSpec, RouteDef},
};
use omp_core::{SecretString, Str, sf};

use super::{
	aws::AwsCredentialSource,
	lease::{
		AuthRejection, CredentialError, CredentialFuture, CredentialKind, CredentialLease,
		CredentialNeed, CredentialSource, ExtensionCredentialKind, LeaseMeta, credential_ready,
	},
};
use crate::{AccountId, PrincipalId, UnleasedLoginRemedy};

const ENVIRONMENT_TAG: &str = "environment";
const STORED_TAG: &str = "stored";
const ADC_TAG: &str = "application-default";
const AWS_TAG: &str = "aws-chain";
const OAUTH_TAG: &str = "oauth";
const SESSION_TAG: &str = "session";
const INVOCATION_TAG: &str = "invocation";

/// Secret environment boundary used by [`CredentialBroker`].
pub trait CredentialEnvironment: Send + Sync {
	/// Reads one exact catalog-declared name into a zeroizing secret wrapper.
	fn read(&self, name: &str) -> Result<Option<SecretString>, CredentialError>;
}

/// Process environment implementation that performs no alias or fallback
/// lookup.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCredentialEnvironment;

impl CredentialEnvironment for SystemCredentialEnvironment {
	fn read(&self, name: &str) -> Result<Option<SecretString>, CredentialError> {
		if name.is_empty() {
			return Err(CredentialError::InvalidSource);
		}
		match env::var(name) {
			Ok(value) if value.is_empty() => Err(CredentialError::InvalidSource),
			Ok(value) => Ok(Some(SecretString::from(value))),
			Err(env::VarError::NotPresent) => Ok(None),
			Err(env::VarError::NotUnicode(_)) => Err(CredentialError::SourceFailure),
		}
	}
}

/// Optional typed engines used by the catalog credential broker.
#[derive(Clone, Default)]
pub struct CredentialBrokerEngines {
	/// Encrypted account-store engine.
	pub stored:              Option<Arc<dyn CredentialSource>>,
	/// Application-default credential engine.
	pub application_default: Option<Arc<dyn CredentialSource>>,
	/// AWS credential-chain engine.
	pub aws:                 Option<Arc<dyn CredentialSource>>,
	/// OAuth login/refresh engine.
	pub oauth:               Option<Arc<dyn CredentialSource>>,
	/// Interactive provider-session engine.
	pub session:             Option<Arc<dyn CredentialSource>>,
}

impl fmt::Debug for CredentialBrokerEngines {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("CredentialBrokerEngines")
			.field("stored", &self.stored.is_some())
			.field("application_default", &self.application_default.is_some())
			.field("aws", &self.aws.is_some())
			.field("oauth", &self.oauth.is_some())
			.field("session", &self.session.is_some())
			.finish()
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EngineKind {
	Stored,
	ApplicationDefault,
	Aws,
	OAuth,
	Session,
}

impl EngineKind {
	const fn tag(self) -> &'static str {
		match self {
			Self::Stored => STORED_TAG,
			Self::ApplicationDefault => ADC_TAG,
			Self::Aws => AWS_TAG,
			Self::OAuth => OAUTH_TAG,
			Self::Session => SESSION_TAG,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BrokerSource {
	Environment(Box<[Str]>),
	BasicEnvironment { username_names: Box<[Str]>, password_names: Box<[Str]> },
	Engine(EngineKind),
}
#[derive(Clone, Debug)]
struct InvocationOverride {
	specs:  Arc<BTreeMap<AuthSpecId, CredentialKind>>,
	secret: SecretString,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BrokerPlan {
	kind:    CredentialKind,
	sources: Box<[BrokerSource]>,
}

/// Catalog compilation failure for credential acquisition plans.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CredentialBrokerError {
	/// An authenticated catalog record has no declared acquisition source.
	#[error("catalog authentication specification has no credential source")]
	MissingSource(AuthSpecId),
	/// A credential environment source is empty or does not lead with an OMP
	/// name.
	#[error("catalog credential environment source must lead with an OMP_* name")]
	InvalidEnvironment(AuthSpecId),
	/// The selected provider does not exist in the catalog.
	#[error("invocation credential override names an unknown provider")]
	UnknownProvider(omp_catalog::ProviderId),
	/// The selected provider has no scalar authentication compatible with a
	/// generic API key.
	#[error("selected provider does not accept a generic invocation API key")]
	UnsupportedOverride(omp_catalog::ProviderId),
}

/// Catalog-aware composite credential source.
///
/// Plans retain exact catalog source order. Only `Unavailable` advances to the
/// next source; cancellation, invalid source, expiry, staleness, and engine
/// failure remain typed terminal evidence.
#[derive(Clone)]
pub struct CredentialBroker {
	plans:       Arc<BTreeMap<AuthSpecId, BrokerPlan>>,
	environment: Arc<dyn CredentialEnvironment>,
	engines:     CredentialBrokerEngines,
	invocation:  Option<InvocationOverride>,
}

impl CredentialBroker {
	/// Compiles immutable acquisition plans from the canonical catalog.
	pub fn from_catalog(
		catalog: &Catalog,
		environment: Arc<dyn CredentialEnvironment>,
		engines: CredentialBrokerEngines,
	) -> Result<Self, CredentialBrokerError> {
		let mut plans = BTreeMap::new();
		for auth in catalog.auth_specs() {
			let Some(kind) = credential_kind(auth.kind) else {
				continue;
			};
			let mut sources = Vec::with_capacity(auth.credential_sources.len());
			for source in &auth.credential_sources {
				use omp_catalog::provider::CredentialSourceSpec as CatalogSource;
				let source = match source {
					CatalogSource::Environment { ordered_names } => {
						if ordered_names
							.first()
							.is_none_or(|name| !name.starts_with("OMP_"))
						{
							return Err(CredentialBrokerError::InvalidEnvironment(auth.id.clone()));
						}
						BrokerSource::Environment(ordered_names.clone())
					},
					CatalogSource::BasicEnvironment { username_names, password_names } => {
						if username_names
							.first()
							.is_none_or(|name| !name.starts_with("OMP_"))
							|| password_names
								.first()
								.is_none_or(|name| !name.starts_with("OMP_"))
						{
							return Err(CredentialBrokerError::InvalidEnvironment(auth.id.clone()));
						}
						BrokerSource::BasicEnvironment {
							username_names: username_names.clone(),
							password_names: password_names.clone(),
						}
					},
					CatalogSource::Stored => BrokerSource::Engine(EngineKind::Stored),
					CatalogSource::ApplicationDefault { .. } => {
						BrokerSource::Engine(EngineKind::ApplicationDefault)
					},
					CatalogSource::AwsChain => BrokerSource::Engine(EngineKind::Aws),
					CatalogSource::Oauth { .. } => BrokerSource::Engine(EngineKind::OAuth),
					CatalogSource::Session => BrokerSource::Engine(EngineKind::Session),
				};
				sources.push(source);
			}
			if sources.is_empty() {
				return Err(CredentialBrokerError::MissingSource(auth.id.clone()));
			}
			plans.insert(auth.id.clone(), BrokerPlan { kind, sources: sources.into_boxed_slice() });
		}
		Ok(Self { plans: Arc::new(plans), environment, engines, invocation: None })
	}

	/// Uses the process environment without upstream aliases and installs the
	/// complete process-wide AWS credential chain when no injected engine was
	/// supplied.
	pub fn system(
		catalog: &Catalog,
		mut engines: CredentialBrokerEngines,
	) -> Result<Self, CredentialBrokerError> {
		if engines.aws.is_none() {
			engines.aws = Some(Arc::new(AwsCredentialSource::system()));
		}
		Self::from_catalog(catalog, Arc::new(SystemCredentialEnvironment), engines)
	}

	/// Returns a session-owned broker overlay for one selected provider.
	///
	/// The generic key is held only by the returned clone. It is never written
	/// to the process environment or delegated to a durable credential engine.
	pub fn with_api_key_override(
		&self,
		catalog: &Catalog,
		provider: &omp_catalog::ProviderId<str>,
		secret: SecretString,
	) -> Result<Self, CredentialBrokerError> {
		let provider = catalog
			.provider(provider)
			.ok_or_else(|| CredentialBrokerError::UnknownProvider(provider.to_owned()))?;
		let specs = provider
			.auth
			.iter()
			.filter_map(|id| {
				let kind = credential_kind(catalog.auth_spec(id)?.kind)?;
				matches!(
					kind,
					CredentialKind::ApiKey | CredentialKind::Bearer | CredentialKind::SessionToken
				)
				.then(|| (id.clone(), kind))
			})
			.collect::<BTreeMap<_, _>>();
		if specs.is_empty() {
			return Err(CredentialBrokerError::UnsupportedOverride(provider.id.clone()));
		}
		let mut broker = self.clone();
		broker.invocation = Some(InvocationOverride { specs: Arc::new(specs), secret });
		Ok(broker)
	}

	/// Refreshes the renewable engine for an exact account/spec selection.
	///
	/// Stored OAuth is authoritative when installed. An AWS-only plan refreshes
	/// its chain in place so rejected or expiring role credentials are
	/// re-resolved. Environment, invocation, ADC, and session sources remain
	/// nonrenewable, and no ordinary source fallback occurs.
	pub fn refresh_account(
		&self,
		need: CredentialNeed,
	) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>> {
		let Some(plan) = self.plans.get(&need.spec) else {
			return credential_ready(Err(CredentialError::InvalidSource));
		};
		let Some(selected) = [EngineKind::Stored, EngineKind::OAuth, EngineKind::Aws]
			.into_iter()
			.find(|kind| {
				self.engine(*kind).is_some()
					&& plan
						.sources
						.iter()
						.any(|source| source == &BrokerSource::Engine(*kind))
			})
		else {
			return credential_ready(Err(CredentialError::Unavailable));
		};
		let kind = plan.kind;
		let refreshed = self
			.engine(selected)
			.expect("selected installed renewable engine")
			.refresh_lease(need.clone());
		map_credential(refreshed, move |result| {
			result.and_then(|lease| Self::validate_lease(lease, &need, kind, selected.tag()))
		})
	}

	/// Refreshes the exact source that produced a rejected lease and returns
	/// its new generation once.
	///
	/// Environment and invocation credentials are nonrenewable and fail
	/// closed; no later source is tried.
	pub fn refresh_lease<'a>(
		&'a self,
		rejected: &'a CredentialLease,
		need: CredentialNeed,
	) -> CredentialFuture<'a, Result<CredentialLease, CredentialError>> {
		let Some(tag) = rejected.source_tag() else {
			return credential_ready(Err(CredentialError::InvalidSource));
		};
		let engine = match tag {
			STORED_TAG => EngineKind::Stored,
			AWS_TAG => EngineKind::Aws,
			OAUTH_TAG => EngineKind::OAuth,
			ENVIRONMENT_TAG | INVOCATION_TAG | ADC_TAG | SESSION_TAG => {
				return credential_ready(Err(CredentialError::Unavailable));
			},
			_ => return credential_ready(Err(CredentialError::InvalidSource)),
		};
		let Some(plan) = self.plans.get(&need.spec) else {
			return credential_ready(Err(CredentialError::InvalidSource));
		};
		let Some(source) = self.engine(engine) else {
			return credential_ready(Err(CredentialError::Unavailable));
		};
		let kind = plan.kind;
		map_credential(source.refresh_lease(need.clone()), move |result| {
			result.and_then(|lease| Self::validate_lease(lease, &need, kind, engine.tag()))
		})
	}

	fn invocation_lease(
		&self,
		need: &CredentialNeed,
	) -> Option<Result<CredentialLease, CredentialError>> {
		let invocation = self.invocation.as_ref()?;
		let kind = *invocation.specs.get(&need.spec)?;
		let account = need
			.account
			.clone()
			.unwrap_or_else(|| AccountId::from("invocation"));
		let principal = need
			.principal
			.clone()
			.unwrap_or_else(|| PrincipalId::from("invocation"));
		let meta = LeaseMeta { account, principal, generation: 0, expires_at: None };
		let lease = match kind {
			CredentialKind::ApiKey => CredentialLease::api_key(meta, invocation.secret.clone()),
			CredentialKind::Bearer => CredentialLease::bearer(meta, invocation.secret.clone()),
			CredentialKind::SessionToken => {
				CredentialLease::session_token(meta, invocation.secret.clone())
			},
			CredentialKind::Basic | CredentialKind::AwsSigV4 => {
				return Some(Err(CredentialError::InvalidSource));
			},
		};
		Some(Ok(lease.with_source_tag(sf!(INVOCATION_TAG))))
	}

	fn engine(&self, kind: EngineKind) -> Option<&Arc<dyn CredentialSource>> {
		match kind {
			EngineKind::Stored => self.engines.stored.as_ref(),
			EngineKind::ApplicationDefault => self.engines.application_default.as_ref(),
			EngineKind::Aws => self.engines.aws.as_ref(),
			EngineKind::OAuth => self.engines.oauth.as_ref(),
			EngineKind::Session => self.engines.session.as_ref(),
		}
	}

	/// Reads the first declared name that is set, tracing every miss.
	///
	/// Returns the name that produced the secret so the lease can be
	/// attributed without exposing the value.
	fn read_environment<'n>(
		&self,
		names: &'n [Str],
		spec: &AuthSpecId,
	) -> Result<Option<(&'n Str, SecretString)>, CredentialError> {
		for name in names {
			match self.environment.read(name)? {
				Some(secret) => {
					tracing::debug!(spec = %spec, variable = %name, "credential environment variable set");
					return Ok(Some((name, secret)));
				},
				None => {
					tracing::debug!(spec = %spec, variable = %name, "credential environment variable unset");
				},
			}
		}
		Ok(None)
	}

	/// Lease identity for an environment credential.
	///
	/// Brokered routes (no durable account selected) carry no identity, so the
	/// lease is attributed to the environment and the variable that produced
	/// it; an explicit account/principal from a selected record is kept.
	fn environment_meta(need: &CredentialNeed, variable: &Str) -> LeaseMeta {
		let account = need
			.account
			.clone()
			.unwrap_or_else(|| AccountId::from(ENVIRONMENT_TAG));
		let principal = need
			.principal
			.clone()
			.unwrap_or_else(|| PrincipalId::from(variable.as_str()));
		LeaseMeta { account, principal, generation: 0, expires_at: None }
	}

	fn environment_lease(
		&self,
		names: &[Str],
		need: &CredentialNeed,
		kind: CredentialKind,
	) -> Result<CredentialLease, CredentialError> {
		let Some((variable, secret)) = self.read_environment(names, &need.spec)? else {
			return Err(CredentialError::Unavailable);
		};
		let meta = Self::environment_meta(need, variable);
		let lease = match kind {
			CredentialKind::ApiKey => CredentialLease::api_key(meta, secret),
			CredentialKind::Basic => return Err(CredentialError::InvalidSource),
			CredentialKind::Bearer => CredentialLease::bearer(meta, secret),
			CredentialKind::SessionToken => CredentialLease::session_token(meta, secret),
			CredentialKind::AwsSigV4 => return Err(CredentialError::InvalidSource),
		};
		Ok(lease.with_source_tag(sf!(ENVIRONMENT_TAG)))
	}

	fn basic_environment_lease(
		&self,
		username_names: &[Str],
		password_names: &[Str],
		need: &CredentialNeed,
	) -> Result<CredentialLease, CredentialError> {
		let Some((variable, username)) = self.read_environment(username_names, &need.spec)? else {
			return Err(CredentialError::Unavailable);
		};
		let Some((_, password)) = self.read_environment(password_names, &need.spec)? else {
			return Err(CredentialError::Unavailable);
		};
		let meta = Self::environment_meta(need, variable);
		Ok(CredentialLease::basic(meta, username, password).with_source_tag(sf!(ENVIRONMENT_TAG)))
	}

	fn validate_lease(
		lease: CredentialLease,
		need: &CredentialNeed,
		expected: CredentialKind,
		tag: &'static str,
	) -> Result<CredentialLease, CredentialError> {
		if lease.kind() != expected {
			return Err(CredentialError::KindMismatch {
				expected,
				actual: lease.kind(),
				origin: lease.origin(),
			});
		}
		if need
			.account
			.as_ref()
			.is_some_and(|account| account != &lease.meta().account)
			|| need
				.principal
				.as_ref()
				.is_some_and(|principal| principal != &lease.meta().principal)
		{
			return Err(CredentialError::InvalidSource);
		}
		if lease.is_expired_at(need.valid_after) {
			return Err(CredentialError::Expired);
		}
		Ok(lease.with_source_tag(Str::new(tag)))
	}
}

impl fmt::Debug for CredentialBroker {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("CredentialBroker")
			.field("plans", &self.plans.len())
			.field("engines", &self.engines)
			.field("invocation", &self.invocation.is_some())
			.finish()
	}
}

impl CredentialBroker {
	/// Tries one plan source; `Err(Unavailable)` means "try the next".
	fn source_lease(
		&self,
		source: &BrokerSource,
		need: &CredentialNeed,
		kind: CredentialKind,
	) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>> {
		match source {
			BrokerSource::Environment(names) => {
				credential_ready(self.environment_lease(names, need, kind))
			},
			BrokerSource::BasicEnvironment { username_names, password_names } => {
				credential_ready(self.basic_environment_lease(username_names, password_names, need))
			},
			BrokerSource::Engine(engine) => match self.engine(*engine) {
				Some(installed) => {
					let need = need.clone();
					let tag = engine.tag();
					map_credential(installed.lease(need.clone()), move |result| {
						result.and_then(|lease| Self::validate_lease(lease, &need, kind, tag))
					})
				},
				None => credential_ready(Err(CredentialError::Unavailable)),
			},
		}
	}
}

impl CredentialSource for CredentialBroker {
	/// Walks the plan's sources in order. Every source that answers
	/// synchronously (environment, invocation, the encrypted store) is
	/// resolved inline; the first source that must perform I/O boxes the
	/// remainder of the walk once.
	fn lease(
		&self,
		need: CredentialNeed,
	) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>> {
		if let Some(lease) = self.invocation_lease(&need) {
			return credential_ready(lease);
		}
		let Some(plan) = self.plans.get(&need.spec) else {
			return credential_ready(Err(CredentialError::InvalidSource));
		};
		let mut sources = plan.sources.iter();
		while let Some(source) = sources.next() {
			let pending = match self.source_lease(source, &need, plan.kind) {
				Either::Left(ready) => {
					let result = ready.into_inner();
					if matches!(&result, Err(CredentialError::Unavailable)) {
						continue;
					}
					return credential_ready(result);
				},
				Either::Right(pending) => pending,
			};
			let kind = plan.kind;
			return Either::Right(
				async move {
					let result = pending.await;
					if !matches!(&result, Err(CredentialError::Unavailable)) {
						return result;
					}
					for source in sources {
						let result = self.source_lease(source, &need, kind).await;
						if !matches!(&result, Err(CredentialError::Unavailable)) {
							return result;
						}
					}
					Err(CredentialError::Unavailable)
				}
				.boxed(),
			);
		}
		credential_ready(Err(CredentialError::Unavailable))
	}

	fn reject<'a>(
		&'a self,
		lease: &'a CredentialLease,
		evidence: AuthRejection,
	) -> CredentialFuture<'a, Result<(), CredentialError>> {
		let Some(tag) = lease.source_tag() else {
			return credential_ready(Err(CredentialError::InvalidSource));
		};
		let kind = match tag {
			ENVIRONMENT_TAG | INVOCATION_TAG => return credential_ready(Ok(())),
			STORED_TAG => EngineKind::Stored,
			ADC_TAG => EngineKind::ApplicationDefault,
			AWS_TAG => EngineKind::Aws,
			OAUTH_TAG => EngineKind::OAuth,
			SESSION_TAG => EngineKind::Session,
			_ => return credential_ready(Err(CredentialError::InvalidSource)),
		};
		match self.engine(kind) {
			Some(engine) => engine.reject(lease, evidence),
			None => credential_ready(Err(CredentialError::Unavailable)),
		}
	}
}

/// Applies a synchronous continuation to a credential future without
/// allocating when the answer is already known.
fn map_credential<'a, T: Send + 'a, U: Send + 'a>(
	future: CredentialFuture<'a, T>,
	map: impl FnOnce(T) -> U + Send + 'a,
) -> CredentialFuture<'a, U> {
	match future {
		Either::Left(ready) => credential_ready(map(ready.into_inner())),
		Either::Right(pending) => Either::Right(pending.map(map).boxed()),
	}
}

/// The kind a static secret (an API key or a session token) is stored and
/// leased as under a catalog authentication of `spec`, or `None` when `spec`
/// takes no static secret.
///
/// A stored row whose kind differs from the one its lease's authentication
/// maps to is rejected by the broker as [`CredentialError::KindMismatch`].
#[must_use]
pub(crate) const fn static_secret_kind(spec: AuthSpecKind) -> Option<CredentialKind> {
	match spec {
		AuthSpecKind::ApiKey => Some(CredentialKind::ApiKey),
		AuthSpecKind::Bearer | AuthSpecKind::OptionalBearer => Some(CredentialKind::Bearer),
		AuthSpecKind::OmpSession => Some(CredentialKind::SessionToken),
		AuthSpecKind::None
		| AuthSpecKind::Basic
		| AuthSpecKind::Oauth
		| AuthSpecKind::GcpAdc
		| AuthSpecKind::AzureAd
		| AuthSpecKind::GithubApp
		| AuthSpecKind::AwsSigv4 => None,
	}
}

/// The kind an API key for `provider` is stored under: the kind of the first
/// authentication its routes lease ([`provider_auth_specs`]) that takes a
/// static key or bearer token. `None` when the catalog does not know the
/// provider or none of those authentications takes one.
fn api_key_kind(catalog: &Catalog, provider: &ProviderId<str>) -> Option<CredentialKind> {
	provider_auth_specs(catalog, provider)
		.filter_map(|spec| static_secret_kind(spec.kind))
		.find(|kind| matches!(kind, CredentialKind::ApiKey | CredentialKind::Bearer))
}

/// Whether some authentication a route of `provider` leases
/// ([`provider_auth_specs`]) takes a credential of `kind`, so a stored row of
/// that kind is usable on at least one of its routes.
pub(crate) fn provider_accepts_kind(
	catalog: &Catalog,
	provider: &ProviderId<str>,
	kind: CredentialKind,
) -> bool {
	provider_auth_specs(catalog, provider).any(|spec| credential_kind(spec.kind) == Some(kind))
}

/// What gives the routes of `provider` a credential when a login would store
/// a static secret of `kind` none of them leases, or `None` when one leases
/// `kind`.
///
/// The remedy names the first route authentication that takes a static
/// secret and an environment variable it reads, so a `models.toml` auth (whose
/// authentication reads `OMP_<PROVIDER>_API_KEY`) names that variable.
pub(crate) fn unleased_login_remedy(
	catalog: &Catalog,
	provider: &ProviderId<str>,
	kind: CredentialKind,
) -> Option<UnleasedLoginRemedy> {
	if provider_accepts_kind(catalog, provider, kind) {
		return None;
	}
	let mut statics = provider_auth_specs(catalog, provider)
		.filter_map(|spec| Some((static_secret_kind(spec.kind)?, spec)));
	let Some((first, _)) = statics.clone().next() else {
		return Some(UnleasedLoginRemedy::NoStaticSecret);
	};
	Some(
		statics
			.find_map(|(kind, spec)| {
				spec
					.credential_sources
					.iter()
					.find_map(|source| match source {
						CredentialSourceSpec::Environment { ordered_names } => {
							ordered_names
								.first()
								.map(|variable| UnleasedLoginRemedy::Variable {
									kind,
									variable: variable.clone(),
								})
						},
						_ => None,
					})
			})
			.unwrap_or(UnleasedLoginRemedy::NoVariable { kind: first }),
	)
}

/// The kind a static secret written as `kind` for `provider` is stored
/// under, or `None` when `kind` names no static secret (OAuth or AWS
/// material, or a kind neither vocabulary knows).
///
/// `kind` is spelled as the store spells it (`api-key`, `bearer`,
/// `session-token`) or as extensions do (`api_key`, `bearer`, `session`).
/// The spelling is parsed, then normalized by [`leased_static_secret_kind`].
/// This is the normalization every control-plane write applies and the
/// stored-kind repair re-applies to rows written before it.
pub(crate) fn stored_static_secret_kind(
	catalog: &Catalog,
	provider: &ProviderId<str>,
	kind: &str,
) -> Option<CredentialKind> {
	let kind = match kind.parse::<CredentialKind>() {
		Ok(kind) => kind,
		Err(_) => kind
			.parse::<ExtensionCredentialKind>()
			.ok()?
			.static_secret()?,
	};
	leased_static_secret_kind(catalog, provider, kind)
}

/// The kind a static secret of `kind` for `provider` is stored under so its
/// routes can lease it, or `None` when `kind` is no static secret.
///
/// A kind some authentication of the provider's routes leases
/// ([`provider_auth_specs`]) is kept, and so is a session token. An API key or
/// bearer token no route leases moves to the other of the two only where that
/// cannot change what reaches the provider, under this configuration or a
/// later one ([`relabeled_static_secret_kind`]): for a provider whose bundled
/// routes treat the two alike ([`bundled_routes_take_keys_as_tokens`]), an API
/// key is stored as `bearer` where its routes take a bearer token, and a bearer
/// token as `api-key` where every leased key authentication sends it as
/// `Authorization: Bearer` (a `models.toml` `apiKey` auth). Otherwise it keeps
/// its kind, and its routes reject it as [`CredentialError::KindMismatch`].
pub(crate) fn leased_static_secret_kind(
	catalog: &Catalog,
	provider: &ProviderId<str>,
	kind: CredentialKind,
) -> Option<CredentialKind> {
	match kind {
		CredentialKind::ApiKey | CredentialKind::Bearer
			if !provider_accepts_kind(catalog, provider, kind) =>
		{
			Some(relabeled_static_secret_kind(catalog, provider, kind).unwrap_or(kind))
		},
		CredentialKind::ApiKey | CredentialKind::Bearer | CredentialKind::SessionToken => Some(kind),
		CredentialKind::Basic | CredentialKind::AwsSigV4 => None,
	}
}

/// The kind an API key or bearer token of `kind`, which no route of
/// `provider` leases, is stored under instead: the provider's API-key kind
/// ([`api_key_kind`]), or `None` when it keeps `kind`.
///
/// The kind is authenticated with the ciphertext and nothing records the one a
/// row was written under, and every launch re-stores rows under the catalog
/// and `models.toml` it runs with. So a move must not change how the secret
/// reaches the provider under any configuration a later launch may run with,
/// and above all not once a `models.toml` auth is removed again. Two rules
/// keep it so:
///
/// - Only a provider whose bundled routes ([`Catalog::embedded`]) treat an API
///   key and a bearer token alike has a key or token moved
///   ([`bundled_routes_take_keys_as_tokens`]): none of them takes an API key,
///   and either one takes a static bearer token (they re-store a key as one
///   themselves) or none takes a bearer credential at all. Every other provider
///   keeps each row's kind whatever a `models.toml` auth says. Anthropic is
///   why: a key moved to `bearer` under `auth = "bearer"` would, once the auth
///   is removed, lease on its OAuth authentication and be sent as an OAuth
///   token, and no rule could move it back.
/// - A bearer token becomes an API key only when every leased API-key
///   authentication sends it exactly as a bearer token is sent (`Authorization:
///   Bearer`), as every `models.toml` auth does; never into a key header, a
///   query parameter, a cookie, or a sealed body.
///
/// A row of such a provider thus follows the configuration: under a
/// `models.toml` `auth = "apiKey"` it is an API key, without it a bearer
/// token, and both are sent as `Authorization: Bearer`. Under one
/// configuration a second pass moves nothing.
fn relabeled_static_secret_kind(
	catalog: &Catalog,
	provider: &ProviderId<str>,
	kind: CredentialKind,
) -> Option<CredentialKind> {
	if !bundled_routes_take_keys_as_tokens(provider) {
		return None;
	}
	match (kind, api_key_kind(catalog, provider)?) {
		(CredentialKind::ApiKey, CredentialKind::Bearer) => Some(CredentialKind::Bearer),
		(CredentialKind::Bearer, CredentialKind::ApiKey) => provider_auth_specs(catalog, provider)
			.filter(|spec| spec.kind == AuthSpecKind::ApiKey)
			.all(sends_as_bearer_token)
			.then_some(CredentialKind::ApiKey),
		_ => None,
	}
}

/// Whether `provider`'s bundled routes ([`Catalog::embedded`]) lease an API
/// key and a bearer token alike, so moving a row between the two kinds cannot
/// change what they send.
///
/// That is when none of them takes an API key, and either the first static
/// secret one of them takes is a bearer token (a key is re-stored as one under
/// them) or none of them takes a bearer credential of any kind (OAuth,
/// application-default, and the like included), as for a keyless provider or
/// one the bundled catalog does not know. A provider whose routes take a key
/// header, query parameter, cookie, or sealed body, or a bearer credential only
/// from OAuth or another non-static source, is not one.
fn bundled_routes_take_keys_as_tokens(provider: &ProviderId<str>) -> bool {
	let bundled = Catalog::embedded();
	!provider_accepts_kind(bundled, provider, CredentialKind::ApiKey)
		&& (api_key_kind(bundled, provider) == Some(CredentialKind::Bearer)
			|| !provider_accepts_kind(bundled, provider, CredentialKind::Bearer))
}

/// The kind of static secret the next launch's stored-kind repair re-stores,
/// for `provider` under `catalog`, as a kind one of `specs` leases: a row
/// stored as it is unusable on a route leasing `specs` until then, and usable
/// after. `None` when the repair moves no row of `provider` to a kind one of
/// `specs` leases.
pub(crate) fn restored_static_secret_kind<'c>(
	catalog: &Catalog,
	provider: &ProviderId<str>,
	specs: impl Iterator<Item = &'c AuthSpec> + Clone,
) -> Option<CredentialKind> {
	[CredentialKind::ApiKey, CredentialKind::Bearer]
		.into_iter()
		.find(|&stored| {
			leased_static_secret_kind(catalog, provider, stored).is_some_and(|restored| {
				restored != stored
					&& specs
						.clone()
						.any(|spec| credential_kind(spec.kind) == Some(restored))
			})
		})
}

/// Whether `spec` places its credential where a bearer token goes: the
/// `Authorization` header behind a `Bearer ` prefix, and nowhere else.
fn sends_as_bearer_token(spec: &AuthSpec) -> bool {
	spec.query_parameter.is_none()
		&& spec.sealed_body.is_none()
		&& spec
			.header_name
			.as_deref()
			.is_some_and(|header| header.eq_ignore_ascii_case("authorization"))
		&& spec
			.prefix
			.as_deref()
			.is_some_and(|prefix| prefix.trim_end().eq_ignore_ascii_case("bearer"))
}

/// Every catalog authentication a request for `provider` may lease under.
///
/// That is, for each of its routes in catalog route order, the route's own
/// authentication and the provider-declared ones its codec also leases (those
/// leased before it first); an authentication several routes share
/// repeats. A provider-declared authentication no route leases is left out: a
/// `models.toml` auth replaces the routes' own, and a credential only the
/// declared one takes is never leased.
pub fn provider_auth_specs<'c>(
	catalog: &'c Catalog,
	provider: &'c ProviderId<str>,
) -> impl Iterator<Item = &'c AuthSpec> + Clone + 'c {
	catalog
		.routes()
		.iter()
		.filter(move |route| route.provider.as_str() == provider.as_str())
		.flat_map(move |route| route_auth_specs(catalog, route))
}

/// The catalog authentications a request on `route` leases under: the
/// route's own and the provider-declared ones its codec also leases
/// ([`declared_auth_lease`]), those leased before it first.
fn route_auth_specs<'c>(
	catalog: &'c Catalog,
	route: &'c RouteDef,
) -> impl Iterator<Item = &'c AuthSpec> + Clone + 'c {
	let lease = declared_auth_lease(route.codec.as_str());
	let declared = move |first: bool| {
		lease
			.filter(move |lease| lease.first == first)
			.into_iter()
			.flat_map(move |lease| {
				catalog
					.provider(&route.provider)
					.into_iter()
					.flat_map(|provider| provider.auth.iter())
					.filter(move |id| *id != &route.auth)
					.filter_map(move |id| catalog.auth_spec(id))
					.filter(move |spec| (lease.accepts)(spec.kind))
			})
	};
	declared(true)
		.chain(catalog.auth_spec(&route.auth))
		.chain(declared(false))
}

/// How a route leases the authentications its provider declares besides
/// the route's own ([`declared_auth_lease`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeclaredAuthLease {
	/// Whether the route also leases a declared authentication of this kind.
	pub accepts: fn(AuthSpecKind) -> bool,
	/// Whether it leases those before its own authentication.
	pub first:   bool,
}

/// How a route of `codec` leases its provider's declared authentications,
/// or `None` when it leases only its own.
///
/// Anthropic routes also take an OAuth or bearer token after their key
/// header, Bedrock Converse a bearer token before `SigV4`, Bedrock Mantle
/// `SigV4` after its own, and Perplexity search an OAuth token first. The
/// route lease plan and every stored-kind decision read this one table.
pub(crate) fn declared_auth_lease(codec: &str) -> Option<DeclaredAuthLease> {
	let (accepts, first): (fn(AuthSpecKind) -> bool, bool) = match codec {
		"anthropic" => (
			|kind| {
				matches!(
					kind,
					AuthSpecKind::Oauth | AuthSpecKind::Bearer | AuthSpecKind::OptionalBearer
				)
			},
			false,
		),
		"bedrock-converse" => {
			(|kind| matches!(kind, AuthSpecKind::Bearer | AuthSpecKind::OptionalBearer), true)
		},
		"bedrock-mantle" => (|kind| kind == AuthSpecKind::AwsSigv4, false),
		"search-perplexity" => (|kind| kind == AuthSpecKind::Oauth, true),
		_ => return None,
	};
	Some(DeclaredAuthLease { accepts, first })
}

const fn credential_kind(kind: AuthSpecKind) -> Option<CredentialKind> {
	match kind {
		AuthSpecKind::None => None,
		AuthSpecKind::ApiKey => Some(CredentialKind::ApiKey),
		AuthSpecKind::Basic => Some(CredentialKind::Basic),
		AuthSpecKind::Bearer
		| AuthSpecKind::OptionalBearer
		| AuthSpecKind::Oauth
		| AuthSpecKind::GcpAdc
		| AuthSpecKind::AzureAd
		| AuthSpecKind::GithubApp => Some(CredentialKind::Bearer),
		AuthSpecKind::AwsSigv4 => Some(CredentialKind::AwsSigV4),
		AuthSpecKind::OmpSession => Some(CredentialKind::SessionToken),
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::atomic::{AtomicUsize, Ordering},
		time::SystemTime,
	};

	use bytes::Bytes;
	use http::Request;
	use omp_core::ExposeSecret as _;
	use parking_lot::Mutex;

	use super::{super::lease::AuthRejectionKind, *};
	use crate::{
		auth::AuthSpec,
		id::{AccountId, PrincipalId},
	};

	#[derive(Debug, Default)]
	struct EmptyEnvironment;

	impl CredentialEnvironment for EmptyEnvironment {
		fn read(&self, _: &str) -> Result<Option<SecretString>, CredentialError> {
			Ok(None)
		}
	}
	#[derive(Debug, Default)]
	struct TrackingEnvironment {
		reads: AtomicUsize,
	}

	impl CredentialEnvironment for TrackingEnvironment {
		fn read(&self, _: &str) -> Result<Option<SecretString>, CredentialError> {
			self.reads.fetch_add(1, Ordering::Relaxed);
			Ok(None)
		}
	}

	#[derive(Debug, Default)]
	struct TrackingStore {
		leases: AtomicUsize,
	}

	impl CredentialSource for TrackingStore {
		fn lease(
			&self,
			_: CredentialNeed,
		) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>> {
			self.leases.fetch_add(1, Ordering::Relaxed);
			credential_ready(Err(CredentialError::Unavailable))
		}

		fn reject<'a>(
			&'a self,
			_: &'a CredentialLease,
			_: AuthRejection,
		) -> CredentialFuture<'a, Result<(), CredentialError>> {
			credential_ready(Ok(()))
		}
	}

	#[tokio::test]
	async fn invocation_key_is_provider_scoped_and_bypasses_external_sources() {
		let catalog = Catalog::embedded();
		let selected = catalog
			.providers()
			.iter()
			.find_map(|provider| {
				provider.auth.iter().find_map(|spec| {
					let kind = credential_kind(catalog.auth_spec(spec)?.kind)?;
					matches!(
						kind,
						CredentialKind::ApiKey | CredentialKind::Bearer | CredentialKind::SessionToken
					)
					.then(|| (provider, spec.clone()))
				})
			})
			.expect("provider with scalar authentication");
		let selected_kind = credential_kind(
			catalog
				.auth_spec(&selected.1)
				.expect("selected authentication spec")
				.kind,
		)
		.expect("selected scalar authentication kind");
		let other = catalog
			.auth_specs()
			.iter()
			.find(|spec| {
				spec.id != selected.1
					&& credential_kind(spec.kind).is_some()
					&& !selected.0.auth.contains(&spec.id)
			})
			.expect("authentication outside selected provider");
		let environment = Arc::new(TrackingEnvironment::default());
		let store = Arc::new(TrackingStore::default());
		let broker =
			CredentialBroker::from_catalog(catalog, environment.clone(), CredentialBrokerEngines {
				stored: Some(store.clone()),
				..CredentialBrokerEngines::default()
			})
			.expect("base broker")
			.with_api_key_override(catalog, &selected.0.id, SecretString::from("invocation-only-key"))
			.expect("provider override");
		let need = |spec| CredentialNeed {
			spec,
			account: Some(AccountId::from("selected-account")),
			principal: Some(PrincipalId::from("selected-principal")),
			valid_after: SystemTime::UNIX_EPOCH,
		};

		let lease = broker
			.lease(need(selected.1.clone()))
			.await
			.expect("invocation lease");
		assert_eq!(lease.scalar_secret().expect("scalar key").expose_secret(), "invocation-only-key");
		assert_eq!(lease.kind(), selected_kind);
		assert_eq!(lease.meta().account.as_str(), "selected-account");
		assert_eq!(lease.meta().principal.as_str(), "selected-principal");
		assert_eq!(
			broker
				.refresh_lease(&lease, need(selected.1.clone()))
				.await
				.expect_err("invocation credentials are nonrenewable"),
			CredentialError::Unavailable,
		);
		assert_eq!(
			broker
				.refresh_account(need(selected.1.clone()))
				.await
				.expect_err("account has no renewable stored credential"),
			CredentialError::Unavailable,
		);
		assert_eq!(environment.reads.load(Ordering::Relaxed), 0);
		assert_eq!(store.leases.load(Ordering::Relaxed), 0);

		assert_eq!(
			broker.lease(need(other.id.clone())).await.unwrap_err(),
			CredentialError::Unavailable
		);
		assert!(
			environment.reads.load(Ordering::Relaxed) > 0 || store.leases.load(Ordering::Relaxed) > 0
		);
	}

	#[tokio::test]
	async fn explicit_bedrock_key_remains_bearer_instead_of_resolving_to_sigv4() {
		let catalog = Catalog::embedded();
		let provider = catalog
			.provider(omp_catalog::ProviderId::from_ref("amazon-bedrock"))
			.expect("embedded Bedrock provider");
		let _route = provider
			.routes
			.iter()
			.filter_map(|id| catalog.route(id))
			.find(|route| route.codec.as_str() == "bedrock-converse")
			.expect("Bedrock Converse route");
		let bearer = provider
			.auth
			.iter()
			.find(|id| {
				catalog
					.auth_spec(id)
					.is_some_and(|auth| auth.kind == AuthSpecKind::Bearer)
			})
			.expect("Bedrock bearer alternative");
		let broker = CredentialBroker::from_catalog(
			catalog,
			Arc::new(EmptyEnvironment),
			CredentialBrokerEngines::default(),
		)
		.expect("base broker")
		.with_api_key_override(catalog, &provider.id, SecretString::from("explicit-bedrock-token"))
		.expect("Bedrock bearer override");

		let lease = broker
			.lease(CredentialNeed {
				spec:        bearer.clone(),
				account:     None,
				principal:   None,
				valid_after: SystemTime::UNIX_EPOCH,
			})
			.await
			.expect("explicit bearer lease");
		assert_eq!(lease.kind(), CredentialKind::Bearer);
		assert_eq!(
			lease.scalar_secret().expect("bearer token").expose_secret(),
			"explicit-bedrock-token",
		);

		let catalog_auth = catalog.auth_spec(bearer).expect("catalog bearer auth");
		let runtime_auth =
			AuthSpec::from_catalog(catalog_auth, None, None).expect("runtime bearer auth");
		let applied = lease
			.prepare(&runtime_auth, SystemTime::UNIX_EPOCH)
			.expect("AWS bearer alternative prepares");
		let mut request = Request::builder()
			.uri("https://bedrock-runtime.us-east-1.amazonaws.com/model/test/converse-stream")
			.body(Bytes::new())
			.expect("request");
		applied
			.finalize_buffered(&mut request)
			.expect("AWS bearer alternative applies");
		assert_eq!(request.headers()["authorization"], "Bearer explicit-bedrock-token");
	}

	#[test]
	fn embedded_catalog_compiles_one_exact_plan_per_authenticated_spec() {
		let catalog = Catalog::embedded();
		let broker = CredentialBroker::from_catalog(
			catalog,
			Arc::new(EmptyEnvironment),
			CredentialBrokerEngines::default(),
		)
		.expect("credential plans");
		let authenticated = catalog
			.auth_specs()
			.iter()
			.filter(|auth| credential_kind(auth.kind).is_some())
			.count();
		assert_eq!(broker.plans.len(), authenticated);
		for auth in catalog
			.auth_specs()
			.iter()
			.filter(|auth| credential_kind(auth.kind).is_some())
		{
			let plan = broker
				.plans
				.get(&auth.id)
				.expect("plan by exact auth identity");
			assert_eq!(plan.sources.len(), auth.credential_sources.len());
		}
	}

	#[derive(Debug)]
	struct OrderedEnvironment {
		calls: Mutex<Vec<Str>>,
	}

	impl CredentialEnvironment for OrderedEnvironment {
		fn read(&self, name: &str) -> Result<Option<SecretString>, CredentialError> {
			self.calls.lock().push(name.into());
			Ok((name == "ANTHROPIC_API_KEY").then(|| SecretString::from("secret".to_owned())))
		}
	}

	#[tokio::test]
	async fn environment_names_are_tried_in_declared_order() {
		let spec = AuthSpecId::new("ordered");
		let environment = Arc::new(OrderedEnvironment { calls: Mutex::new(Vec::new()) });
		let broker = CredentialBroker {
			plans:       Arc::new(BTreeMap::from([(spec.clone(), BrokerPlan {
				kind:    CredentialKind::ApiKey,
				sources: vec![BrokerSource::Environment(
					vec![sf!("OMP_ANTHROPIC_API_KEY"), sf!("ANTHROPIC_API_KEY")].into_boxed_slice(),
				)]
				.into_boxed_slice(),
			})])),
			environment: environment.clone(),
			engines:     CredentialBrokerEngines::default(),
			invocation:  None,
		};
		let lease = broker
			.lease(CredentialNeed {
				spec,
				account: Some(AccountId::from("account")),
				principal: Some(PrincipalId::from("principal")),
				valid_after: SystemTime::UNIX_EPOCH,
			})
			.await
			.expect("second source");
		assert_eq!(lease.kind(), CredentialKind::ApiKey);
		assert_eq!(*environment.calls.lock(), vec![
			sf!("OMP_ANTHROPIC_API_KEY"),
			sf!("ANTHROPIC_API_KEY")
		]);
		assert!(!format!("{broker:?} {lease:?}").contains("secret"));
	}

	/// Route execution with no durable account selects a brokered identity
	/// (`account: None`); a vendor environment name must still yield a lease.
	#[tokio::test]
	async fn brokered_need_without_account_leases_vendor_environment_name() {
		let spec = AuthSpecId::new("ordered");
		let environment = Arc::new(OrderedEnvironment { calls: Mutex::new(Vec::new()) });
		let broker = CredentialBroker {
			plans:       Arc::new(BTreeMap::from([(spec.clone(), BrokerPlan {
				kind:    CredentialKind::ApiKey,
				sources: vec![BrokerSource::Environment(
					vec![sf!("OMP_ANTHROPIC_API_KEY"), sf!("ANTHROPIC_API_KEY")].into_boxed_slice(),
				)]
				.into_boxed_slice(),
			})])),
			environment: environment.clone(),
			engines:     CredentialBrokerEngines::default(),
			invocation:  None,
		};
		let lease = broker
			.lease(CredentialNeed {
				spec,
				account: None,
				principal: None,
				valid_after: SystemTime::UNIX_EPOCH,
			})
			.await
			.expect("brokered environment lease");
		assert_eq!(lease.kind(), CredentialKind::ApiKey);
		assert_eq!(lease.scalar_secret().expect("scalar key").expose_secret(), "secret");
		assert_eq!(lease.source_tag(), Some(ENVIRONMENT_TAG));
		assert_eq!(lease.meta().account.as_str(), ENVIRONMENT_TAG);
		assert_eq!(lease.meta().principal.as_str(), "ANTHROPIC_API_KEY");
		assert_eq!(*environment.calls.lock(), vec![
			sf!("OMP_ANTHROPIC_API_KEY"),
			sf!("ANTHROPIC_API_KEY")
		]);
	}

	/// A stored source answering an API key for a bearer authentication.
	#[derive(Debug)]
	struct ApiKeyStore;

	impl CredentialSource for ApiKeyStore {
		fn lease(
			&self,
			need: CredentialNeed,
		) -> CredentialFuture<'_, Result<CredentialLease, CredentialError>> {
			let meta = LeaseMeta {
				account:    need.account.unwrap_or_else(|| AccountId::from("account")),
				principal:  need
					.principal
					.unwrap_or_else(|| PrincipalId::from("principal")),
				generation: 1,
				expires_at: None,
			};
			credential_ready(Ok(CredentialLease::api_key(meta, SecretString::from("fake-key"))
				.with_origin(crate::auth::LeaseOrigin::StoredSecret)))
		}

		fn reject<'a>(
			&'a self,
			_: &'a CredentialLease,
			_: AuthRejection,
		) -> CredentialFuture<'a, Result<(), CredentialError>> {
			credential_ready(Ok(()))
		}
	}

	/// A stored credential whose kind is not the one the authentication
	/// requires names both kinds instead of an opaque invalid source.
	#[tokio::test]
	async fn wrong_kind_stored_credential_is_a_typed_kind_mismatch() {
		let spec = AuthSpecId::new("bearer-only");
		let broker = CredentialBroker {
			plans:       Arc::new(BTreeMap::from([(spec.clone(), BrokerPlan {
				kind:    CredentialKind::Bearer,
				sources: vec![BrokerSource::Engine(EngineKind::Stored)].into_boxed_slice(),
			})])),
			environment: Arc::new(EmptyEnvironment),
			engines:     CredentialBrokerEngines {
				stored: Some(Arc::new(ApiKeyStore)),
				..Default::default()
			},
			invocation:  None,
		};
		let error = broker
			.lease(CredentialNeed {
				spec,
				account: Some(AccountId::from("huggingface:agent-db")),
				principal: None,
				valid_after: SystemTime::UNIX_EPOCH,
			})
			.await
			.expect_err("an API key cannot satisfy a bearer authentication");
		assert_eq!(error, CredentialError::KindMismatch {
			expected: CredentialKind::Bearer,
			actual:   CredentialKind::ApiKey,
			origin:   crate::auth::LeaseOrigin::StoredSecret,
		});
		assert_eq!(error.to_string(), "credential is api-key but the authentication requires bearer");
	}

	/// The stored kind of a static secret follows the catalog authentication
	/// kind, and a provider's API key takes the kind of its first
	/// authentication an API key satisfies.
	#[test]
	fn static_secret_kinds_follow_the_catalog_authentication() {
		assert_eq!(static_secret_kind(AuthSpecKind::ApiKey), Some(CredentialKind::ApiKey));
		assert_eq!(static_secret_kind(AuthSpecKind::Bearer), Some(CredentialKind::Bearer));
		assert_eq!(static_secret_kind(AuthSpecKind::OptionalBearer), Some(CredentialKind::Bearer));
		assert_eq!(static_secret_kind(AuthSpecKind::OmpSession), Some(CredentialKind::SessionToken));
		assert_eq!(static_secret_kind(AuthSpecKind::Oauth), None);
		let catalog = Catalog::embedded();
		let kind = |provider| api_key_kind(catalog, ProviderId::from_ref(provider));
		assert_eq!(kind("huggingface"), Some(CredentialKind::Bearer));
		assert_eq!(kind("anthropic"), Some(CredentialKind::ApiKey));
		assert_eq!(kind("v1-only-provider"), None);
		let accepts =
			|provider, kind| provider_accepts_kind(catalog, ProviderId::from_ref(provider), kind);
		assert!(!accepts("huggingface", CredentialKind::ApiKey));
		assert!(accepts("huggingface", CredentialKind::Bearer));
		// Anthropic takes a key header and an OAuth bearer token.
		assert!(accepts("anthropic", CredentialKind::ApiKey));
		assert!(accepts("anthropic", CredentialKind::Bearer));
		// Google takes only a key.
		assert!(accepts("google", CredentialKind::ApiKey));
		assert!(!accepts("google", CredentialKind::Bearer));
		assert_eq!("api-key".parse::<CredentialKind>(), Ok(CredentialKind::ApiKey));
		assert_eq!(<&'static str>::from(CredentialKind::SessionToken), "session-token");
	}

	/// A provider's authentications are the ones its routes lease: each
	/// route's own and the declared ones its codec leases too. A declared
	/// authentication no route leases takes no part in a stored kind.
	#[test]
	fn provider_authentications_are_the_ones_its_routes_lease() {
		let catalog = Catalog::embedded();
		for provider in catalog.providers() {
			let leased = provider_auth_specs(catalog, &provider.id)
				.map(|spec| spec.id.clone())
				.collect::<std::collections::BTreeSet<_>>();
			let mut expected = std::collections::BTreeSet::new();
			for route in catalog
				.routes()
				.iter()
				.filter(|route| route.provider == provider.id)
			{
				expected.insert(route.auth.clone());
				let Some(lease) = declared_auth_lease(route.codec.as_str()) else {
					continue;
				};
				expected.extend(
					provider
						.auth
						.iter()
						.filter(|id| {
							catalog
								.auth_spec(id)
								.is_some_and(|spec| (lease.accepts)(spec.kind))
						})
						.cloned(),
				);
			}
			assert_eq!(leased, expected, "{}", provider.id);
		}
		// A written key or token keeps a kind some route leases. One that
		// moves lands on a kind a route leases, and only an API key of a
		// provider whose routes take no key moves: in the bundled catalog a
		// bearer token never moves, since no bundled key authentication sends a
		// key as a bearer token.
		for provider in catalog.providers() {
			if let Some(kind) = api_key_kind(catalog, &provider.id) {
				assert!(provider_accepts_kind(catalog, &provider.id, kind), "{}", provider.id);
			}
			for written in [CredentialKind::ApiKey, CredentialKind::Bearer] {
				let stored =
					leased_static_secret_kind(catalog, &provider.id, written).expect("a static secret");
				if stored == written {
					continue;
				}
				assert_eq!(
					(written, stored),
					(CredentialKind::ApiKey, CredentialKind::Bearer),
					"{}",
					provider.id
				);
				assert!(provider_accepts_kind(catalog, &provider.id, stored), "{}", provider.id);
				assert!(
					!provider_accepts_kind(catalog, &provider.id, CredentialKind::ApiKey),
					"{}: a key moved although a route takes one",
					provider.id
				);
				assert!(bundled_routes_take_keys_as_tokens(&provider.id), "{}", provider.id);
				// The move is idempotent: the re-stored kind stays.
				assert_eq!(
					leased_static_secret_kind(catalog, &provider.id, stored),
					Some(stored),
					"{}",
					provider.id
				);
			}
		}
	}

	/// Only a provider whose bundled routes lease an API key and a bearer
	/// token alike has a row moved between the two: one whose routes take a
	/// key in a header, query parameter, or cookie, or a bearer credential only
	/// from OAuth or application-default credentials, keeps every row's kind,
	/// so no `models.toml` auth can change how its stored secrets are sent once
	/// the auth is removed.
	#[test]
	fn only_providers_whose_bundled_routes_take_keys_as_tokens_move_rows() {
		let moves = |provider| bundled_routes_take_keys_as_tokens(ProviderId::from_ref(provider));
		// A key header (`x-api-key`, `api-key`, `X-Subscription-Token`), the
		// `key` query parameter, a cookie.
		for provider in ["anthropic", "azure", "brave", "google", "perplexity-cookie"] {
			assert!(!moves(provider), "{provider}");
		}
		// A bearer credential only from OAuth or application-default
		// credentials.
		for provider in ["google-antigravity", "google-gemini-cli", "kimi-code", "google-vertex"] {
			assert!(!moves(provider), "{provider}");
		}
		// A static bearer token, alone or beside OAuth; a keyless provider; one
		// only a `models.toml` defines, whose every auth sends `Authorization:
		// Bearer`.
		for provider in ["huggingface", "zai", "github-copilot", "duckduckgo", "v1-only-provider"] {
			assert!(moves(provider), "{provider}");
		}
	}

	/// The kind a restart re-stores is the one a mismatched row of the
	/// provider is stored as, and only for a route leasing the kind it moves
	/// to.
	#[test]
	fn a_restart_restores_only_a_row_the_repair_moves_to_a_leased_kind() {
		let catalog = Catalog::embedded();
		let restored = |provider| {
			let provider = ProviderId::from_ref(provider);
			restored_static_secret_kind(catalog, provider, provider_auth_specs(catalog, provider))
		};
		assert_eq!(restored("huggingface"), Some(CredentialKind::ApiKey));
		assert_eq!(restored("anthropic"), None);
		assert_eq!(restored("google"), None);
		let huggingface = ProviderId::from_ref("huggingface");
		let no_specs = std::iter::empty::<&omp_catalog::provider::AuthSpec>();
		assert_eq!(restored_static_secret_kind(catalog, huggingface, no_specs), None);
	}

	/// A login of a kind no route leases is told what gives the routes a
	/// credential: the environment variable the first authentication taking a
	/// static secret reads, that authentication's kind when it reads none, or
	/// that the routes take no stored key or token.
	#[test]
	fn an_unleased_login_names_what_the_routes_take() {
		let catalog = Catalog::embedded();
		let huggingface = ProviderId::from_ref("huggingface");
		assert_eq!(unleased_login_remedy(catalog, huggingface, CredentialKind::Bearer), None);
		let token = provider_auth_specs(catalog, huggingface)
			.next()
			.expect("Hugging Face's authentication")
			.clone();
		let variable = token
			.credential_sources
			.iter()
			.find_map(|source| match source {
				CredentialSourceSpec::Environment { ordered_names } => ordered_names.first().cloned(),
				_ => None,
			})
			.expect("Hugging Face's variable");
		assert_eq!(
			unleased_login_remedy(catalog, huggingface, CredentialKind::ApiKey),
			Some(UnleasedLoginRemedy::Variable { kind: CredentialKind::Bearer, variable })
		);
		assert_eq!(
			unleased_login_remedy(catalog, ProviderId::from_ref("duckduckgo"), CredentialKind::ApiKey),
			Some(UnleasedLoginRemedy::NoStaticSecret)
		);

		// Hugging Face's authentication reading only the store.
		let stored_only = omp_catalog::provider::AuthSpec {
			credential_sources: Box::new([CredentialSourceSpec::Stored]),
			..token
		};
		let overlay = omp_catalog::CatalogOverlayBuilder::new(omp_catalog::ProvenanceSource {
			kind:           omp_catalog::ProvenanceKind::Configured,
			origin:         "test".into(),
			revision:       None,
			confidence:     omp_catalog::EvidenceConfidence::Declared,
			observed_at_ms: None,
		})
		.with_auth_spec(stored_only)
		.build();
		let catalog = catalog
			.with_overlay_stack(
				&omp_catalog::OverlayStack::from_layers([(
					omp_catalog::OverlaySource::UserConfig,
					overlay,
				)]),
				omp_catalog::UnsafeTrustScope::ALL,
			)
			.expect("overlaid catalog");
		assert_eq!(
			unleased_login_remedy(&catalog, huggingface, CredentialKind::ApiKey),
			Some(UnleasedLoginRemedy::NoVariable { kind: CredentialKind::Bearer })
		);
	}

	/// A bearer token becomes an API key only where the key authentication
	/// sends it as `Authorization: Bearer`, never into a key header, a query
	/// parameter, or a sealed body.
	#[test]
	fn only_an_authorization_bearer_key_takes_a_bearer_token() {
		let catalog = Catalog::embedded();
		let spec = |header: Option<&'static str>, prefix: Option<&'static str>| {
			omp_catalog::provider::AuthSpec {
				header_name: header.map(Str::new_static),
				prefix: prefix.map(Str::new_static),
				query_parameter: None,
				..catalog
					.provider(ProviderId::from_ref("google"))
					.and_then(|google| catalog.auth_spec(&google.auth[0]))
					.expect("google key auth")
					.clone()
			}
		};
		assert!(sends_as_bearer_token(&spec(Some("authorization"), Some("Bearer "))));
		assert!(sends_as_bearer_token(&spec(Some("Authorization"), Some("bearer "))));
		assert!(!sends_as_bearer_token(&spec(Some("x-api-key"), None)));
		assert!(!sends_as_bearer_token(&spec(Some("authorization"), None)));
		assert!(!sends_as_bearer_token(&spec(Some("authorization"), Some("Token "))));
		// Google's own key goes in the `key` query parameter.
		let google = catalog
			.provider(ProviderId::from_ref("google"))
			.and_then(|google| catalog.auth_spec(&google.auth[0]))
			.expect("google key auth");
		assert!(google.query_parameter.is_some(), "{google:?}");
		assert!(!sends_as_bearer_token(google));

		// The stored kind follows it. Hugging Face's bundled routes take keys as
		// tokens; with its authentication replaced by a key authentication, a
		// bearer token is stored as a key where that sends it as `Authorization:
		// Bearer`, and keeps its kind where it would go in a header of its own.
		let huggingface = ProviderId::from_ref("huggingface");
		let token = provider_auth_specs(catalog, huggingface)
			.next()
			.expect("Hugging Face's authentication")
			.clone();
		let keyed = |header: &'static str, prefix: Option<&'static str>| {
			let key = omp_catalog::provider::AuthSpec {
				kind: AuthSpecKind::ApiKey,
				header_name: Some(Str::new_static(header)),
				prefix: prefix.map(Str::new_static),
				..token.clone()
			};
			let overlay = omp_catalog::CatalogOverlayBuilder::new(omp_catalog::ProvenanceSource {
				kind:           omp_catalog::ProvenanceKind::Configured,
				origin:         "test".into(),
				revision:       None,
				confidence:     omp_catalog::EvidenceConfidence::Declared,
				observed_at_ms: None,
			})
			.with_auth_spec(key)
			.build();
			catalog
				.with_overlay_stack(
					&omp_catalog::OverlayStack::from_layers([(
						omp_catalog::OverlaySource::UserConfig,
						overlay,
					)]),
					omp_catalog::UnsafeTrustScope::ALL,
				)
				.expect("overlaid catalog")
		};
		let stored = |catalog: &Catalog| {
			leased_static_secret_kind(catalog, huggingface, CredentialKind::Bearer)
		};
		assert_eq!(stored(&keyed("authorization", Some("Bearer "))), Some(CredentialKind::ApiKey));
		assert_eq!(stored(&keyed("x-api-key", None)), Some(CredentialKind::Bearer));
	}

	/// A written static secret is stored under the kind its provider leases:
	/// an API key the provider takes only as a bearer token becomes `bearer`,
	/// the extension spellings become the store's, and anything else is kept
	/// or is no static secret at all.
	#[test]
	fn written_static_secrets_take_the_kind_their_provider_leases() {
		let catalog = Catalog::embedded();
		let stored =
			|provider, kind| stored_static_secret_kind(catalog, ProviderId::from_ref(provider), kind);
		assert_eq!(stored("huggingface", "api-key"), Some(CredentialKind::Bearer));
		assert_eq!(stored("huggingface", "api_key"), Some(CredentialKind::Bearer));
		assert_eq!(stored("huggingface", "bearer"), Some(CredentialKind::Bearer));
		assert_eq!(stored("anthropic", "api-key"), Some(CredentialKind::ApiKey));
		assert_eq!(stored("anthropic", "api_key"), Some(CredentialKind::ApiKey));
		// A bearer token for a provider whose routes take only a key keeps its
		// kind: Google's key goes in a query parameter, where an OAuth access
		// token must not, so its routes refuse the token.
		assert_eq!(stored("google", "bearer"), Some(CredentialKind::Bearer));
		assert_eq!(stored("google", "api_key"), Some(CredentialKind::ApiKey));
		// A kind some authentication of the provider leases is kept.
		assert_eq!(stored("anthropic", "bearer"), Some(CredentialKind::Bearer));
		assert_eq!(stored("anthropic", "session"), Some(CredentialKind::SessionToken));
		assert_eq!(stored("anthropic", "session-token"), Some(CredentialKind::SessionToken));
		// An unknown provider keeps the key as written.
		assert_eq!(stored("v1-only-provider", "api_key"), Some(CredentialKind::ApiKey));
		for kind in ["oauth", "aws", "oauth-renewable-v1", "basic", "aws-sigv4", "API_KEY"] {
			assert_eq!(stored("huggingface", kind), None, "{kind}");
		}
	}

	/// Environment, invocation, and encrypted-store sources answer without
	/// touching the heap: the broker resolves the plan walk inline and only a
	/// source that performs I/O boxes.
	#[test]
	fn synchronous_sources_lease_and_reject_without_boxing() {
		let spec = AuthSpecId::new("ordered");
		let stored: Arc<dyn CredentialSource> = Arc::new(TrackingStore::default());
		let broker = CredentialBroker {
			plans:       Arc::new(BTreeMap::from([(spec.clone(), BrokerPlan {
				kind:    CredentialKind::ApiKey,
				sources: vec![
					BrokerSource::Engine(EngineKind::Stored),
					BrokerSource::Environment(vec![sf!("ANTHROPIC_API_KEY")].into_boxed_slice()),
				]
				.into_boxed_slice(),
			})])),
			environment: Arc::new(OrderedEnvironment { calls: Mutex::new(Vec::new()) }),
			engines:     CredentialBrokerEngines { stored: Some(stored), ..Default::default() },
			invocation:  None,
		};
		let need = CredentialNeed {
			spec,
			account: None,
			principal: None,
			valid_after: SystemTime::UNIX_EPOCH,
		};
		let Either::Left(ready) = broker.lease(need.clone()) else {
			panic!("stored miss followed by environment hit must resolve inline");
		};
		let lease = ready.into_inner().expect("environment lease");
		assert_eq!(lease.source_tag(), Some(ENVIRONMENT_TAG));
		let Either::Left(rejected) = broker.reject(&lease, AuthRejection {
			kind:        AuthRejectionKind::Unauthorized,
			status:      Some(403),
			code:        None,
			refreshable: false,
		}) else {
			panic!("environment rejection is synchronous");
		};
		assert_eq!(rejected.into_inner(), Ok(()));
		let Either::Left(unknown) =
			broker.lease(CredentialNeed { spec: AuthSpecId::new("missing"), ..need })
		else {
			panic!("unknown spec is rejected inline");
		};
		assert_eq!(unknown.into_inner().map(|_| ()), Err(CredentialError::InvalidSource));
	}
}
