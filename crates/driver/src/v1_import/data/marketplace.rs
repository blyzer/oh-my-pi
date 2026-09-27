//! The `marketplace` step: v1's Claude-format marketplace registry, installed
//! plugins, and their cache (owner decision #14).
//!
//! v2 keeps the same files at the same places under its profile data
//! directory (`omp ext`'s state paths): `marketplaces.json`,
//! `plugins/installed_plugins.json`, and
//! `plugins/cache/{marketplaces,plugins}`. The installed-plugins registry is
//! read and written as [`omp_ext::claude_plugin::InstalledPluginsRegistry`],
//! the type `omp ext` writes and runtime discovery
//! ([`omp_ext::claude_plugin::ClaudePlugins::resolve`]) reads, so an imported
//! enabled plugin loads on the next launch. Entries merge by marketplace name
//! and plugin id, and an entry v2 already has wins. Paths into v1's plugin
//! cache are rebased onto v2's copy, so v2 never reads the v1 tree. v1's usage
//! statistics (`stats.db` and the usage tables of `agent.db`) are dropped and
//! reported.

use std::{
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{Str, StrMut};
use omp_ext::claude_plugin::{InstalledPluginsRegistry, REGISTRY_FILE, user_plugins_dir};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{DataImportError, copied, copy_tree, counted, same_file, sqlite::V1Database};
use crate::v1_import::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, NotMigratable, StepContext,
	V1Item, report::SkipReason, step::atomic_replace,
};

/// `marketplaces.json`: the catalogs added with `/marketplace add`.
#[derive(Debug, Deserialize, Serialize)]
struct MarketplacesRegistry {
	#[serde(default = "marketplaces_version")]
	version:      u32,
	#[serde(default)]
	marketplaces: Vec<MarketplaceEntry>,
}

impl Default for MarketplacesRegistry {
	fn default() -> Self {
		Self { version: marketplaces_version(), marketplaces: Vec::new() }
	}
}

const fn marketplaces_version() -> u32 {
	1
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MarketplaceEntry {
	name:         Str,
	source_type:  Str,
	source_uri:   Str,
	catalog_path: PathBuf,
	added_at:     Str,
	updated_at:   Str,
}

/// v1 tables of per-model and per-command usage in `agent.db`.
const USAGE_TABLES: [&str; 3] = ["model_usage", "model_perf", "command_usage"];

/// The two plugin caches under `plugins/cache`, with the noun each reports.
const CACHES: [(&str, &str); 2] =
	[("marketplaces", "cached marketplace"), ("plugins", "cached plugin")];

pub(in crate::v1_import) fn import(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let target = &cx.pair.target;
	let v2_plugins = user_plugins_dir(&target.data_dir);
	let v1_plugins = cx.locate(V1Item::Plugins);
	let prefix = v1_plugins
		.clone()
		.or_else(|| cx.pair.source.candidates(V1Item::Plugins).next());
	let rebase = |path: &mut PathBuf| {
		if let Some(relative) = prefix
			.as_deref()
			.and_then(|prefix| path.strip_prefix(prefix).ok())
		{
			*path = v2_plugins.join(relative);
		}
	};
	let mut entries = Vec::new();

	if let Some(v1) = cx.locate(V1Item::Marketplaces) {
		let v2 = target.data_dir.join("marketplaces.json");
		let entry = |subject, outcome| ImportEntry {
			step: ImportStep::Marketplace,
			item: V1Item::Marketplaces,
			path: Some(v1.clone()),
			subject,
			outcome,
		};
		if same_file(&v1, &v2) {
			entries.push(entry(None, ImportOutcome::Skipped(SkipReason::SharedWithV2)));
		} else {
			let (new, kept) =
				merge::<MarketplacesRegistry>(&v1, &v2, cx.mode, |incoming, existing| {
					let mut new = 0;
					for mut marketplace in incoming.marketplaces {
						if existing
							.marketplaces
							.iter()
							.any(|present| present.name == marketplace.name)
						{
							continue;
						}
						rebase(&mut marketplace.catalog_path);
						existing.marketplaces.push(marketplace);
						new += 1;
					}
					existing
						.marketplaces
						.sort_by(|left, right| left.name.cmp(&right.name));
					new
				})?;
			report_merge(&mut entries, entry, cx.mode, new, kept, "marketplace");
		}
	}

	if let Some(v1_plugins) = &v1_plugins {
		let entry = |path: &Path, subject, outcome| ImportEntry {
			step: ImportStep::Marketplace,
			item: V1Item::Plugins,
			path: Some(path.to_owned()),
			subject,
			outcome,
		};
		if same_file(v1_plugins, &v2_plugins) {
			entries.push(entry(v1_plugins, None, ImportOutcome::Skipped(SkipReason::SharedWithV2)));
		} else {
			let v1 = v1_plugins.join(REGISTRY_FILE);
			if v1.is_file() {
				let v2 = v2_plugins.join(REGISTRY_FILE);
				let (new, kept) =
					merge::<InstalledPluginsRegistry>(&v1, &v2, cx.mode, |incoming, existing| {
						let mut new = 0;
						for (id, mut installs) in incoming.plugins {
							if existing.plugins.contains_key(&id) {
								continue;
							}
							for install in &mut installs {
								rebase(&mut install.install_path);
							}
							existing.plugins.insert(id, installs);
							new += 1;
						}
						new
					})?;
				report_merge(
					&mut entries,
					|subject, outcome| entry(&v1, subject, outcome),
					cx.mode,
					new,
					kept,
					"installed plugin",
				);
				// Runtime discovery (`ClaudePlugins::resolve`) loads an enabled
				// install's skills, commands, rules, MCP servers, language
				// servers and debug adapters; the kinds it has no home for are
				// launch diagnostics (`PluginDiagnostic::Unsupported`).
				if new + kept > 0 {
					entries.push(entry(
						&v1,
						Some(Str::new_static(
							"plugin hooks, agents, JS tools and output styles (v2 loads the skills, \
							 commands, rules, MCP, LSP and DAP servers of enabled plugins and reports \
							 these at launch)",
						)),
						ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
					));
				}
			}
			for (kind, noun) in CACHES {
				let cache = v1_plugins.join("cache").join(kind);
				let copied_count =
					copy_cache(&cache, &v2_plugins.join("cache").join(kind), cx.mode, |name| {
						let mut subject = StrMut::new("cache/");
						subject.push_str(kind);
						subject.push('/');
						subject.push_str(&name.to_string_lossy());
						entries.push(entry(
							&cache,
							Some(subject.freeze()),
							ImportOutcome::Skipped(SkipReason::TargetExists),
						));
					})?;
				if copied_count > 0 {
					entries.push(entry(
						&cache,
						Some(counted(None, copied_count, noun, "")),
						copied(cx.mode),
					));
				}
			}
			let npm = v1_plugins.join("package.json");
			if npm.is_file() {
				entries.push(entry(
					&npm,
					Some(Str::new_static("npm plugins (v2 runs no JavaScript plugins)")),
					ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
				));
			}
		}
	}

	if let Some(stats) = cx.locate(V1Item::StatsDb) {
		entries.push(ImportEntry::new(
			ImportStep::Marketplace,
			V1Item::StatsDb,
			Some(stats),
			ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
		));
	}
	if let Some(agent_db) = cx.locate(V1Item::AgentDb)
		&& let Some(tables) = usage_tables(&agent_db)?
	{
		entries.push(ImportEntry {
			step:    ImportStep::Marketplace,
			item:    V1Item::AgentDb,
			path:    Some(agent_db),
			subject: Some(tables),
			outcome: ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
		});
	}

	if entries.is_empty() {
		entries.push(ImportEntry::new(
			ImportStep::Marketplace,
			V1Item::Marketplaces,
			None,
			ImportOutcome::NothingToImport,
		));
	}
	if cx.mode == ImportMode::Apply {
		ImportStep::Marketplace
			.marker(&target.config_dir)
			.set(None)
			.map_err(DataImportError::write(&target.config_dir))?;
	}
	Ok(entries)
}

/// Reads the v1 registry at `v1` and v2's at `v2` (empty when absent),
/// merges with `add` (which returns how many v1 entries it took), and, when
/// applying and something was added, writes v2's atomically. Returns the
/// added and kept-from-v2 counts.
fn merge<R>(
	v1: &Path,
	v2: &Path,
	mode: ImportMode,
	add: impl FnOnce(R, &mut R) -> usize,
) -> Result<(usize, usize), DataImportError>
where
	R: Serialize + Registry,
{
	let incoming = R::read(v1)?;
	let total = incoming.entries();
	let mut existing = R::read(v2)?;
	let new = add(incoming, &mut existing);
	if new > 0 && mode == ImportMode::Apply {
		let bytes = serde_json::to_vec_pretty(&existing)
			.map_err(|source| DataImportError::Json { path: v2.to_owned(), source })?;
		if let Some(parent) = v2.parent() {
			fs::create_dir_all(parent).map_err(DataImportError::write(parent))?;
		}
		atomic_replace(v2, &bytes).map_err(DataImportError::write(v2))?;
	}
	Ok((new, total - new))
}

/// A JSON registry the step merges.
trait Registry: Sized {
	/// Reads the registry at `path`; a missing file is the empty registry.
	fn read(path: &Path) -> Result<Self, DataImportError>;

	/// How many entries it holds.
	fn entries(&self) -> usize;
}

impl Registry for MarketplacesRegistry {
	fn read(path: &Path) -> Result<Self, DataImportError> {
		Ok(read_json(path)?.unwrap_or_default())
	}

	fn entries(&self) -> usize {
		self.marketplaces.len()
	}
}

impl Registry for InstalledPluginsRegistry {
	/// The shared reader, which also rejects a schema version runtime
	/// discovery would skip.
	fn read(path: &Path) -> Result<Self, DataImportError> {
		Self::read(path).map_err(DataImportError::PluginRegistry)
	}

	fn entries(&self) -> usize {
		self.plugins.len()
	}
}

fn read_json<R: DeserializeOwned>(path: &Path) -> Result<Option<R>, DataImportError> {
	let bytes = match fs::read(path) {
		Ok(bytes) => bytes,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
		Err(source) => return Err(DataImportError::Read { path: path.to_owned(), source }),
	};
	serde_json::from_slice(&bytes)
		.map(Some)
		.map_err(|source| DataImportError::Json { path: path.to_owned(), source })
}

fn report_merge(
	entries: &mut Vec<ImportEntry>,
	entry: impl Fn(Option<Str>, ImportOutcome) -> ImportEntry,
	mode: ImportMode,
	new: usize,
	kept: usize,
	noun: &str,
) {
	if new > 0 {
		entries.push(entry(Some(counted(None, new, noun, "")), copied(mode)));
	}
	if kept > 0 {
		entries.push(entry(
			Some(counted(None, kept, noun, " v2 already had")),
			ImportOutcome::Skipped(SkipReason::TargetExists),
		));
	}
	if new + kept == 0 {
		entries.push(entry(None, ImportOutcome::NothingToImport));
	}
}

/// Copies each entry of the v1 cache directory `source` that `target` lacks
/// (skipping interrupted `.tmp-*` downloads), reporting the rest to
/// `conflict`. Returns how many were (or would be) copied.
fn copy_cache(
	source: &Path,
	target: &Path,
	mode: ImportMode,
	mut conflict: impl FnMut(&std::ffi::OsStr),
) -> Result<usize, DataImportError> {
	let listing = match fs::read_dir(source) {
		Ok(listing) => listing,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
		Err(source_error) => {
			return Err(DataImportError::Read { path: source.to_owned(), source: source_error });
		},
	};
	let mut names = listing
		.map(|entry| entry.map(|entry| entry.file_name()))
		.collect::<Result<Vec<_>, _>>()
		.map_err(DataImportError::read(source))?;
	names.sort();
	let mut copied_count = 0;
	for name in names {
		if name.to_str().is_some_and(|name| name.starts_with(".tmp-")) {
			continue;
		}
		let destination = target.join(&name);
		if fs::symlink_metadata(&destination).is_ok() {
			conflict(&name);
			continue;
		}
		if mode == ImportMode::Apply {
			fs::create_dir_all(target).map_err(DataImportError::write(target))?;
			copy_tree(&source.join(&name), &destination)?;
		}
		copied_count += 1;
	}
	Ok(copied_count)
}

/// The non-empty v1 usage tables in `agent.db`, as a report subject.
fn usage_tables(agent_db: &Path) -> Result<Option<Str>, DataImportError> {
	let database = V1Database::open(agent_db)?;
	let connection = database.connect()?;
	let mut found = StrMut::new("usage tables:");
	let mut any = false;
	for table in USAGE_TABLES {
		let exists = connection
			.query_row(
				"SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
				[table],
				|_| Ok(()),
			)
			.map(|()| true)
			.or_else(|error| match error {
				rusqlite::Error::QueryReturnedNoRows => Ok(false),
				error => Err(error),
			})
			.map_err(DataImportError::sqlite(agent_db))?;
		if !exists {
			continue;
		}
		let mut query = StrMut::new("SELECT EXISTS (SELECT 1 FROM ");
		query.push_str(table);
		query.push(')');
		let rows = connection
			.query_row(query.as_str(), [], |row| row.get::<_, bool>(0))
			.map_err(DataImportError::sqlite(agent_db))?;
		if rows {
			found.push(' ');
			found.push_str(table);
			any = true;
		}
	}
	Ok(any.then(|| found.freeze()))
}
