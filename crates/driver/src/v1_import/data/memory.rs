//! The memory steps: `mnemopi` (v1 Mnemopi stores), `learned-lessons` (the v1
//! `local` backend's `learned.md` lessons), and `memory-backends` (the v1
//! backends v2 dropped, reported only).
//!
//! # Where v2 keeps Mnemopi
//!
//! v1 kept one install-wide Mnemopi directory: the shared bank
//! `memories/mnemopi/mnemopi.db` and one `banks/<bank>/mnemopi.db` per project
//! bank. v2 keeps one per project, in the project's environment state
//! (`<data>/projects/<id>/memory/mnemopi`), unless `ai_mnemopi_db_path` names
//! an install-wide primary database. A v2 runtime recalls a sibling
//! `banks/<bank>` whose rows all name its project (legacy-bank adoption), so:
//!
//! - with `ai_mnemopi_db_path` set, the v1 directory maps onto it whole;
//! - otherwise each v1 project bank whose rows all carry one existing `cwd`
//!   goes to that project's store, and the shared bank and any bank no single
//!   project owns go to `<data>/mnemopi`, reported as needing
//!   `ai_mnemopi_db_path` to be recalled.
//!
//! Lessons are stored through [`omp_tools::learn::retain_lesson`], the entry
//! point the `learn` device uses, on a runtime composed by
//! [`omp_envd::memory::start`] exactly as a session in that project composes
//! it, so they land in the bank the device writes for that project.

use std::{
	collections::HashSet,
	ffi::OsString,
	fmt::{self, Write as _},
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{Str, StrMut};
use omp_envd::{
	host_settings::HostSettings,
	vcs::{RepositoryAvailability, RepositorySnapshot},
};
use omp_memory::{MemoryBackend, MemoryRuntime, recall::RecallBounds};
use serde::{
	Deserialize, Deserializer,
	de::{self, IgnoredAny, MapAccess, Visitor},
};

use super::{DataImportError, copied, counted, same_file, sqlite::V1Database, subject};
use crate::v1_import::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, NotMigratable, StepContext,
	V1Item, V2Target,
	report::{Attention, SkipReason},
};

/// The primary (shared-bank) database file name in a Mnemopi directory.
const MNEMOPI_DB: &str = "mnemopi.db";
/// The memory session that authors imported lessons.
const IMPORT_SESSION: &str = "v1-import";
/// The v1 `local` backend's lessons file in a project memory directory.
const LEARNED_FILE: &str = "learned.md";

fn finish(step: ImportStep, cx: &StepContext<'_>) -> Result<(), ImportError> {
	if cx.mode == ImportMode::Apply {
		step
			.marker(&cx.pair.target.config_dir)
			.set(None)
			.map_err(DataImportError::write(&cx.pair.target.config_dir))?;
	}
	Ok(())
}

/// The memory settings a session of the target profile (in `project`, whose
/// `.omp/config.cfg` overlays the profile's) runs with.
fn memory_settings(
	target: &V2Target,
	project: Option<&Path>,
) -> Result<HostSettings, DataImportError> {
	let ctx = omp_con::Ctx::new();
	let files = crate::cfg::CfgFiles::with_roots(
		target.config_dir.clone(),
		project.map(|project| project.join(".omp")),
	);
	ctx.exec_configs(&files, None)
		.map_err(|source| DataImportError::Config { path: target.config_dir.clone(), source })?;
	Ok(HostSettings::from_con(&ctx))
}

/// The directory a session in the canonical `project` keeps its banks in.
fn project_bank_dir(
	target: &V2Target,
	project: &Path,
	settings: &HostSettings,
) -> Result<PathBuf, DataImportError> {
	let state = omp_env::project_state::directory(&target.data_dir, project)
		.map_err(DataImportError::read(project))?;
	Ok(omp_memory::runtime::database_dir(
		&omp_envd::memory::memory_data_dir(&state),
		&settings.mnemopi,
	))
}

// ── mnemopi ─────────────────────────────────────────────────────────────

/// One v1 Mnemopi database: the shared bank, or a named project bank.
struct V1Store {
	bank: Option<OsString>,
	path: PathBuf,
}

/// Where one v1 store goes.
enum Placement {
	/// Where a v2 runtime recalls it.
	Recalled(PathBuf),
	/// Under `<data>/mnemopi`, recalled only once `ai_mnemopi_db_path` names it.
	Unscoped(PathBuf),
	/// Its project no longer exists.
	ProjectMissing(PathBuf),
}

pub(in crate::v1_import) fn import_mnemopi(
	cx: &StepContext<'_>,
) -> Result<Vec<ImportEntry>, ImportError> {
	let entry = |path: &Path, subject, outcome| ImportEntry {
		step: ImportStep::Mnemopi,
		item: V1Item::MnemopiMemory,
		path: Some(path.to_owned()),
		subject,
		outcome,
	};
	let root = cx.locate(V1Item::MnemopiMemory);
	let stores = root
		.as_deref()
		.map(v1_stores)
		.transpose()?
		.unwrap_or_default();
	if stores.is_empty() {
		finish(ImportStep::Mnemopi, cx)?;
		return Ok(vec![ImportEntry::new(
			ImportStep::Mnemopi,
			V1Item::MnemopiMemory,
			root,
			ImportOutcome::NothingToImport,
		)]);
	}
	let target = &cx.pair.target;
	let profile = memory_settings(target, None)?;
	let mut entries = Vec::with_capacity(stores.len());
	for store in &stores {
		let (destination, unscoped) = match place(target, &profile, store)? {
			Placement::Recalled(destination) => (destination, false),
			Placement::Unscoped(destination) => (destination, true),
			Placement::ProjectMissing(project) => {
				entries.push(entry(
					&store.path,
					Some(subject(&project)),
					ImportOutcome::Skipped(SkipReason::ProjectMissing),
				));
				continue;
			},
		};
		let label = Some(subject(&destination));
		if same_file(&store.path, &destination) {
			entries.push(entry(&store.path, label, ImportOutcome::Skipped(SkipReason::SharedWithV2)));
			continue;
		}
		if destination.exists() {
			entries.push(entry(&store.path, label, ImportOutcome::Skipped(SkipReason::TargetExists)));
			continue;
		}
		if cx.mode == ImportMode::Apply {
			V1Database::open(&store.path)?.copy_to(&destination)?;
		}
		entries.push(entry(&store.path, label.clone(), copied(cx.mode)));
		if unscoped {
			entries.push(entry(
				&store.path,
				label,
				ImportOutcome::NeedsAttention(Attention::MnemopiStoreUnscoped),
			));
		}
	}
	finish(ImportStep::Mnemopi, cx)?;
	Ok(entries)
}

/// The shared bank, then every project bank by name.
fn v1_stores(root: &Path) -> Result<Vec<V1Store>, DataImportError> {
	let mut stores = Vec::new();
	let shared = root.join(MNEMOPI_DB);
	if shared.is_file() {
		stores.push(V1Store { bank: None, path: shared });
	}
	let banks = root.join("banks");
	let listing = match fs::read_dir(&banks) {
		Ok(listing) => listing,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(stores),
		Err(source) => return Err(DataImportError::Read { path: banks, source }),
	};
	let mut named = Vec::new();
	for bank in listing {
		let bank = bank.map_err(DataImportError::read(&banks))?;
		let path = bank.path().join(MNEMOPI_DB);
		if path.is_file() {
			named.push(V1Store { bank: Some(bank.file_name()), path });
		}
	}
	named.sort_by(|left, right| left.bank.cmp(&right.bank));
	stores.extend(named);
	Ok(stores)
}

fn place(
	target: &V2Target,
	profile: &HostSettings,
	store: &V1Store,
) -> Result<Placement, DataImportError> {
	let in_dir = |directory: &Path| match &store.bank {
		None => directory.join(MNEMOPI_DB),
		Some(bank) => directory.join("banks").join(bank).join(MNEMOPI_DB),
	};
	// An install-wide v2 store takes the v1 directory as it was.
	if let Some(primary) = &profile.mnemopi.db_path {
		return Ok(Placement::Recalled(match &store.bank {
			None => primary.clone(),
			Some(_) => in_dir(&omp_memory::runtime::database_dir(&target.data_dir, &profile.mnemopi)),
		}));
	}
	let unscoped = || Placement::Unscoped(in_dir(&target.data_dir.join("mnemopi")));
	if store.bank.is_none() {
		return Ok(unscoped());
	}
	let Some(cwd) = single_cwd(&store.path) else {
		return Ok(unscoped());
	};
	let Ok(project) = fs::canonicalize(&cwd) else {
		return Ok(Placement::ProjectMissing(cwd));
	};
	if !project.is_dir() {
		return Ok(Placement::ProjectMissing(cwd));
	}
	let settings = memory_settings(target, Some(&project))?;
	Ok(Placement::Recalled(in_dir(&project_bank_dir(target, &project, &settings)?)))
}

/// The one working directory every row of a v1 bank was written from, which
/// is what v2's legacy-bank adoption matches; `None` for a bank that is
/// empty, mixed, or unreadable.
fn single_cwd(path: &Path) -> Option<PathBuf> {
	let database = V1Database::open(path).ok()?;
	let connection = database.connect().ok()?;
	let (total, distinct, missing, cwd) = connection
		.query_row(
			"SELECT count(*), count(DISTINCT json_extract(metadata_json, '$.cwd')),
			        sum(json_extract(metadata_json, '$.cwd') IS NULL),
			        max(json_extract(metadata_json, '$.cwd'))
			 FROM working_memory",
			[],
			|row| {
				Ok((
					row.get::<_, i64>(0)?,
					row.get::<_, i64>(1)?,
					row.get::<_, Option<i64>>(2)?,
					row.get::<_, Option<String>>(3)?,
				))
			},
		)
		.ok()?;
	(total > 0 && distinct == 1 && missing == Some(0))
		.then(|| cwd.map(PathBuf::from))
		.flatten()
}

// ── learned-lessons ────────────────────────────────────────────────────

/// One `- <lesson> _(context: <context>)_` bullet of a `learned.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Lesson<'a> {
	content: &'a str,
	context: Option<&'a str>,
}

/// Lessons stored, and lessons Mnemopi already had.
#[derive(Clone, Copy, Default)]
struct Stored {
	new:       usize,
	duplicate: usize,
}

pub(in crate::v1_import) fn import_learned(
	cx: &StepContext<'_>,
) -> Result<Vec<ImportEntry>, ImportError> {
	let entry = |path: &Path, subject, outcome| ImportEntry {
		step: ImportStep::LearnedLessons,
		item: V1Item::Memories,
		path: Some(path.to_owned()),
		subject,
		outcome,
	};
	let root = cx.locate(V1Item::Memories);
	let files = root
		.as_deref()
		.map(learned_files)
		.transpose()?
		.unwrap_or_default();
	if files.is_empty() {
		finish(ImportStep::LearnedLessons, cx)?;
		return Ok(vec![ImportEntry::new(
			ImportStep::LearnedLessons,
			V1Item::Memories,
			root,
			ImportOutcome::NothingToImport,
		)]);
	}
	let target = &cx.pair.target;
	let mut entries = Vec::with_capacity(files.len());
	let mut total = 0;
	let mut inactive = false;
	for (encoded, file) in &files {
		let text = fs::read_to_string(file).map_err(DataImportError::read(file))?;
		let lessons = parse_lessons(&text);
		let Some(project) = decode_project(encoded) else {
			entries.push(entry(
				file,
				Some(subject(&naive_project(encoded))),
				ImportOutcome::Skipped(SkipReason::ProjectMissing),
			));
			continue;
		};
		if lessons.is_empty() {
			entries.push(entry(file, Some(subject(&project)), ImportOutcome::NothingToImport));
			continue;
		}
		let settings = memory_settings(target, Some(&project))?;
		let stored = match cx.mode {
			ImportMode::DryRun => Stored { new: lessons.len(), duplicate: 0 },
			ImportMode::Apply => store_lessons(target, &project, &settings, &lessons)?,
		};
		if stored.new > 0 {
			inactive |= settings.memory.backend != MemoryBackend::Mnemopi;
			total += stored.new;
			entries.push(entry(
				file,
				Some(counted(Some(&project), stored.new, "lesson", "")),
				copied(cx.mode),
			));
		}
		if stored.duplicate > 0 {
			entries.push(entry(
				file,
				Some(counted(Some(&project), stored.duplicate, "lesson", " already stored")),
				ImportOutcome::Skipped(SkipReason::TargetExists),
			));
		}
	}
	if inactive {
		entries.push(ImportEntry {
			step:    ImportStep::LearnedLessons,
			item:    V1Item::Memories,
			path:    root,
			subject: Some(counted(None, total, "imported lesson", "")),
			outcome: ImportOutcome::NeedsAttention(Attention::EnableMnemopi),
		});
	}
	finish(ImportStep::LearnedLessons, cx)?;
	Ok(entries)
}

/// Every `--<encoded cwd>--/learned.md` under the v1 memory root, by name.
fn learned_files(root: &Path) -> Result<Vec<(String, PathBuf)>, DataImportError> {
	let mut files = Vec::new();
	for directory in fs::read_dir(root).map_err(DataImportError::read(root))? {
		let directory = directory.map_err(DataImportError::read(root))?;
		let name = directory.file_name();
		let Some(encoded) = name
			.to_str()
			.and_then(|name| name.strip_prefix("--"))
			.and_then(|name| name.strip_suffix("--"))
		else {
			continue;
		};
		let file = directory.path().join(LEARNED_FILE);
		if file.is_file() {
			files.push((encoded.to_owned(), file));
		}
	}
	files.sort();
	Ok(files)
}

/// The lesson bullets of a `learned.md`, as v1's `saveLearnedLesson` wrote
/// them (`- <lesson>` or `- <lesson> _(context: <context>)_`); other lines
/// (headings, prose) are not lessons.
fn parse_lessons(text: &str) -> Vec<Lesson<'_>> {
	text
		.lines()
		.filter_map(|line| line.trim_start().strip_prefix("- "))
		.map(|bullet| {
			let bullet = bullet.trim();
			match bullet
				.strip_suffix(")_")
				.and_then(|head| head.rsplit_once(" _(context: "))
			{
				Some((content, context)) => Lesson {
					content: content.trim(),
					context: Some(context.trim()).filter(|context| !context.is_empty()),
				},
				None => Lesson { content: bullet, context: None },
			}
		})
		.filter(|lesson| !lesson.content.is_empty())
		.collect()
}

/// Recovers the project directory v1 encoded as `<cwd without its leading
/// separator, with every '/', '\' and ':' turned into '-'>`
/// (`encodeProjectPath` in v1's `memories/index.ts`, the same scheme as its
/// absolute session directories).
///
/// The encoding is lossy (`-` is also legal inside names), so the directory
/// tree decides: each level takes the longest existing entry whose encoded
/// name the rest starts with, backtracking on a dead end. `None` when no
/// existing directory encodes to `encoded`.
fn decode_project(encoded: &str) -> Option<PathBuf> {
	#[cfg(windows)]
	if let [drive, b'-', b'-', ..] = encoded.as_bytes()
		&& drive.is_ascii_alphabetic()
	{
		let mut root = String::with_capacity(3);
		root.push(char::from(*drive));
		root.push_str(":\\");
		return decode_under(Path::new(&root), &encoded[3..]);
	}
	decode_under(Path::new("/"), encoded)
}

fn decode_under(directory: &Path, rest: &str) -> Option<PathBuf> {
	if rest.is_empty() {
		return Some(directory.to_owned());
	}
	let mut candidates = fs::read_dir(directory)
		.ok()?
		.filter_map(Result::ok)
		.filter_map(|entry| {
			let name = entry.file_name();
			let encoded = name.to_str()?.replace(['/', '\\', ':'], "-");
			let tail = rest.strip_prefix(encoded.as_str())?;
			let tail = if tail.is_empty() {
				tail
			} else {
				tail.strip_prefix('-')?
			};
			entry
				.path()
				.is_dir()
				.then_some((encoded.len(), name, tail.len()))
		})
		.collect::<Vec<_>>();
	candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
	candidates
		.into_iter()
		.find_map(|(_, name, tail)| decode_under(&directory.join(name), &rest[rest.len() - tail..]))
}

/// A readable guess at a project that no longer exists: every `-` a
/// separator.
fn naive_project(encoded: &str) -> PathBuf {
	let mut path = String::with_capacity(encoded.len() + 1);
	path.push('/');
	path.push_str(&encoded.replace('-', "/"));
	PathBuf::from(path)
}

/// Stores `lessons` in `project`'s bank the way the `learn` device does,
/// skipping any the scoped banks already hold.
fn store_lessons(
	target: &V2Target,
	project: &Path,
	settings: &HostSettings,
	lessons: &[Lesson<'_>],
) -> Result<Stored, DataImportError> {
	let memory = |source| DataImportError::Memory { project: project.to_owned(), source };
	let state = omp_env::project_state::directory(&target.data_dir, project)
		.map_err(DataImportError::read(project))?;
	let primary_root = omp_vcs::detect(project)
		.map_err(|source| DataImportError::Repository { path: project.to_owned(), source })?
		.map(|repository| repository.primary_root());
	let snapshot = RepositorySnapshot {
		availability: if primary_root.is_some() {
			RepositoryAvailability::Available
		} else {
			RepositoryAvailability::NotRepository
		},
		worktree_root: None,
		primary_root,
		head: None,
		branch: None,
		status_counts: Default::default(),
	};
	let registered = omp_envd::memory::start(
		MemoryBackend::Mnemopi,
		&settings.mnemopi,
		&state,
		IMPORT_SESSION,
		project.to_owned(),
		Some(&snapshot),
	)
	.map_err(memory)?;
	let runtime = registered.runtime();
	let mut seen = HashSet::with_capacity(lessons.len());
	let mut stored = Stored::default();
	for lesson in lessons {
		if !seen.insert(lesson.content) || already_stored(runtime, lesson.content).map_err(memory)? {
			stored.duplicate += 1;
			continue;
		}
		omp_tools::learn::retain_lesson(runtime, lesson.content, lesson.context).map_err(memory)?;
		stored.new += 1;
	}
	Ok(stored)
}

/// Whether a bank the runtime recalls already holds `lesson` verbatim.
fn already_stored(runtime: &MemoryRuntime, lesson: &str) -> omp_memory::Result<bool> {
	let bounds = RecallBounds { limit: 50, token_budget: 32 * 1024, voice_limit: 100 };
	Ok(runtime
		.search(lesson, None, bounds)?
		.items
		.iter()
		.any(|item| item.memory.content.trim() == lesson))
}

// ── memory-backends ────────────────────────────────────────────────────

/// The v1 `config.yml` memory keys v2 has no backend for.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct V1MemorySettings {
	memory:       V1MemorySection,
	hindsight:    KeyCount,
	sharpshooter: KeyCount,
	memories:     KeyCount,
}

/// How many settings a v1 section holds (`null` or absent is none); the
/// values are never read.
#[derive(Clone, Copy, Debug, Default)]
struct KeyCount(usize);

impl<'de> Deserialize<'de> for KeyCount {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		struct Keys;

		impl<'de> Visitor<'de> for Keys {
			type Value = KeyCount;

			fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
				formatter.write_str("a settings section")
			}

			fn visit_unit<E: de::Error>(self) -> Result<KeyCount, E> {
				Ok(KeyCount(0))
			}

			fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeyCount, A::Error> {
				let mut count = 0;
				while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
					count += 1;
				}
				Ok(KeyCount(count))
			}
		}

		deserializer.deserialize_any(Keys)
	}
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct V1MemorySection {
	backend: Option<V1MemoryBackend>,
}

/// v1 `memory.backend`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, strum::IntoStaticStr)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
enum V1MemoryBackend {
	Off,
	Local,
	Hindsight,
	Mnemopi,
	Sharpshooter,
	#[serde(other)]
	Unknown,
}

pub(in crate::v1_import) fn report_backends(
	cx: &StepContext<'_>,
) -> Result<Vec<ImportEntry>, ImportError> {
	let path = cx.locate(V1Item::Settings);
	let settings = match &path {
		Some(path) => {
			let text = fs::read_to_string(path).map_err(DataImportError::read(path))?;
			if text.trim().is_empty() {
				V1MemorySettings::default()
			} else {
				serde_yaml::from_str::<V1MemorySettings>(&text)
					.map_err(|source| DataImportError::Yaml { path: path.clone(), source })?
			}
		},
		None => V1MemorySettings::default(),
	};
	let dropped = |subject: Str| ImportEntry {
		step:    ImportStep::MemoryBackends,
		item:    V1Item::Settings,
		path:    path.clone(),
		subject: Some(subject),
		outcome: ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
	};
	let mut entries = Vec::new();
	if let Some(
		backend @ (V1MemoryBackend::Local
		| V1MemoryBackend::Hindsight
		| V1MemoryBackend::Sharpshooter),
	) = settings.memory.backend
	{
		let name: &'static str = backend.into();
		let mut text = StrMut::new("memory.backend ");
		text.push_str(name);
		if backend == V1MemoryBackend::Local {
			text.push_str(" (its summaries; `learned.md` lessons move to Mnemopi)");
		}
		entries.push(dropped(text.freeze()));
	}
	for (prefix, keys, note) in [
		("hindsight", &settings.hindsight, ""),
		("sharpshooter", &settings.sharpshooter, ""),
		("memories", &settings.memories, ", the `local` backend's tuning"),
	] {
		let KeyCount(count) = *keys;
		if count > 0 {
			let mut text = StrMut::default();
			let _ =
				write!(text, "{prefix}.* ({count} setting{}{note})", if count == 1 { "" } else { "s" });
			entries.push(dropped(text.freeze()));
		}
	}
	if entries.is_empty() {
		entries.push(ImportEntry::new(
			ImportStep::MemoryBackends,
			V1Item::Settings,
			path,
			ImportOutcome::NothingToImport,
		));
	}
	finish(ImportStep::MemoryBackends, cx)?;
	Ok(entries)
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn lesson_bullets_keep_their_context() {
		let text = "# Lessons\n\nSome prose.\n- Run `just fmt` first _(context: CI failed)_\n  - \
		            nested plain lesson\n- \n-no space is not a bullet\n- a _(context: )_\n";
		assert_eq!(parse_lessons(text), [
			Lesson { content: "Run `just fmt` first", context: Some("CI failed") },
			Lesson { content: "nested plain lesson", context: None },
			Lesson { content: "a", context: None },
		]);
	}

	#[test]
	fn a_project_path_decodes_against_the_directory_tree() {
		let root = tempfile::tempdir().expect("scratch");
		let project = root.path().join("my-app/src-tauri");
		fs::create_dir_all(&project).expect("project");
		fs::create_dir_all(root.path().join("my/app")).expect("decoy");
		let canonical = fs::canonicalize(&project).expect("canonical");
		let encoded = canonical.to_str().expect("utf-8")[1..].replace(['/', '\\', ':'], "-");
		assert_eq!(decode_project(&encoded), Some(canonical));
		assert_eq!(decode_project(&format!("{encoded}-missing")), None);
		assert_eq!(naive_project("home-me-gone"), Path::new("/home/me/gone"));
	}
}
