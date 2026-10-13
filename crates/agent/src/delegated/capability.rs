//! Capability discovery: what an executor can do and which CLI versions it
//! supports.

use super::{DelegatedPolicy, ExecutorId};

/// Who owns authentication and the agent loop for a session.
///
/// The mode is chosen by the caller and never changed by the contract: a
/// delegated executor only ever offers [`ExecutionMode::CliManaged`], and a
/// request for the other mode is an error, not a fallback.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::EnumString, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum ExecutionMode {
	/// OMP calls a provider API with OMP-managed credentials through `omp-ai`.
	DirectApi,
	/// OMP starts the official CLI; the CLI owns its login, provider calls and
	/// agent loop.
	CliManaged,
}

/// One optional behavior an executor may or may not provide.
///
/// An executor advertises exactly what it can do. The host rejects events that
/// require a feature the executor did not advertise.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::EnumString, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
#[repr(u8)]
pub enum Feature {
	/// A session can be resumed by its CLI-assigned identity.
	Resume = 0,
	/// A running turn can be interrupted cooperatively.
	Interrupt = 1,
	/// The CLI asks for permission before privileged actions and accepts an
	/// answer.
	Permissions = 2,
	/// Agent text arrives incrementally ([`DelegatedEvent::AgentTextDelta`]).
	///
	/// [`DelegatedEvent::AgentTextDelta`]: super::DelegatedEvent::AgentTextDelta
	StreamingText = 3,
	/// Command and tool activity is reported as events.
	ToolEvents = 4,
	/// Token usage is reported by the CLI.
	UsageReporting = 5,
	/// The agent runs rooted at the working directory OMP passes in.
	WorkingDirectoryInheritance = 6,
}

const _: () = assert!(
	Feature::WorkingDirectoryInheritance as u8 <= 7,
	"FeatureSet is a u8 bitmask; widen it before adding an eighth-plus feature"
);

impl Feature {
	const fn bit(self) -> u8 {
		1_u8 << (self as u8)
	}
}

/// A set of [`Feature`]s.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeatureSet(u8);

impl FeatureSet {
	/// The set that advertises nothing.
	pub const EMPTY: Self = Self(0);

	/// Every defined feature.
	pub const FULL: Self = Self::EMPTY
		.with(Feature::Resume)
		.with(Feature::Interrupt)
		.with(Feature::Permissions)
		.with(Feature::StreamingText)
		.with(Feature::ToolEvents)
		.with(Feature::UsageReporting)
		.with(Feature::WorkingDirectoryInheritance);

	/// Returns this set plus `feature`.
	#[must_use]
	pub const fn with(self, feature: Feature) -> Self {
		Self(self.0 | feature.bit())
	}

	/// Returns this set minus `feature`.
	#[must_use]
	pub const fn without(self, feature: Feature) -> Self {
		Self(self.0 & !feature.bit())
	}

	/// Whether `feature` is advertised.
	#[must_use]
	pub const fn contains(self, feature: Feature) -> bool {
		self.0 & feature.bit() != 0
	}
}

/// A CLI version.
///
/// This is deliberately not [`omp_core::SemVer`]: that type stores `u8`
/// components, and a CLI's release numbering is not ours to bound.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CliVersion {
	/// Major version.
	pub major: u32,
	/// Minor version.
	pub minor: u32,
	/// Patch version.
	pub patch: u32,
}

impl CliVersion {
	/// Creates a version from its three components.
	#[must_use]
	pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
		Self { major, minor, patch }
	}
}

/// The CLI versions an executor has been written against.
///
/// Versions outside the range are refused rather than attempted: a protocol
/// that drifted is indistinguishable from a malformed stream at runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionRange {
	/// Lowest supported version, inclusive.
	pub min_inclusive: CliVersion,
	/// First unsupported version, or `None` for no upper bound.
	pub max_exclusive: Option<CliVersion>,
}

impl VersionRange {
	/// Whether `version` is supported.
	#[must_use]
	pub fn contains(&self, version: CliVersion) -> bool {
		version >= self.min_inclusive && self.max_exclusive.is_none_or(|ceiling| version < ceiling)
	}
}

/// Everything an executor declares about itself before any process exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capabilities {
	/// Identity of the executor.
	pub executor: ExecutorId,
	/// The execution mode this executor offers.
	pub mode: ExecutionMode,
	/// CLI versions this executor supports.
	pub versions: VersionRange,
	/// Optional behaviors this executor provides.
	pub features: FeatureSet,
	/// The exact policies this executor can enforce.
	///
	/// A requested policy is accepted only if it is listed here verbatim.
	/// There is no "closest stricter" fallback: a CLI whose sandbox and
	/// approval flags cannot express the request is refused.
	pub policies: Box<[DelegatedPolicy]>,
}

impl Capabilities {
	/// Declares a CLI-managed executor.
	#[must_use]
	pub fn new(
		executor: ExecutorId,
		versions: VersionRange,
		features: FeatureSet,
		policies: impl Into<Box<[DelegatedPolicy]>>,
	) -> Self {
		Self {
			executor,
			mode: ExecutionMode::CliManaged,
			versions,
			features,
			policies: policies.into(),
		}
	}

	/// Whether `policy` is one of the policies this executor can enforce.
	#[must_use]
	pub fn supports_policy(&self, policy: DelegatedPolicy) -> bool {
		self.policies.contains(&policy)
	}
}
