//! Registered import steps, their idempotency markers, and the runner.

use std::{
	fmt::Write as _,
	fs, io,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_ai::auth::AuthControlHandle;
use strum::{Display, EnumIter, EnumString, IntoEnumIterator as _, IntoStaticStr};
use thiserror::Error;

use super::{
	ImportPair, V1Item,
	report::{Attention, ImportEntry, ImportOutcome, ImportReport, PairReport, SkipReason},
};

/// Whether a run may write.
#[derive(Clone, Copy, Debug, Display, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "kebab-case")]
pub enum ImportMode {
	/// Read v1, report what would happen, write nothing anywhere.
	DryRun,
	/// Copy into v2 and set each finished step's marker.
	Apply,
}

/// One registered import step. Declaration order is run order.
///
/// The kebab-case name is the step's identity: reports print it, and its
/// marker is `<v2 profile config root>/.<name>-migration-v1`.
#[derive(
	Clone,
	Copy,
	Debug,
	Display,
	EnumIter,
	EnumString,
	Eq,
	Hash,
	IntoStaticStr,
	Ord,
	PartialEq,
	PartialOrd,
)]
#[strum(serialize_all = "kebab-case")]
pub enum ImportStep {
	/// v1 `models.yml` (or an earlier omp2 `models.toml`) into `models.toml`.
	Models,
	/// Literal v1 `models.yml` `apiKey`s into the encrypted credential store.
	ModelsKeys,
}

impl ImportStep {
	/// Every registered step, in run order.
	pub fn registered() -> impl Iterator<Item = Self> + Clone {
		Self::iter()
	}

	/// The v1 item this step reads.
	#[must_use]
	pub const fn item(self) -> V1Item {
		match self {
			Self::Models | Self::ModelsKeys => V1Item::Models,
		}
	}

	/// Whether applying this step writes credentials.
	#[must_use]
	pub const fn needs_credentials(self) -> bool {
		matches!(self, Self::ModelsKeys)
	}

	/// This step's marker in a v2 profile configuration root.
	#[must_use]
	pub fn marker(self, config_dir: &Path) -> Marker {
		let name: &'static str = self.into();
		let mut file = String::with_capacity(name.len() + ".-migration-v1".len());
		let _ = write!(file, ".{name}-migration-v1");
		Marker { path: config_dir.join(file) }
	}

	fn run(self, cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
		match self {
			Self::Models => super::models::import_models(cx),
			Self::ModelsKeys => super::models::import_keys(cx),
		}
	}
}

/// A per-step, per-profile idempotency marker: once set, the step never runs
/// again for that v2 profile, even if the v1 data changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Marker {
	path: PathBuf,
}

impl Marker {
	/// Current marker revision.
	pub const REVISION: u32 = 1;

	/// Where the marker lives.
	#[must_use]
	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Whether the step already ran.
	#[must_use]
	pub fn is_set(&self) -> bool {
		self.path.exists()
	}

	/// Records that the step ran, with an optional source label, creating the
	/// configuration root when needed. The write is atomic.
	///
	/// # Errors
	///
	/// Returns the filesystem failure.
	pub fn set(&self, source: Option<&str>) -> io::Result<()> {
		let mut contents = String::with_capacity(48);
		let _ = writeln!(contents, "revision = {}", Self::REVISION);
		if let Some(source) = source {
			let _ = writeln!(contents, "source = \"{source}\"");
		}
		if let Some(parent) = self.path.parent() {
			fs::create_dir_all(parent)?;
		}
		atomic_replace(&self.path, contents.as_bytes())
	}
}

/// Writes `contents` to a sibling temporary file and renames it over `path`.
pub(crate) fn atomic_replace(path: &Path, contents: &[u8]) -> io::Result<()> {
	let mut temporary = path.as_os_str().to_owned();
	temporary.push(format!(".tmp-{}", std::process::id()));
	let temporary = PathBuf::from(temporary);
	let result = (|| {
		let file = fs::File::create(&temporary)?;
		io::Write::write_all(&mut &file, contents)?;
		file.sync_all()?;
		fs::rename(&temporary, path)
	})();
	if result.is_err() {
		let _ = fs::remove_file(&temporary);
	}
	result
}

/// How credential-writing steps reach the encrypted store.
#[derive(Clone, Copy)]
pub enum CredentialAccess<'a> {
	/// The live authentication stack of this process, which owns the stores
	/// under `data_dir` (the first-run hook). Other profiles' credential
	/// steps wait, unmarked, for their own first run or `omp config
	/// import-v1`: their stores follow their own launch's key policy.
	Live {
		/// Data directory the live stack's stores live in.
		data_dir: &'a Path,
		/// Control handle over those stores.
		control:  &'a AuthControlHandle,
	},
	/// Open each target profile's stores on demand, under this console
	/// policy (`omp config import-v1`).
	Offline(&'a omp_con::Ctx),
}

/// Everything one step sees while it runs for one profile pair.
pub struct StepContext<'a> {
	/// The v1 profile read and the v2 profile written.
	pub pair:        &'a ImportPair,
	/// Whether the step may write.
	pub mode:        ImportMode,
	/// The credential store, present when `mode` is [`ImportMode::Apply`]
	/// and the step [needs it](ImportStep::needs_credentials).
	pub credentials: Option<&'a AuthControlHandle>,
}

impl StepContext<'_> {
	/// Where v1 keeps `item` for this pair.
	#[must_use]
	pub fn locate(&self, item: V1Item) -> Option<PathBuf> {
		self.pair.source.locate(item)
	}
}

/// Runs every registered step for each pair, in order.
///
/// Per step: a set marker skips it; otherwise the step runs and, in
/// [`ImportMode::Apply`], sets its own marker once finished. A failure is
/// reported as [`Attention::Failed`] and leaves the marker unset. Nothing
/// under the v1 roots is ever written, moved, or deleted.
pub fn run(
	pairs: &[ImportPair],
	mode: ImportMode,
	credentials: CredentialAccess<'_>,
) -> ImportReport {
	ImportReport {
		dry_run: mode == ImportMode::DryRun,
		pairs:   pairs
			.iter()
			.map(|pair| run_pair(pair, mode, credentials))
			.collect(),
	}
}

fn run_pair(pair: &ImportPair, mode: ImportMode, access: CredentialAccess<'_>) -> PairReport {
	let steps = ImportStep::registered();
	let inventory = pair
		.source
		.inventory()
		.map(|(item, path)| {
			let importers = steps.clone().filter(|step| step.item() == item).collect();
			(item, path, importers)
		})
		.collect();
	let mut offline: Option<Result<AuthControlHandle, Arc<ImportError>>> = None;
	let mut entries = Vec::new();
	for step in steps {
		let marker = step.marker(&pair.target.config_dir);
		if marker.is_set() {
			entries.push(ImportEntry::new(
				step,
				step.item(),
				pair.source.locate(step.item()),
				ImportOutcome::Skipped(SkipReason::MarkerPresent),
			));
			continue;
		}
		let credentials = if mode == ImportMode::Apply && step.needs_credentials() {
			match access {
				CredentialAccess::Live { data_dir, control } if pair.target.data_dir == data_dir => {
					Some(control.clone())
				},
				CredentialAccess::Live { .. } => {
					entries.push(ImportEntry::new(
						step,
						step.item(),
						pair.source.locate(step.item()),
						ImportOutcome::Skipped(SkipReason::WaitsForProfile),
					));
					continue;
				},
				CredentialAccess::Offline(ctx) => {
					match offline.get_or_insert_with(|| {
						super::credentials::offline_control(&pair.target, ctx).map_err(Arc::new)
					}) {
						Ok(control) => Some(control.clone()),
						Err(error) => {
							entries.push(ImportEntry::new(
								step,
								step.item(),
								pair.source.locate(step.item()),
								ImportOutcome::NeedsAttention(Attention::Failed(
									ImportError::CredentialsUnavailable {
										target: pair.target.data_dir.clone(),
										source: Arc::clone(error),
									},
								)),
							));
							continue;
						},
					}
				},
			}
		} else {
			None
		};
		let cx = StepContext { pair, mode, credentials: credentials.as_ref() };
		match step.run(&cx) {
			Ok(produced) => entries.extend(produced),
			Err(error) => entries.push(ImportEntry::new(
				step,
				step.item(),
				pair.source.locate(step.item()),
				ImportOutcome::NeedsAttention(Attention::Failed(error)),
			)),
		}
	}
	PairReport {
		source_profile: pair.source.profile().map(omp_core::Str::new),
		source_root: pair.source.config_root().to_owned(),
		agent_dir: pair.source.agent_dir().to_owned(),
		xdg_roots: [super::XdgCategory::Data, super::XdgCategory::State, super::XdgCategory::Cache]
			.into_iter()
			.filter_map(|category| {
				pair
					.source
					.xdg_root(category)
					.map(|root| (category, root.to_owned()))
			})
			.collect(),
		target_profile: pair.target.profile.clone(),
		target_config: pair.target.config_dir.clone(),
		collisions: pair.xdg_collisions(),
		inventory,
		entries,
	}
}

/// A step failure. Reported per item; never fatal to the run or to startup.
#[derive(Debug, Error)]
pub enum ImportError {
	/// The v1 model configuration could not be read or converted.
	#[error("could not import the model configuration")]
	Models(#[from] crate::discovery::models::ModelsConfigError),
	/// A credential step ran without a credential store.
	#[error("no credential store was provided to import into")]
	NoCredentialStore,
	/// The target profile's credential store could not be opened.
	#[error("could not open the credential store under {}", target.display())]
	CredentialsUnavailable {
		/// The target profile's data directory.
		target: PathBuf,
		/// Why opening failed, shared by every step of the pair.
		#[source]
		source: Arc<Self>,
	},
	/// The embedded catalog snapshot is invalid.
	#[error("the embedded catalog snapshot is invalid")]
	Catalog(#[source] &'static omp_catalog::snapshot::SnapshotError),
	/// The target profile's configured catalog did not materialize.
	#[error("the target profile's configured catalog is invalid")]
	ConfiguredCatalog(#[source] omp_catalog::snapshot::SnapshotError),
	/// Opening encrypted credential state failed.
	#[error("could not open encrypted credential state")]
	CredentialStore(#[source] crate::registry::RegistryError),
	/// Opening durable account state failed.
	#[error("could not open durable account state")]
	AccountState(#[from] omp_ai::account::AccountStateStoreError),
	/// The credential broker could not be composed over the catalog.
	#[error("could not compose the credential broker")]
	CredentialBroker(#[from] omp_ai::auth::CredentialBrokerError),
}
