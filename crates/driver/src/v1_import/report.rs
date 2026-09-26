//! Typed import report, rendered once at the app boundary.

use std::path::PathBuf;

use omp_core::Str;
use strum::{Display, IntoStaticStr};

use super::{ImportError, ImportStep, V1Item, XdgCategory, XdgCollision};

/// Coarse outcome class, the first column of a rendered report.
#[derive(Clone, Copy, Debug, Display, Eq, Hash, IntoStaticStr, Ord, PartialEq, PartialOrd)]
#[strum(serialize_all = "kebab-case")]
pub enum OutcomeKind {
	/// Copied into v2.
	Imported,
	/// A dry run found something a real run would copy.
	WouldImport,
	/// Already done (a marker, an existing v2 file, or an existing account),
	/// or waiting for its profile's own run.
	Skipped,
	/// v1 has nothing for this step.
	NothingToImport,
	/// v1 has it, and v2 has nowhere to put it.
	NotMigratable,
	/// The owner has to act.
	NeedsAttention,
}

/// Why a step skipped an item.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, PartialEq)]
pub enum SkipReason {
	/// The step's marker records an earlier run.
	#[strum(to_string = "already imported (marker present)")]
	MarkerPresent,
	/// v2 already has its own copy, which the import never replaces.
	#[strum(to_string = "v2 already has its own copy")]
	TargetExists,
	/// The provider already has a stored v2 account.
	#[strum(to_string = "kept the existing v2 login")]
	AccountExists,
	/// Credentials for a profile other than the running one wait for that
	/// profile's own first run (or `omp config import-v1`).
	#[strum(to_string = "waits for that profile's first run or `omp config import-v1`")]
	WaitsForProfile,
	/// The v1 value is already the v2 default, so no line is written.
	#[strum(to_string = "already the v2 default")]
	MatchesDefault,
	/// An earlier v1 credential of the same provider and identity was taken.
	#[strum(to_string = "an earlier v1 credential has the same identity")]
	DuplicateIdentity,
}

/// Why v1 data cannot move to v2.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, PartialEq)]
pub enum NotMigratable {
	/// v2 has no equivalent feature or backend.
	#[strum(to_string = "v2 has no equivalent")]
	NoV2Equivalent,
	/// v2 has the setting but rejects the v1 value.
	#[strum(to_string = "v2 rejects the v1 value")]
	ValueRejected,
}

/// What the owner has to do about an item.
#[derive(Debug, Display)]
pub enum Attention {
	/// A v1 `apiKey` named an environment variable or a `!command`; v2 reads
	/// `OMP_<PROVIDER>_API_KEY` or a `/login` instead.
	#[strum(
		to_string = "the v1 key names a variable or command; set OMP_<PROVIDER>_API_KEY or /login"
	)]
	KeyNeedsEnvironment,
	/// v2 has no such memory backend (v1 `hindsight`, `sharpshooter`); its
	/// settings are kept as comments.
	#[strum(to_string = "v2 has no such memory backend; set ai_memory_backend mnemopi or off")]
	MemoryBackendDropped,
	/// v2 cannot use a v1 login as stored (a missing refresh token or a
	/// login-time fact v2 cannot derive); a fresh v2 login replaces it.
	#[strum(to_string = "v2 cannot use this v1 login; re-run /login for this provider")]
	ReloginRequired,
	/// A v1 login bound to a custom endpoint v2 keeps per provider, not per
	/// account.
	#[strum(to_string = "the v1 login used a custom endpoint; set it as the provider's baseUrl in \
	                     models.toml, then /login")]
	CustomEndpoint,
	/// A v1 MCP OAuth grant v2 cannot refresh or place (no server URL, token
	/// endpoint, or client id, or disabled in v1).
	#[strum(to_string = "re-authorize this MCP server in v2")]
	McpReauthorize,
	/// The step failed; nothing it would have written is marked done, so the
	/// next run retries.
	#[strum(to_string = "import failed")]
	Failed(ImportError),
}

/// What happened to one item.
#[derive(Debug)]
pub enum ImportOutcome {
	/// Copied into v2.
	Imported,
	/// A dry run found something a real run would copy.
	WouldImport,
	/// Already done.
	Skipped(SkipReason),
	/// v1 has nothing for this step.
	NothingToImport,
	/// v1 has it, and v2 has nowhere to put it.
	NotMigratable(NotMigratable),
	/// The owner has to act.
	NeedsAttention(Attention),
}

impl ImportOutcome {
	/// The coarse class of this outcome.
	#[must_use]
	pub const fn kind(&self) -> OutcomeKind {
		match self {
			Self::Imported => OutcomeKind::Imported,
			Self::WouldImport => OutcomeKind::WouldImport,
			Self::Skipped(_) => OutcomeKind::Skipped,
			Self::NothingToImport => OutcomeKind::NothingToImport,
			Self::NotMigratable(_) => OutcomeKind::NotMigratable,
			Self::NeedsAttention(_) => OutcomeKind::NeedsAttention,
		}
	}
}

/// One reported item.
#[derive(Debug)]
pub struct ImportEntry {
	/// The step that produced this entry.
	pub step:    ImportStep,
	/// The v1 item it concerns.
	pub item:    V1Item,
	/// The file or tree read, when there was one.
	pub path:    Option<PathBuf>,
	/// A finer subject inside the item (a provider id for keys).
	pub subject: Option<Str>,
	/// What happened.
	pub outcome: ImportOutcome,
}

impl ImportEntry {
	/// An entry about the whole item.
	#[must_use]
	pub const fn new(
		step: ImportStep,
		item: V1Item,
		path: Option<PathBuf>,
		outcome: ImportOutcome,
	) -> Self {
		Self { step, item, path, subject: None, outcome }
	}
}

/// Everything one v1 → v2 profile pair produced.
#[derive(Debug)]
pub struct PairReport {
	/// v1 profile read (`None` is the default).
	pub source_profile: Option<Str>,
	/// v1 profile root.
	pub source_root:    PathBuf,
	/// v1 agent directory.
	pub agent_dir:      PathBuf,
	/// v1 XDG roots this profile relocated to.
	pub xdg_roots:      Vec<(XdgCategory, PathBuf)>,
	/// v2 profile written (`None` is the default).
	pub target_profile: Option<Str>,
	/// v2 profile configuration root.
	pub target_config:  PathBuf,
	/// v1 XDG roots shared with v2 (owner decision #9: reported, never acted
	/// on).
	pub collisions:     Vec<XdgCollision>,
	/// Every v1 item found, with the steps that import it (empty until a
	/// later step lands).
	pub inventory:      Vec<(V1Item, PathBuf, Vec<ImportStep>)>,
	/// Per-step results.
	pub entries:        Vec<ImportEntry>,
}

/// The whole import report.
#[derive(Debug, Default)]
pub struct ImportReport {
	/// Whether nothing was written.
	pub dry_run: bool,
	/// One report per profile pair.
	pub pairs:   Vec<PairReport>,
	/// The current project's items (`<project>/.omp`), imported once per
	/// project.
	pub project: Vec<ImportEntry>,
}

impl ImportReport {
	/// Every entry across all pairs.
	pub fn entries(&self) -> impl Iterator<Item = &ImportEntry> + '_ {
		self
			.pairs
			.iter()
			.flat_map(|pair| pair.entries.iter())
			.chain(&self.project)
	}

	/// Logs the report: imports and skips at info, anything the owner must
	/// act on and every XDG collision at warn.
	pub fn log(&self) {
		for pair in &self.pairs {
			for collision in &pair.collisions {
				tracing::warn!(
					category = %collision.category,
					v1 = %collision.v1.display(),
					v2 = %collision.v2.display(),
					"v1 and v2 share an XDG root; the v1 data there is not relocated"
				);
			}
			pair.entries.iter().for_each(log_entry);
		}
		self.project.iter().for_each(log_entry);
	}
}

fn log_entry(entry: &ImportEntry) {
	let kind = entry.outcome.kind();
	let path = entry.path.as_ref().map(|path| path.display());
	match &entry.outcome {
		ImportOutcome::NeedsAttention(Attention::Failed(error)) => tracing::warn!(
			step = %entry.step,
			item = %entry.item,
			path = ?path,
			subject = ?entry.subject,
			error = error as &dyn std::error::Error,
			"v1 import step failed; it will retry next run"
		),
		ImportOutcome::NeedsAttention(attention) => tracing::warn!(
			step = %entry.step,
			item = %entry.item,
			path = ?path,
			subject = ?entry.subject,
			%attention,
			"v1 import needs attention"
		),
		ImportOutcome::Imported => tracing::info!(
			step = %entry.step,
			item = %entry.item,
			path = ?path,
			subject = ?entry.subject,
			"imported from v1"
		),
		_ => tracing::debug!(
			step = %entry.step,
			item = %entry.item,
			%kind,
			"v1 import step had nothing new"
		),
	}
}
