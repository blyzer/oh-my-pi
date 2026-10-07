//! Operator trust in a workspace's own project-sourced inputs.
//!
//! A workspace is trusted per canonical workspace root (the trust key) and
//! bound to an [`InputsDigest`] over the project inputs that load with the
//! operator's authority. Rows live in the local grant file
//! ([`crate::trust::GrantsFile::workspace_trust`]), never in the repository.
//!
//! Each row's [`TrustBinding`] is one of:
//!
//! - [`TrustBinding::Exact`]: this workspace, while its inputs hash to the
//!   recorded digest.
//! - [`TrustBinding::Subtree`]: an explicit operator grant over every workspace
//!   below a root. It never trusts a workspace by itself: a workspace under it
//!   evaluates to [`TrustDecision::PinUnderSubtree`], an ask the operator
//!   confirms once per workspace. A subtree rooted at `/`, at the canonical
//!   home directory, or at an ancestor of it is refused.
//! - [`TrustBinding::Pin`]: the operator's confirmation of one workspace under
//!   the subtree rooted exactly at `under`, bound to a digest. It is live only
//!   while that subtree row is.
//! - [`TrustBinding::Deny`]: a revocation that outranks every covering subtree.
//!
//! [`evaluate`] is the single pure decision over a set of rows. An operator's
//! answer is recorded only while [`evaluate`] still returns the decision the
//! operator was asked about
//! ([`crate::trust::GrantsFile::persist_workspace_trust`]), so a stale answer
//! never undoes a concurrent revoke.

use std::{
	fmt::{self, Display},
	fs, io,
	path::{Component, Path, PathBuf},
	str::FromStr,
};

use jiff::Timestamp;
use omp_core::{Hash32, Hash32ParseError, Str, sf};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use strum::{Display, EnumDiscriminants, EnumString, IntoStaticStr};
use thiserror::Error;

use crate::trust::GrantDuration;

/// Whether a workspace's project-sourced inputs load with the operator's
/// authority.
///
/// The host supplies this from [`evaluate`]; it is never a convar, so a
/// project cannot raise its own trust.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Display,
	EnumString,
	Eq,
	Hash,
	IntoStaticStr,
	PartialEq,
	Serialize,
	Deserialize,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum WorkspaceTrust {
	/// Project-sourced inputs are withheld (fail closed).
	#[default]
	Untrusted,
	/// Project-sourced inputs load with the operator's authority.
	Trusted,
}

/// The operator channel that recorded a workspace trust row.
#[derive(
	Clone,
	Copy,
	Debug,
	Display,
	EnumString,
	Eq,
	Hash,
	IntoStaticStr,
	PartialEq,
	Serialize,
	Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TrustChannel {
	/// An interactive prompt the operator answered.
	Interactive,
	/// An explicit `omp` CLI command.
	Cli,
	/// A launch flag.
	Flag,
	/// An RPC or ACP client request.
	Rpc,
}

/// SHA-256 digest of a workspace's gated project inputs, rendered
/// `sha256:<64 lowercase hex>`.
///
/// A dedicated type, so an inputs digest is never passed where another
/// digest (an artifact, a plugin command) is expected.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InputsDigest(Hash32);

/// The textual prefix naming the digest algorithm.
const INPUTS_DIGEST_PREFIX: &str = "sha256:";

impl InputsDigest {
	/// Wraps a SHA-256 digest of the gated inputs.
	#[inline]
	pub const fn new(hash: Hash32) -> Self {
		Self(hash)
	}

	/// The underlying SHA-256 digest.
	#[inline]
	pub const fn hash(&self) -> &Hash32 {
		&self.0
	}
}

impl From<Hash32> for InputsDigest {
	#[inline]
	fn from(hash: Hash32) -> Self {
		Self::new(hash)
	}
}

impl Display for InputsDigest {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(INPUTS_DIGEST_PREFIX)?;
		formatter.write_str(self.0.to_hex().as_str())
	}
}

impl fmt::Debug for InputsDigest {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		Display::fmt(self, formatter)
	}
}

/// Failure to parse an [`InputsDigest`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InputsDigestParseError {
	/// The text did not start with `sha256:` (a `b3:` digest is refused).
	#[error("inputs digest must start with `sha256:`")]
	MissingPrefix,
	/// The hexadecimal payload was malformed.
	#[error("inputs digest hash is malformed")]
	Hash(#[from] Hash32ParseError),
}

impl FromStr for InputsDigest {
	type Err = InputsDigestParseError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let hex = value
			.strip_prefix(INPUTS_DIGEST_PREFIX)
			.ok_or(InputsDigestParseError::MissingPrefix)?;
		Ok(Self(hex.parse()?))
	}
}

impl Serialize for InputsDigest {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for InputsDigest {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = Str::deserialize(deserializer)?;
		value.as_str().parse().map_err(de::Error::custom)
	}
}

/// What a [`WorkspaceTrustGrant`] binds its workspace to.
///
/// Serialized inline in the row, tagged by `scope`, so invalid combinations
/// (a subtree with a digest, an exact row without one) cannot be written.
#[derive(Clone, Debug, EnumDiscriminants, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
#[strum_discriminants(
	name(TrustScope),
	derive(Display, IntoStaticStr),
	strum(serialize_all = "snake_case"),
	doc = "The `scope` tag of a [`TrustBinding`], as the grant file spells it."
)]
pub enum TrustBinding {
	/// The workspace itself, while its inputs hash to `inputs_digest`.
	Exact {
		/// Digest of the gated inputs the operator approved.
		inputs_digest: InputsDigest,
	},
	/// An explicit grant over the workspace and every workspace below it.
	/// It asks once per workspace ([`TrustDecision::PinUnderSubtree`]) and
	/// never trusts one by itself.
	Subtree,
	/// The operator's confirmation of one workspace under the subtree rooted
	/// exactly at `under`, while its inputs hash to `inputs_digest`.
	Pin {
		/// Root of the [`TrustBinding::Subtree`] row that admitted this pin.
		under:         PathBuf,
		/// Digest of the gated inputs the operator approved.
		inputs_digest: InputsDigest,
	},
	/// The operator's revocation: the workspace stays untrusted even under a
	/// trusted subtree.
	Deny,
}

/// Which row a [`WorkspaceTrustGrant`] replaces when it is persisted: one
/// subtree root and one decision ([`TrustBinding::Exact`],
/// [`TrustBinding::Pin`] or [`TrustBinding::Deny`]) per workspace path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustSlot {
	/// The workspace's own decision.
	Decision,
	/// The subtree rooted at the workspace.
	Subtree,
}

impl TrustBinding {
	/// The slot this binding occupies at its workspace path.
	#[inline]
	pub const fn slot(&self) -> TrustSlot {
		match self {
			Self::Subtree => TrustSlot::Subtree,
			Self::Exact { .. } | Self::Pin { .. } | Self::Deny => TrustSlot::Decision,
		}
	}

	/// The binding's `scope` tag.
	#[inline]
	pub fn scope(&self) -> TrustScope {
		self.into()
	}
}

/// One `[[workspace_trust]]` row of the local grant file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceTrustGrant {
	/// Canonical absolute workspace root: the trust key, or a subtree root.
	pub workspace:  PathBuf,
	/// What the workspace is bound to.
	#[serde(flatten)]
	pub binding:    TrustBinding,
	/// RFC 3339 timestamp.
	pub granted_at: Str,
	/// Operator channel that recorded the row.
	pub granted_by: TrustChannel,
	/// Lifetime selected by the operator; only persistent rows are stored.
	#[serde(default)]
	pub duration:   GrantDuration,
}

impl WorkspaceTrustGrant {
	/// Trust in the workspace at `workspace` (canonicalized) while its inputs
	/// hash to `inputs_digest`.
	///
	/// # Errors
	///
	/// [`WorkspaceTrustError::Canonicalize`] when `workspace` cannot be
	/// canonicalized.
	pub fn exact(
		workspace: &Path,
		inputs_digest: InputsDigest,
		granted_by: TrustChannel,
		duration: GrantDuration,
	) -> Result<Self, WorkspaceTrustError> {
		Ok(Self::stamped(
			canonical(workspace)?,
			TrustBinding::Exact { inputs_digest },
			granted_by,
			duration,
		))
	}

	/// An explicit grant over the subtree rooted at `root` (canonicalized).
	///
	/// # Errors
	///
	/// [`WorkspaceTrustError::Canonicalize`] when `root` cannot be
	/// canonicalized; [`WorkspaceTrustError::SubtreeTooBroad`] when the
	/// canonical root is `/`, the canonical home directory, an ancestor of
	/// it, or `home` is [`CanonicalHome::Unresolved`].
	pub fn subtree(
		root: &Path,
		home: &CanonicalHome,
		granted_by: TrustChannel,
		duration: GrantDuration,
	) -> Result<Self, WorkspaceTrustError> {
		let root = canonical(root)?;
		if home.subtree_too_broad(&root) {
			return Err(WorkspaceTrustError::SubtreeTooBroad { root });
		}
		Ok(Self::stamped(root, TrustBinding::Subtree, granted_by, duration))
	}

	/// The operator's confirmation of the workspace at `workspace` under the
	/// subtree rooted at `under` (both canonicalized), the answer to
	/// [`TrustDecision::PinUnderSubtree`].
	///
	/// # Errors
	///
	/// [`WorkspaceTrustError::Canonicalize`] when either path cannot be
	/// canonicalized; [`WorkspaceTrustError::NotUnderSubtree`] when the
	/// workspace is not below `under`.
	pub fn pin(
		workspace: &Path,
		inputs_digest: InputsDigest,
		under: &Path,
		granted_by: TrustChannel,
		duration: GrantDuration,
	) -> Result<Self, WorkspaceTrustError> {
		let workspace = canonical(workspace)?;
		let under = canonical(under)?;
		if !workspace.starts_with(&under) {
			return Err(WorkspaceTrustError::NotUnderSubtree { workspace, under });
		}
		Ok(Self::stamped(workspace, TrustBinding::Pin { under, inputs_digest }, granted_by, duration))
	}

	/// The operator's revocation of the workspace at `workspace`
	/// (canonicalized), which outranks any covering subtree.
	///
	/// # Errors
	///
	/// [`WorkspaceTrustError::Canonicalize`] when `workspace` cannot be
	/// canonicalized.
	pub fn deny(
		workspace: &Path,
		granted_by: TrustChannel,
		duration: GrantDuration,
	) -> Result<Self, WorkspaceTrustError> {
		Ok(Self::stamped(canonical(workspace)?, TrustBinding::Deny, granted_by, duration))
	}

	/// A row recorded now.
	pub(crate) fn stamped(
		workspace: PathBuf,
		binding: TrustBinding,
		granted_by: TrustChannel,
		duration: GrantDuration,
	) -> Self {
		Self { workspace, binding, granted_at: sf!("{}", Timestamp::now()), granted_by, duration }
	}

	/// Whether this row and `other` occupy the same slot, so persisting one
	/// replaces the other.
	#[inline]
	pub fn same_slot(&self, other: &Self) -> bool {
		self.workspace == other.workspace && self.binding.slot() == other.binding.slot()
	}

	/// Checks this [`TrustBinding::Exact`] or [`TrustBinding::Pin`] row as the
	/// operator's answer to `answered`, against the durable `rows` read under
	/// the grant file lock: a compare-and-set on the decision.
	///
	/// A pin also needs the live subtree row at exactly `under` covering its
	/// workspace, and never replaces the workspace's [`TrustBinding::Deny`].
	pub(crate) fn check_answer(
		&self,
		rows: &[Self],
		answered: &TrustDecision,
		home: &CanonicalHome,
	) -> Result<(), WorkspaceTrustRefusal> {
		let inputs_digest = match &self.binding {
			TrustBinding::Exact { inputs_digest } => inputs_digest,
			TrustBinding::Pin { under, inputs_digest } => {
				if rows
					.iter()
					.any(|row| row.workspace == self.workspace && row.binding == TrustBinding::Deny)
				{
					return Err(WorkspaceTrustRefusal::Denied { workspace: self.workspace.clone() });
				}
				if !self.workspace.starts_with(under) || !live_subtree_at(rows, under, home) {
					return Err(WorkspaceTrustRefusal::NoLiveSubtree {
						workspace: self.workspace.clone(),
						under:     under.clone(),
					});
				}
				inputs_digest
			},
			TrustBinding::Subtree | TrustBinding::Deny => {
				return Err(WorkspaceTrustRefusal::Scope { scope: self.binding.scope() });
			},
		};
		let current = evaluate(rows, &self.workspace, inputs_digest, home);
		if current == *answered {
			Ok(())
		} else {
			Err(WorkspaceTrustRefusal::Stale { workspace: self.workspace.clone(), current })
		}
	}
}

/// Whether `rows` hold a [`TrustBinding::Subtree`] row rooted at exactly
/// `root` that `home` admits.
fn live_subtree_at<'g>(
	rows: impl IntoIterator<Item = &'g WorkspaceTrustGrant>,
	root: &Path,
	home: &CanonicalHome,
) -> bool {
	!home.subtree_too_broad(root)
		&& rows
			.into_iter()
			.any(|row| row.binding == TrustBinding::Subtree && row.workspace == root)
}

fn canonical(path: &Path) -> Result<PathBuf, WorkspaceTrustError> {
	fs::canonicalize(path)
		.map_err(|source| WorkspaceTrustError::Canonicalize { path: path.to_path_buf(), source })
}

/// The operator's home directory as subtree refusal compares it.
///
/// Rows hold canonical paths, so home is canonicalized too: with a symlinked
/// home (`/home` -> `/var/home`) the raw `$HOME` would never match a
/// canonical subtree root at it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalHome {
	/// The canonical home directory.
	Resolved(PathBuf),
	/// Home is unknown or cannot be canonicalized: every subtree is too broad.
	Unresolved,
}

impl CanonicalHome {
	/// Canonicalizes `home`; [`Self::Unresolved`] when it is absent or cannot
	/// be canonicalized.
	pub fn resolve(home: Option<&Path>) -> Self {
		home
			.and_then(|home| fs::canonicalize(home).ok())
			.map_or(Self::Unresolved, Self::Resolved)
	}

	/// The process owner's home directory ([`omp_core::dirs::home_dir`]),
	/// canonicalized.
	pub fn current() -> Self {
		Self::resolve(omp_core::dirs::home_dir().as_deref())
	}

	/// Whether a subtree rooted at `root` is refused: `root` is not a
	/// canonical absolute path, is `/` (has no parent), is home or an
	/// ancestor of it, or home is [`Self::Unresolved`].
	pub fn subtree_too_broad(&self, root: &Path) -> bool {
		match self {
			Self::Unresolved => true,
			Self::Resolved(home) => {
				!canonical_shape(root) || root.parent().is_none() || home.starts_with(root)
			},
		}
	}
}

/// Whether `path` is absolute and holds only normal components (no `.` or
/// `..`), as a canonical path does.
fn canonical_shape(path: &Path) -> bool {
	path.is_absolute()
		&& path.components().all(|component| {
			matches!(component, Component::Prefix(_) | Component::RootDir | Component::Normal(_))
		})
}

/// The outcome of [`evaluate`] for one workspace.
///
/// Serializable, so a host records it once per session in the journal.
#[derive(Clone, Debug, EnumDiscriminants, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
#[strum_discriminants(
	name(TrustDecisionKind),
	derive(Display, EnumString, IntoStaticStr, Hash, Serialize, Deserialize),
	strum(serialize_all = "snake_case"),
	serde(rename_all = "snake_case"),
	doc = "The closed vocabulary of [`TrustDecision`] outcomes."
)]
pub enum TrustDecision {
	/// An exact or pinned row matches the current inputs digest.
	Trusted {
		/// Lifetime of the longest-lived matching row.
		duration: GrantDuration,
	},
	/// No row decides the workspace itself, but an explicit subtree covers
	/// it. This is an ask: an interactive host confirms once and records a
	/// [`TrustBinding::Pin`]; a non-interactive host treats it as
	/// [`WorkspaceTrust::Untrusted`].
	PinUnderSubtree {
		/// Root of the most specific covering subtree.
		root:     PathBuf,
		/// Lifetime of that subtree row.
		duration: GrantDuration,
	},
	/// The workspace's own row was granted for other inputs; the operator
	/// must approve the current ones. A covering subtree never rescues it.
	DigestChanged {
		/// Digest the operator approved.
		granted: InputsDigest,
		/// Digest of the current inputs.
		current: InputsDigest,
	},
	/// A [`TrustBinding::Deny`] row revokes the workspace, whatever covers
	/// it. No subtree asks about it again; only an explicit
	/// [`TrustBinding::Exact`] grant answering this decision trusts it.
	Denied,
	/// No live row trusts the workspace or asks about it.
	Untrusted,
}

impl TrustDecision {
	/// The trust a host applies: only [`Self::Trusted`] trusts.
	#[inline]
	pub const fn trust(&self) -> WorkspaceTrust {
		match self {
			Self::Trusted { .. } => WorkspaceTrust::Trusted,
			Self::PinUnderSubtree { .. }
			| Self::DigestChanged { .. }
			| Self::Denied
			| Self::Untrusted => WorkspaceTrust::Untrusted,
		}
	}

	/// The decision's kind, for notices and journal vocabulary.
	#[inline]
	pub fn kind(&self) -> TrustDecisionKind {
		self.into()
	}
}

/// Decides the trust of the workspace keyed by `trust_key` (its canonical
/// root) with gated inputs hashing to `current`.
///
/// Rules, most specific first:
///
/// 1. A [`TrustBinding::Deny`] row at `trust_key` makes it
///    [`TrustDecision::Denied`], whatever else covers it.
/// 2. An [`TrustBinding::Exact`] row at `trust_key`, or a [`TrustBinding::Pin`]
///    row whose subtree row at exactly `under` is live, decides:
///    [`TrustDecision::Trusted`] when one carries `current`, else
///    [`TrustDecision::DigestChanged`].
/// 3. Otherwise the most specific live [`TrustBinding::Subtree`] covering
///    `trust_key` asks: [`TrustDecision::PinUnderSubtree`].
/// 4. Otherwise [`TrustDecision::Untrusted`].
///
/// A subtree row is live only while `home` admits its root
/// ([`CanonicalHome::subtree_too_broad`]), so a hand-edited row at `/` or
/// home trusts nothing. Containment is per path component: a subtree at
/// `/w` covers `/w/x`, never `/w2`. `rows` may chain durable and session
/// rows; it is walked more than once, hence the `Clone` bound.
pub fn evaluate<'g, I>(
	rows: I,
	trust_key: &Path,
	current: &InputsDigest,
	home: &CanonicalHome,
) -> TrustDecision
where
	I: IntoIterator<Item = &'g WorkspaceTrustGrant>,
	I::IntoIter: Clone,
{
	let rows = rows.into_iter();
	let mut trusted: Option<GrantDuration> = None;
	let mut granted: Option<InputsDigest> = None;
	for row in rows.clone().filter(|row| row.workspace == trust_key) {
		let digest = match &row.binding {
			TrustBinding::Deny => return TrustDecision::Denied,
			TrustBinding::Exact { inputs_digest } => inputs_digest,
			TrustBinding::Pin { under, inputs_digest }
				if trust_key.starts_with(under) && live_subtree_at(rows.clone(), under, home) =>
			{
				inputs_digest
			},
			TrustBinding::Pin { .. } | TrustBinding::Subtree => continue,
		};
		if digest == current {
			trusted = trusted.max(Some(row.duration));
		} else {
			granted.get_or_insert(*digest);
		}
	}
	if let Some(duration) = trusted {
		return TrustDecision::Trusted { duration };
	}
	if let Some(granted) = granted {
		return TrustDecision::DigestChanged { granted, current: *current };
	}
	rows
		.filter(|row| {
			row.binding == TrustBinding::Subtree
				&& trust_key.starts_with(&row.workspace)
				&& !home.subtree_too_broad(&row.workspace)
		})
		.max_by_key(|row| row.workspace.components().count())
		.map_or(TrustDecision::Untrusted, |row| TrustDecision::PinUnderSubtree {
			root:     row.workspace.clone(),
			duration: row.duration,
		})
}

/// Failure to build a [`WorkspaceTrustGrant`].
#[derive(Debug, Error)]
pub enum WorkspaceTrustError {
	/// A workspace or subtree path could not be canonicalized.
	#[error("workspace path {} cannot be canonicalized", path.display())]
	Canonicalize {
		/// The path as given.
		path:   PathBuf,
		/// Canonicalization failure.
		#[source]
		source: io::Error,
	},
	/// A subtree grant would cover `/`, the home directory, or an ancestor of
	/// it, or home could not be resolved.
	#[error(
		"subtree trust at {} is too broad: it covers `/`, the home directory, or an ancestor of it",
		root.display()
	)]
	SubtreeTooBroad {
		/// The canonical subtree root.
		root: PathBuf,
	},
	/// A pin names a subtree that does not contain the workspace.
	#[error("workspace {} is not under the trusted subtree {}", workspace.display(), under.display())]
	NotUnderSubtree {
		/// The canonical workspace.
		workspace: PathBuf,
		/// The canonical subtree root.
		under:     PathBuf,
	},
}

/// Why a workspace trust writer recorded nothing; the grant file is
/// unchanged.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorkspaceTrustRefusal {
	/// The writer does not record rows of this scope.
	#[error("a `{scope}` workspace trust row cannot be recorded by this writer")]
	Scope {
		/// The refused row's scope.
		scope: TrustScope,
	},
	/// The rows changed after the operator was asked, so the answer is stale:
	/// ask again about `current`.
	#[error(
		"workspace trust for {} changed after the operator was asked: it is now `{}`",
		workspace.display(),
		current.kind()
	)]
	Stale {
		/// The canonical workspace.
		workspace: PathBuf,
		/// The decision under the grant file lock.
		current:   TrustDecision,
	},
	/// A pin names no live subtree row rooted at exactly `under` that covers
	/// its workspace.
	#[error(
		"no live subtree trust rooted at {} covers workspace {}",
		under.display(),
		workspace.display()
	)]
	NoLiveSubtree {
		/// The canonical workspace.
		workspace: PathBuf,
		/// The subtree root the pin names.
		under:     PathBuf,
	},
	/// A pin never replaces the workspace's [`TrustBinding::Deny`] row.
	#[error("workspace {} is revoked: only an exact grant trusts it again", workspace.display())]
	Denied {
		/// The canonical workspace.
		workspace: PathBuf,
	},
}

#[cfg(test)]
mod tests {
	use super::*;

	fn digest(seed: &str) -> InputsDigest {
		InputsDigest::new(Hash32::sum(seed))
	}

	fn row(workspace: &Path, binding: TrustBinding) -> WorkspaceTrustGrant {
		WorkspaceTrustGrant {
			workspace: workspace.to_path_buf(),
			binding,
			granted_at: sf!("2026-10-07T00:00:00Z"),
			granted_by: TrustChannel::Cli,
			duration: GrantDuration::Persistent,
		}
	}

	fn exact(workspace: &Path, seed: &str) -> WorkspaceTrustGrant {
		row(workspace, TrustBinding::Exact { inputs_digest: digest(seed) })
	}

	fn pin(workspace: &Path, under: &Path, seed: &str) -> WorkspaceTrustGrant {
		row(workspace, TrustBinding::Pin {
			under:         under.to_path_buf(),
			inputs_digest: digest(seed),
		})
	}

	/// A canonical temporary tree: `<tmp>/var/home/u` is home, reached
	/// through the symlink `<tmp>/home -> <tmp>/var/home`.
	struct Tree {
		_temp: tempfile::TempDir,
		root:  PathBuf,
		home:  PathBuf,
		repo:  PathBuf,
	}

	fn tree() -> Tree {
		let temp = tempfile::tempdir().expect("temporary tree");
		let root = fs::canonicalize(temp.path()).expect("canonical temporary tree");
		let home = root.join("var/home/u");
		let repo = home.join("src/team/repo");
		fs::create_dir_all(&repo).expect("repository");
		#[cfg(unix)]
		std::os::unix::fs::symlink(root.join("var/home"), root.join("home")).expect("home symlink");
		Tree { _temp: temp, root, home, repo }
	}

	fn resolved(tree: &Tree) -> CanonicalHome {
		CanonicalHome::resolve(Some(&tree.home))
	}

	#[test]
	fn workspace_trust_defaults_to_untrusted_and_names_round_trip() {
		assert_eq!(WorkspaceTrust::default(), WorkspaceTrust::Untrusted);
		assert_eq!(<&str>::from(WorkspaceTrust::Trusted), "trusted");
		assert_eq!("untrusted".parse::<WorkspaceTrust>(), Ok(WorkspaceTrust::Untrusted));
		assert_eq!(<&str>::from(TrustChannel::Interactive), "interactive");
		assert_eq!("rpc".parse::<TrustChannel>(), Ok(TrustChannel::Rpc));
	}

	#[test]
	fn inputs_digest_renders_and_parses_sha256_only() {
		let value = digest("inputs");
		let text = value.to_string();
		assert_eq!(text, format!("sha256:{}", Hash32::sum("inputs")));
		assert_eq!(text.parse::<InputsDigest>(), Ok(value));
		assert_eq!(
			format!("b3:{}", Hash32::sum("inputs")).parse::<InputsDigest>(),
			Err(InputsDigestParseError::MissingPrefix)
		);
		assert_eq!(
			"sha256:ABC".parse::<InputsDigest>(),
			Err(InputsDigestParseError::Hash(Hash32ParseError::InvalidLength))
		);
	}

	#[test]
	fn decisions_serialize_tagged_with_their_kind() {
		let decision = TrustDecision::DigestChanged { granted: digest("a"), current: digest("b") };
		let json = serde_json::to_value(&decision).expect("serialize decision");
		assert_eq!(json["decision"], "digest_changed");
		assert_eq!(json["granted"], digest("a").to_string());
		assert_eq!(
			serde_json::from_value::<TrustDecision>(json).expect("deserialize decision"),
			decision
		);
		assert_eq!(decision.kind(), TrustDecisionKind::DigestChanged);
		assert_eq!(<&str>::from(TrustDecisionKind::PinUnderSubtree), "pin_under_subtree");
		assert_eq!(TrustDecision::Denied.kind().to_string(), "denied");
		assert_eq!("untrusted".parse::<TrustDecisionKind>(), Ok(TrustDecisionKind::Untrusted));
		assert_eq!(
			serde_json::to_value(TrustDecisionKind::Trusted).expect("serialize kind"),
			"trusted"
		);
	}

	#[test]
	fn exact_row_trusts_only_its_digest() {
		let tree = tree();
		let home = resolved(&tree);
		let rows = [exact(&tree.repo, "a")];
		let trusted = evaluate(&rows, &tree.repo, &digest("a"), &home);
		assert_eq!(trusted, TrustDecision::Trusted { duration: GrantDuration::Persistent });
		assert_eq!(trusted.trust(), WorkspaceTrust::Trusted);
		let changed = evaluate(&rows, &tree.repo, &digest("b"), &home);
		assert_eq!(changed, TrustDecision::DigestChanged {
			granted: digest("a"),
			current: digest("b"),
		});
		assert_eq!(changed.trust(), WorkspaceTrust::Untrusted);
		// An exact row covers nothing below it.
		assert_eq!(
			evaluate(&rows, &tree.repo.join("nested"), &digest("a"), &home),
			TrustDecision::Untrusted
		);
		assert_eq!(evaluate([], &tree.repo, &digest("a"), &home), TrustDecision::Untrusted);
	}

	#[test]
	fn subtree_asks_and_never_trusts_on_first_use() {
		let tree = tree();
		let home = resolved(&tree);
		let team = tree.home.join("src/team");
		let src = tree.home.join("src");
		let rows = [row(&src, TrustBinding::Subtree), row(&team, TrustBinding::Subtree)];
		let decision = evaluate(&rows, &tree.repo, &digest("a"), &home);
		assert_eq!(decision, TrustDecision::PinUnderSubtree {
			root:     team.clone(),
			duration: GrantDuration::Persistent,
		});
		assert_eq!(decision.trust(), WorkspaceTrust::Untrusted, "an ask is untrusted until answered");
		// A broader subtree never rescues a stale exact row.
		let rows = [row(&team, TrustBinding::Subtree), exact(&tree.repo, "old")];
		assert_eq!(evaluate(&rows, &tree.repo, &digest("a"), &home), TrustDecision::DigestChanged {
			granted: digest("old"),
			current: digest("a"),
		});
	}

	#[test]
	fn pin_trusts_only_under_its_exact_live_subtree() {
		let tree = tree();
		let home = resolved(&tree);
		let team = tree.home.join("src/team");
		let src = tree.home.join("src");
		let rows = [row(&team, TrustBinding::Subtree), pin(&tree.repo, &team, "a")];
		assert_eq!(evaluate(&rows, &tree.repo, &digest("a"), &home), TrustDecision::Trusted {
			duration: GrantDuration::Persistent,
		});
		assert_eq!(evaluate(&rows, &tree.repo, &digest("b"), &home), TrustDecision::DigestChanged {
			granted: digest("a"),
			current: digest("b"),
		});
		// The pin names `team`, but only `src` is trusted: the pin is dead
		// and `src` asks again.
		let rows = [row(&src, TrustBinding::Subtree), pin(&tree.repo, &team, "a")];
		assert_eq!(
			evaluate(&rows, &tree.repo, &digest("a"), &home),
			TrustDecision::PinUnderSubtree { root: src, duration: GrantDuration::Persistent }
		);
		// Without any subtree the pin is dead.
		let rows = [pin(&tree.repo, &team, "a")];
		assert_eq!(evaluate(&rows, &tree.repo, &digest("a"), &home), TrustDecision::Untrusted);
		// A pin whose `under` does not contain the workspace is dead too.
		let other = tree.home.join("other");
		let rows = [row(&other, TrustBinding::Subtree), pin(&tree.repo, &other, "a")];
		assert_eq!(evaluate(&rows, &tree.repo, &digest("a"), &home), TrustDecision::Untrusted);
	}

	#[test]
	fn deny_outranks_a_covering_subtree_and_an_exact_row() {
		let tree = tree();
		let home = resolved(&tree);
		let team = tree.home.join("src/team");
		let rows = [
			row(&team, TrustBinding::Subtree),
			pin(&tree.repo, &team, "a"),
			exact(&tree.repo, "a"),
			row(&tree.repo, TrustBinding::Deny),
		];
		let denied = evaluate(&rows, &tree.repo, &digest("a"), &home);
		assert_eq!(denied, TrustDecision::Denied);
		assert_eq!(denied.trust(), WorkspaceTrust::Untrusted);
		// The deny is exact: a sibling under the subtree still asks.
		let sibling = team.join("sibling");
		assert!(matches!(
			evaluate(&rows, &sibling, &digest("a"), &home),
			TrustDecision::PinUnderSubtree { .. }
		));
	}

	#[test]
	fn subtree_at_root_home_or_an_ancestor_of_home_trusts_nothing() {
		let tree = tree();
		let home = resolved(&tree);
		for root in
			[PathBuf::from("/"), tree.home.clone(), tree.root.join("var/home"), tree.root.clone()]
		{
			let root = root.as_path();
			assert!(home.subtree_too_broad(root), "{}", root.display());
			let rows = [row(root, TrustBinding::Subtree), pin(&tree.repo, root, "a")];
			assert_eq!(
				evaluate(&rows, &tree.repo, &digest("a"), &home),
				TrustDecision::Untrusted,
				"{}",
				root.display()
			);
			assert!(matches!(
				WorkspaceTrustGrant::subtree(root, &home, TrustChannel::Cli, GrantDuration::Persistent),
				Err(WorkspaceTrustError::SubtreeTooBroad { .. })
			));
		}
		let src = tree.home.join("src");
		assert!(!home.subtree_too_broad(&src));
		let granted =
			WorkspaceTrustGrant::subtree(&src, &home, TrustChannel::Cli, GrantDuration::Persistent)
				.expect("a subtree below home");
		assert_eq!(granted.workspace, src);
		// Non-canonical shapes never pass.
		assert!(home.subtree_too_broad(Path::new("relative/src")));
		assert!(home.subtree_too_broad(&src.join("..")));
	}

	#[cfg(unix)]
	#[test]
	fn subtree_refusal_uses_the_canonical_home() {
		let tree = tree();
		// `$HOME` reached through `<tmp>/home -> <tmp>/var/home`.
		let linked = tree.root.join("home/u");
		let home = CanonicalHome::resolve(Some(&linked));
		assert_eq!(home, CanonicalHome::Resolved(tree.home.clone()));
		// The raw `$HOME` does not start with the canonical row, so only the
		// canonical comparison refuses it.
		assert!(!linked.starts_with(&tree.home));
		assert!(home.subtree_too_broad(&tree.home));
		assert!(home.subtree_too_broad(&tree.root.join("var")));
		let rows = [row(&tree.home, TrustBinding::Subtree)];
		assert_eq!(evaluate(&rows, &tree.repo, &digest("a"), &home), TrustDecision::Untrusted);
		// The constructor canonicalizes the root it is given too.
		assert!(matches!(
			WorkspaceTrustGrant::subtree(&linked, &home, TrustChannel::Cli, GrantDuration::Persistent),
			Err(WorkspaceTrustError::SubtreeTooBroad { root }) if root == tree.home
		));
	}

	#[test]
	fn unresolvable_home_makes_every_subtree_too_broad() {
		let tree = tree();
		let src = tree.home.join("src");
		for home in
			[CanonicalHome::resolve(Some(&tree.root.join("missing"))), CanonicalHome::resolve(None)]
		{
			assert_eq!(home, CanonicalHome::Unresolved);
			assert!(home.subtree_too_broad(&src));
			let rows = [row(&src, TrustBinding::Subtree), pin(&tree.repo, &src, "a")];
			assert_eq!(evaluate(&rows, &tree.repo, &digest("a"), &home), TrustDecision::Untrusted);
			// An exact row does not depend on home.
			let rows = [exact(&tree.repo, "a")];
			assert_eq!(evaluate(&rows, &tree.repo, &digest("a"), &home), TrustDecision::Trusted {
				duration: GrantDuration::Persistent,
			});
		}
	}

	#[test]
	fn constructors_canonicalize_and_check_containment() {
		let tree = tree();
		let dotted = tree.repo.join("../repo");
		let grant = WorkspaceTrustGrant::exact(
			&dotted,
			digest("a"),
			TrustChannel::Interactive,
			GrantDuration::Session,
		)
		.expect("exact grant");
		assert_eq!(grant.workspace, tree.repo);
		assert_eq!(grant.duration, GrantDuration::Session);
		let team = tree.home.join("src/team");
		let pinned = WorkspaceTrustGrant::pin(
			&tree.repo,
			digest("a"),
			&team,
			TrustChannel::Interactive,
			GrantDuration::Persistent,
		)
		.expect("pin under its subtree");
		assert_eq!(pinned.binding, TrustBinding::Pin {
			under:         team,
			inputs_digest: digest("a"),
		});
		assert!(matches!(
			WorkspaceTrustGrant::pin(
				&tree.home,
				digest("a"),
				&tree.repo,
				TrustChannel::Interactive,
				GrantDuration::Persistent,
			),
			Err(WorkspaceTrustError::NotUnderSubtree { .. })
		));
		assert!(matches!(
			WorkspaceTrustGrant::deny(
				&tree.root.join("missing"),
				TrustChannel::Cli,
				GrantDuration::Persistent
			),
			Err(WorkspaceTrustError::Canonicalize { .. })
		));
	}

	#[test]
	fn longest_lived_matching_row_sets_the_trusted_duration() {
		let tree = tree();
		let home = resolved(&tree);
		let session =
			WorkspaceTrustGrant { duration: GrantDuration::Session, ..exact(&tree.repo, "a") };
		let durable = [exact(&tree.repo, "a")];
		assert_eq!(
			evaluate(std::iter::once(&session).chain(&durable), &tree.repo, &digest("a"), &home),
			TrustDecision::Trusted { duration: GrantDuration::Persistent }
		);
		assert_eq!(evaluate([&session], &tree.repo, &digest("a"), &home), TrustDecision::Trusted {
			duration: GrantDuration::Session,
		});
	}

	#[test]
	fn subtree_and_pin_containment_is_per_path_component() {
		let tree = tree();
		let home = resolved(&tree);
		let team = tree.home.join("src/team");
		let team2 = tree.home.join("src/team2");
		let sibling = team2.join("repo");
		fs::create_dir_all(&sibling).expect("sibling repository");
		assert!(
			team2
				.as_os_str()
				.as_encoded_bytes()
				.starts_with(team.as_os_str().as_encoded_bytes()),
			"`team` is a string prefix of `team2`"
		);
		let subtree = row(&team, TrustBinding::Subtree);
		for workspace in [&team2, &sibling] {
			assert_eq!(
				evaluate([&subtree], workspace, &digest("a"), &home),
				TrustDecision::Untrusted,
				"{}",
				workspace.display()
			);
		}
		// A pin under `team` is dead for a workspace under `team2`, even beside
		// the live `team` subtree, and only a subtree at `team2` asks about it.
		let rows = [subtree.clone(), pin(&sibling, &team, "a")];
		assert_eq!(evaluate(&rows, &sibling, &digest("a"), &home), TrustDecision::Untrusted);
		let rows = [subtree, row(&team2, TrustBinding::Subtree), pin(&sibling, &team, "a")];
		assert_eq!(evaluate(&rows, &sibling, &digest("a"), &home), TrustDecision::PinUnderSubtree {
			root:     team2,
			duration: GrantDuration::Persistent,
		});
		assert!(matches!(
			WorkspaceTrustGrant::pin(
				&sibling,
				digest("a"),
				&team,
				TrustChannel::Interactive,
				GrantDuration::Persistent,
			),
			Err(WorkspaceTrustError::NotUnderSubtree { .. })
		));
		// The answer check refuses such a pin as well.
		let asked = TrustDecision::PinUnderSubtree {
			root:     team.clone(),
			duration: GrantDuration::Persistent,
		};
		assert_eq!(
			pin(&sibling, &team, "a").check_answer(&rows, &asked, &home),
			Err(WorkspaceTrustRefusal::NoLiveSubtree { workspace: sibling, under: team })
		);
	}

	#[test]
	fn an_answer_is_checked_against_the_decision_it_answered() {
		let tree = tree();
		let home = resolved(&tree);
		let team = tree.home.join("src/team");
		let subtree = row(&team, TrustBinding::Subtree);
		let deny = row(&tree.repo, TrustBinding::Deny);
		let asked = TrustDecision::PinUnderSubtree {
			root:     team.clone(),
			duration: GrantDuration::Persistent,
		};
		let answer = pin(&tree.repo, &team, "a");
		assert_eq!(answer.check_answer(std::slice::from_ref(&subtree), &asked, &home), Ok(()));
		// A revoke recorded a deny after the ask: the pin never replaces it.
		assert_eq!(
			answer.check_answer(&[subtree.clone(), deny.clone()], &asked, &home),
			Err(WorkspaceTrustRefusal::Denied { workspace: tree.repo.clone() })
		);
		// The subtree was revoked after the ask: the pin would lie dormant until
		// the subtree is granted again.
		assert_eq!(
			answer.check_answer(&[], &asked, &home),
			Err(WorkspaceTrustRefusal::NoLiveSubtree {
				workspace: tree.repo.clone(),
				under:     team.clone(),
			})
		);
		// A more specific subtree granted after the ask makes it stale.
		let nested = row(&team.join("repo"), TrustBinding::Subtree);
		let rows = [subtree.clone(), nested];
		assert_eq!(
			answer.check_answer(&rows, &asked, &home),
			Err(WorkspaceTrustRefusal::Stale {
				workspace: tree.repo.clone(),
				current:   TrustDecision::PinUnderSubtree {
					root:     tree.repo.clone(),
					duration: GrantDuration::Persistent,
				},
			})
		);

		let reapproved = exact(&tree.repo, "b");
		let asked = TrustDecision::DigestChanged { granted: digest("a"), current: digest("b") };
		assert_eq!(reapproved.check_answer(&[exact(&tree.repo, "a")], &asked, &home), Ok(()));
		// A revoke after the ask under a covering subtree (a deny), and without
		// one (the exact row is gone).
		let revoked = [subtree.clone(), deny.clone()];
		assert_eq!(
			reapproved.check_answer(&revoked, &asked, &home),
			Err(WorkspaceTrustRefusal::Stale {
				workspace: tree.repo,
				current:   TrustDecision::Denied,
			})
		);
		assert!(matches!(
			reapproved.check_answer(&[], &asked, &home),
			Err(WorkspaceTrustRefusal::Stale { current: TrustDecision::Untrusted, .. })
		));
		// An ask that saw no row is stale once a deny is recorded, but an
		// explicit grant answering the deny itself re-trusts the workspace.
		assert!(matches!(
			reapproved.check_answer(&revoked, &TrustDecision::Untrusted, &home),
			Err(WorkspaceTrustRefusal::Stale { current: TrustDecision::Denied, .. })
		));
		assert_eq!(reapproved.check_answer(&revoked, &TrustDecision::Denied, &home), Ok(()));

		// Subtree and deny rows are not answers.
		for refused in [subtree, deny] {
			assert_eq!(
				refused.check_answer(&[], &TrustDecision::Untrusted, &home),
				Err(WorkspaceTrustRefusal::Scope { scope: refused.binding.scope() })
			);
		}
	}

	#[test]
	fn scopes_name_the_serialized_scope_tag() {
		let under = PathBuf::from("/w");
		for (binding, scope) in [
			(TrustBinding::Exact { inputs_digest: digest("a") }, "exact"),
			(TrustBinding::Subtree, "subtree"),
			(TrustBinding::Pin { under, inputs_digest: digest("a") }, "pin"),
			(TrustBinding::Deny, "deny"),
		] {
			assert_eq!(<&str>::from(binding.scope()), scope);
			let json =
				serde_json::to_value(row(Path::new("/w/repo"), binding)).expect("serialize row");
			assert_eq!(json["scope"], scope);
		}
	}
}
