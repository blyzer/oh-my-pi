//! The `settings` step: v1 `config.yml` (or `config.yaml`) into v2
//! `config.cfg`.
//!
//! # Sources and destinations
//!
//! - **User settings.** Each v1 profile's `agent/config.yml` goes to the paired
//!   v2 profile's configuration root (`~/.o2/config.cfg`,
//!   `~/.o2/profiles/<p>/config.cfg`), once, through the step marker.
//! - **Project settings.** `<project>/.omp/config.yml` goes to
//!   `<project>/.omp/config.cfg`. v1 read it from the working directory only,
//!   and v2 reads its overlay from the same directory, so the import follows
//!   the project omp runs in: the first-run hook and `omp config import-v1`
//!   import the current project, and every other repository is imported the
//!   first time omp runs there. The per-profile step marker cannot cover that
//!   (it is set on the first run anywhere), so each project has its own marker
//!   under the v2 state root, named by the SHA-256 of the project's canonical
//!   path ([`project_marker`]). It lives outside the repository so the import
//!   never adds files a repository would commit, and it is written only for
//!   projects that had a v1 file.
//!
//! # What is written
//!
//! - Every convar declares the v1 paths it takes over (`"legacy.path"`);
//!   [`crate::legacy_settings`] converts the values, shared with `omp config
//!   migrate`. Only keys present in the v1 file are considered, and a value
//!   that equals the v2 default is reported and not written.
//! - Imported lines are appended to the destination `config.cfg` under a header
//!   comment. Existing lines are never rewritten: a convar the file already
//!   sets keeps its value and the report says so.
//! - v1 `tier.subagent` becomes `ai_tier_*` lines in `subagent.cfg` (ADR 0013):
//!   children seed from the parent, so `inherit` needs no line.
//! - v1 `memory.backend: local` becomes `ai_memory_backend mnemopi`, so the
//!   lessons v1's local pipeline learned stay usable once imported. `hindsight`
//!   and `sharpshooter` have no v2 backend: the report warns.
//! - Every other v1 key (no v2 equivalent, or a value v2 rejects) is reported
//!   and written as a comment block after the imported lines (owner decision
//!   #6). Values under a key naming a secret are written as `<redacted>`. Note
//!   that a later `omp config set` or `writecfg` rewrites `config.cfg` from
//!   live values and drops comments; the report keeps the list.

use std::{
	fmt::Write as _,
	fs, io,
	path::{Path, PathBuf},
};

use omp_con::{ConError, Ctx, Origin, ParseError, Value};
use omp_core::{FastHashSet, Hash32, Str};
use thiserror::Error;

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item, V2Roots,
	report::{Attention, NotMigratable, SkipReason},
};
use crate::{
	cfg::ConfigFileLock,
	legacy_settings::{self, LegacyMap, LegacySettingsError, LegacyValue, LegacyValueError},
};

/// v1's project configuration directory, relative to the working directory.
const PROJECT_DIR: &str = ".omp";
/// v1's project settings file inside [`PROJECT_DIR`].
const PROJECT_FILE: &str = "config.yml";

/// v1 settings whose value is a whole record: reported and commented as one
/// key, never split into their entries.
const V1_RECORD_PATHS: &[&str] = &[
	"images.urls.credentials",
	"images.urls.options",
	"modelRoles",
	"modelTags",
	"providers.maxInFlightRequests",
	"retry.fallbackChains",
	"statusLine.segmentOptions",
	"task.agentAdvisor",
	"task.agentModelOverrides",
	"task.agentPrewalk",
	"task.agentServiceTierOverrides",
	"tools.approval",
];

/// Words that mark a key as holding a secret. A key is split into its words
/// (camelCase, `_`, `-`), so `apiToken` and `llmApiKey` match while a count
/// such as `recallMaxTokens` does not.
const SECRET_WORDS: &[&str] = &[
	"apikey",
	"credential",
	"credentials",
	"key",
	"keys",
	"passphrase",
	"password",
	"passwords",
	"secret",
	"secrets",
	"token",
];

/// Per-family `ai_tier_*` convars `tier.subagent` broadcasts to.
const TIER_FAMILIES: [&str; 3] = ["ai_tier_anthropic", "ai_tier_google", "ai_tier_openai"];

/// Why the settings step failed. Reported per file; the next run retries.
#[derive(Debug, Error)]
pub enum SettingsImportError {
	/// The v1 settings file could not be read or parsed.
	#[error("could not read the v1 settings")]
	Document(#[from] LegacySettingsError),
	/// An existing v2 cfg could not be read, locked, or written.
	#[error("could not update the v2 cfg")]
	Cfg(#[from] ConError),
	/// An existing v2 cfg does not parse, so nothing is appended to it.
	#[error("{} is not a valid cfg script", path.display())]
	ExistingCfg {
		/// The cfg file.
		path:   PathBuf,
		/// Parse failure.
		#[source]
		source: ParseError,
	},
	/// The import marker could not be written.
	#[error("could not record the settings import marker {}", path.display())]
	Marker {
		/// The marker file.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
}

/// The `settings` step for one profile pair.
pub(super) fn import_settings(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let root = &cx.pair.target.config_dir;
	let entries = match cx.locate(V1Item::Settings) {
		Some(source) => import_file(&source, root, cx.mode)?,
		None => vec![entry(None, None, ImportOutcome::NothingToImport)],
	};
	if cx.mode == ImportMode::Apply {
		let marker = ImportStep::Settings.marker(root);
		marker
			.set(None)
			.map_err(|source| SettingsImportError::Marker {
				path: marker.path().to_owned(),
				source,
			})?;
	}
	Ok(entries)
}

/// Imports `<project>/.omp/config.yml` into `<project>/.omp/config.cfg`,
/// once per project (see the module docs). Never fails: a failure is an
/// [`Attention::Failed`] entry and the next run retries.
#[must_use]
pub fn import_project_settings(
	project: &Path,
	roots: &V2Roots,
	mode: ImportMode,
) -> Vec<ImportEntry> {
	let root = project.join(PROJECT_DIR);
	let source = root.join(PROJECT_FILE);
	if !source.is_file() {
		return vec![entry(Some(&source), None, ImportOutcome::NothingToImport)];
	}
	let marker = project_marker(project, roots);
	if marker.exists() {
		return vec![entry(Some(&source), None, ImportOutcome::Skipped(SkipReason::MarkerPresent))];
	}
	let imported = import_file(&source, &root, mode).and_then(|entries| {
		if mode == ImportMode::Apply {
			write_project_marker(&marker, project)?;
		}
		Ok(entries)
	});
	imported.unwrap_or_else(|error| {
		vec![entry(
			Some(&source),
			None,
			ImportOutcome::NeedsAttention(Attention::Failed(ImportError::Settings(error))),
		)]
	})
}

/// Where the project's settings import is recorded: the v2 state root, keyed
/// by the SHA-256 of the project's canonical path.
#[must_use]
pub fn project_marker(project: &Path, roots: &V2Roots) -> PathBuf {
	let canonical = fs::canonicalize(project).unwrap_or_else(|_| project.to_owned());
	let digest = Hash32::sum(canonical.as_os_str().as_encoded_bytes());
	let mut name = String::with_capacity(".project-settings-migration-v1-".len() + 64);
	let _ = write!(name, ".project-settings-migration-v1-{}", digest.to_hex().as_str());
	roots.state_dir.join("v1-import").join(name)
}

fn write_project_marker(marker: &Path, project: &Path) -> Result<(), SettingsImportError> {
	let failed = |source| SettingsImportError::Marker { path: marker.to_owned(), source };
	if let Some(parent) = marker.parent() {
		fs::create_dir_all(parent).map_err(failed)?;
	}
	let mut contents = String::with_capacity(64);
	let _ = writeln!(contents, "revision = {}", super::Marker::REVISION);
	let _ = writeln!(contents, "project = {:?}", project.display().to_string());
	super::step::atomic_replace(marker, contents.as_bytes()).map_err(failed)
}

fn entry(source: Option<&Path>, subject: Option<Str>, outcome: ImportOutcome) -> ImportEntry {
	ImportEntry {
		step: ImportStep::Settings,
		item: V1Item::Settings,
		path: source.map(Path::to_owned),
		subject,
		outcome,
	}
}

/// Imports one v1 settings file into the cfg files under `root`.
fn import_file(
	source: &Path,
	root: &Path,
	mode: ImportMode,
) -> Result<Vec<ImportEntry>, SettingsImportError> {
	let document = legacy_settings::read_yaml_document(source)?;
	let targets = [root.join("config.cfg"), root.join("subagent.cfg")];
	let [config, subagent] = targets.each_ref().map(|path| read_cfg(path));
	let planned = plan_file(&document, source, config?.as_deref(), subagent?.as_deref(), mode)?;
	if mode == ImportMode::DryRun || planned.blocks.iter().all(Option::is_none) {
		return Ok(planned.entries);
	}
	// Lock what the plan writes, then plan again from what the locks see.
	let mut locks = [None, None];
	for ((lock, block), path) in locks.iter_mut().zip(&planned.blocks).zip(&targets) {
		if block.is_some() {
			*lock = Some(ConfigFileLock::acquire(path.clone())?);
		}
	}
	let mut texts = [None, None];
	for ((text, lock), path) in texts.iter_mut().zip(&locks).zip(&targets) {
		*text = match lock {
			Some(lock) => lock.read()?,
			None => read_cfg(path)?,
		};
	}
	let [config, subagent] = &texts;
	let planned = plan_file(&document, source, config.as_deref(), subagent.as_deref(), mode)?;
	for (((block, lock), text), path) in planned
		.blocks
		.iter()
		.zip(&mut locks)
		.zip(&texts)
		.zip(&targets)
	{
		let Some(block) = block else {
			continue;
		};
		let lock = match lock.take() {
			Some(lock) => lock,
			None => ConfigFileLock::acquire(path.clone())?,
		};
		let mut contents = text.clone().unwrap_or_default();
		if !contents.is_empty() {
			if !contents.ends_with('\n') {
				contents.push('\n');
			}
			contents.push('\n');
		}
		contents.push_str(block);
		omp_con::parse(&Str::new(&contents))
			.map_err(|source| SettingsImportError::ExistingCfg { path: path.clone(), source })?;
		lock.replace_raw(contents.as_bytes())?;
	}
	Ok(planned.entries)
}

fn read_cfg(path: &Path) -> Result<Option<String>, SettingsImportError> {
	match fs::read_to_string(path) {
		Ok(text) => Ok(Some(text)),
		Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
		Err(source) => Err(SettingsImportError::Cfg(
			omp_con::ConfigIoError::new(omp_con::ConfigOperation::Read, path.to_owned(), source)
				.into(),
		)),
	}
}

/// Convar names an existing cfg already assigns.
fn assigned(path: &Path, text: Option<&str>) -> Result<FastHashSet<Str>, SettingsImportError> {
	let Some(text) = text else {
		return Ok(FastHashSet::default());
	};
	let statements = omp_con::parse(&Str::new(text))
		.map_err(|source| SettingsImportError::ExistingCfg { path: path.to_owned(), source })?;
	Ok(statements
		.iter()
		.filter_map(|statement| statement.args.first()?.as_atom())
		.map(|name| Str::from(name.to_ascii_lowercase()))
		.collect())
}

/// What one v1 file produces: the `config.cfg` and `subagent.cfg` blocks to
/// append, and the report.
struct Plan {
	blocks:  [Option<String>; 2],
	entries: Vec<ImportEntry>,
}

/// The lines and comments one destination file receives.
#[derive(Default)]
struct Block {
	lines:    Vec<String>,
	comments: Vec<String>,
}

impl Block {
	fn text(mut self, source: &Path) -> Option<String> {
		if self.lines.is_empty() && self.comments.is_empty() {
			return None;
		}
		let source = one_line(source);
		let mut text = String::new();
		if !self.lines.is_empty() {
			self.lines.sort_unstable();
			let _ = writeln!(text, "// omp v1 settings imported from {source}");
			for line in &self.lines {
				text.push_str(line);
				text.push('\n');
			}
		}
		if !self.comments.is_empty() {
			if !text.is_empty() {
				text.push('\n');
			}
			let _ = writeln!(text, "// v1 settings with no v2 equivalent (imported from {source}):");
			for comment in &self.comments {
				text.push_str(comment);
				text.push('\n');
			}
		}
		Some(text)
	}
}

/// A path rendered on one comment line.
fn one_line(path: &Path) -> String {
	path.display().to_string().replace(['\n', '\r'], " ")
}

/// Plans one v1 document against the existing destination cfg texts.
fn plan_file(
	document: &LegacyMap,
	source: &Path,
	config_text: Option<&str>,
	subagent_text: Option<&str>,
	mode: ImportMode,
) -> Result<Plan, SettingsImportError> {
	let config_assigned = assigned(Path::new("config.cfg"), config_text)?;
	let subagent_assigned = assigned(Path::new("subagent.cfg"), subagent_text)?;
	let imported: fn() -> ImportOutcome = match mode {
		ImportMode::Apply => || ImportOutcome::Imported,
		ImportMode::DryRun => || ImportOutcome::WouldImport,
	};
	let mut entries: Vec<ImportEntry> = Vec::new();
	let mut report = |subject: String, outcome| {
		entries.push(entry(Some(source), Some(Str::from(subject)), outcome));
	};
	let mut config = Block::default();
	let mut subagent = Block::default();
	// v1 paths a convar took, and v1 paths kept as comments although a
	// convar declares them (rejected values, dropped memory backends).
	let mut taken: FastHashSet<Str> = FastHashSet::default();
	let mut kept: FastHashSet<Str> = FastHashSet::default();

	let ctx = Ctx::new();
	for var in ctx.vars() {
		let Some(fold) = legacy_settings::fold_var(document, &var) else {
			continue;
		};
		let paths = fold.paths.join(", ");
		let value = match fold.outcome {
			Ok(value) => value,
			Err((path, LegacyValueError::UnsupportedMemoryBackend { backend })) => {
				report(
					format!("{path} = {backend}"),
					ImportOutcome::NeedsAttention(Attention::MemoryBackendDropped),
				);
				kept.extend(fold.paths);
				continue;
			},
			Err(_) => {
				report(
					format!("{paths} -> {}", var.name),
					ImportOutcome::NotMigratable(NotMigratable::ValueRejected),
				);
				kept.extend(fold.paths);
				continue;
			},
		};
		let subject = format!("{paths} -> {}", var.name);
		let Some(value) = value else {
			report(subject, ImportOutcome::Skipped(SkipReason::MatchesDefault));
			taken.extend(fold.paths);
			continue;
		};
		let effective = ctx
			.set(var.name, value, Origin::Archive)
			.and_then(|_| ctx.value(var.name));
		let Ok(effective) = effective else {
			report(subject, ImportOutcome::NotMigratable(NotMigratable::ValueRejected));
			kept.extend(fold.paths);
			continue;
		};
		taken.extend(fold.paths);
		if effective == var.default() {
			report(subject, ImportOutcome::Skipped(SkipReason::MatchesDefault));
		} else if config_assigned.contains(var.name) {
			report(subject, ImportOutcome::Skipped(SkipReason::TargetExists));
		} else {
			config.lines.push(format!("{} {effective}", var.name));
			report(subject, imported());
		}
	}

	if let Some(tier) = document.value_at("tier.subagent") {
		let subject = || String::from("tier.subagent -> subagent.cfg");
		match subagent_tiers(&ctx, tier) {
			None => {
				report(subject(), ImportOutcome::NotMigratable(NotMigratable::ValueRejected));
				kept.insert(Str::new_static("tier.subagent"));
			},
			Some(lines) => {
				taken.insert(Str::new_static("tier.subagent"));
				if lines.is_empty() {
					report(subject(), ImportOutcome::Skipped(SkipReason::MatchesDefault));
				} else {
					let fresh = lines
						.into_iter()
						.filter(|(var, _)| !subagent_assigned.contains(*var))
						.map(|(_, line)| line)
						.collect::<Vec<_>>();
					if fresh.is_empty() {
						report(subject(), ImportOutcome::Skipped(SkipReason::TargetExists));
					} else {
						subagent.lines.extend(fresh);
						report(subject(), imported());
					}
				}
			},
		}
	}

	let mut walk = Walk {
		taken:    &taken,
		kept:     &kept,
		comments: &mut config.comments,
		unmapped: Vec::new(),
	};
	walk.map(document, "");
	for path in walk.unmapped {
		report(path, ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent));
	}
	drop(report);

	if entries.is_empty() {
		entries.push(entry(Some(source), None, ImportOutcome::NothingToImport));
	}
	entries.sort_by(|left, right| left.subject.cmp(&right.subject));
	Ok(Plan { blocks: [config.text(source), subagent.text(source)], entries })
}

/// The `subagent.cfg` lines for v1 `tier.subagent`, broadcast across
/// families the way v1's `serviceTierForAllFamilies` did: `OpenAI` takes any
/// tier, Anthropic only `priority`, Google only `flex` and `priority`; a
/// family that cannot realize the tier gets none. `inherit` is v2's seeding
/// and needs no line. `None` when the value is not a v1 tier.
fn subagent_tiers(ctx: &Ctx, tier: &LegacyValue) -> Option<Vec<(&'static str, String)>> {
	let tier = tier.as_str()?;
	if tier == "inherit" {
		return Some(Vec::new());
	}
	if !matches!(tier, "none" | "auto" | "default" | "flex" | "scale" | "priority") {
		return None;
	}
	TIER_FAMILIES
		.into_iter()
		.map(|var| {
			let realized = match var {
				"ai_tier_anthropic" => tier == "priority",
				"ai_tier_google" => matches!(tier, "flex" | "priority"),
				_ => true,
			};
			let value = if realized { tier } else { "none" };
			ctx.set(var, Value::Enum(Str::new(value)), Origin::Archive)
				.and_then(|_| ctx.value(var))
				.ok()
				.map(|value| (var, format!("{var} {value}")))
		})
		.collect()
}

/// Collects v1 keys no convar took, in document order, as report paths and
/// comment lines.
struct Walk<'a> {
	taken:    &'a FastHashSet<Str>,
	kept:     &'a FastHashSet<Str>,
	comments: &'a mut Vec<String>,
	unmapped: Vec<String>,
}

impl Walk<'_> {
	fn map(&mut self, map: &LegacyMap, prefix: &str) {
		for (key, value) in map.iter() {
			let mut path = String::with_capacity(prefix.len() + 1 + key.len());
			if !prefix.is_empty() {
				path.push_str(prefix);
				path.push('.');
			}
			path.push_str(key);
			if self.taken.contains(path.as_str()) || *value == LegacyValue::Null {
				continue;
			}
			if self.kept.contains(path.as_str()) {
				self.comment(&path, value);
				continue;
			}
			match value {
				LegacyValue::Map(nested) if nested.is_empty() => {},
				LegacyValue::Map(nested) if !V1_RECORD_PATHS.contains(&path.as_str()) => {
					self.map(nested, &path);
				},
				_ => {
					self.comment(&path, value);
					self.unmapped.push(path);
				},
			}
		}
	}

	fn comment(&mut self, path: &str, value: &LegacyValue) {
		let mut line = String::with_capacity(path.len() + 16);
		let _ = write!(line, "// {path} = ");
		render(&mut line, value, path.split('.').any(is_secret));
		self.comments.push(line);
	}
}

/// Whether a key names a secret: any of its words is a [`SECRET_WORDS`] entry.
fn is_secret(key: &str) -> bool {
	let mut words = Vec::new();
	let mut word = String::new();
	let mut previous_lower = false;
	for character in key.chars() {
		if !character.is_ascii_alphanumeric() {
			words.push(std::mem::take(&mut word));
			previous_lower = false;
			continue;
		}
		if character.is_ascii_uppercase() && previous_lower {
			words.push(std::mem::take(&mut word));
		}
		previous_lower = character.is_ascii_lowercase() || character.is_ascii_digit();
		word.push(character.to_ascii_lowercase());
	}
	words.push(word);
	// Acronym runs (`URLToken`) stay one word; their tail still counts.
	let joined = words.concat();
	words
		.iter()
		.any(|word| SECRET_WORDS.contains(&word.as_str()))
		|| SECRET_WORDS.iter().any(|secret| joined.ends_with(secret))
}

/// Renders a v1 value on one line, JSON-like; a secret, or anything under a
/// key naming one, is `<redacted>`.
fn render(out: &mut String, value: &LegacyValue, secret: bool) {
	if secret {
		out.push_str("<redacted>");
		return;
	}
	match value {
		LegacyValue::Null => out.push_str("null"),
		LegacyValue::Bool(flag) => {
			let _ = write!(out, "{flag}");
		},
		LegacyValue::Int(number) => {
			let _ = write!(out, "{number}");
		},
		LegacyValue::Float(number) => {
			let _ = write!(out, "{number}");
		},
		LegacyValue::Str(text) => {
			let _ = write!(out, "{}", serde_json::Value::from(text.as_str()));
		},
		LegacyValue::List(items) => {
			out.push('[');
			for (index, item) in items.iter().enumerate() {
				if index > 0 {
					out.push_str(", ");
				}
				render(out, item, false);
			}
			out.push(']');
		},
		LegacyValue::Map(fields) => {
			out.push('{');
			for (index, (key, field)) in fields.iter().enumerate() {
				if index > 0 {
					out.push_str(", ");
				}
				let _ = write!(out, "{}: ", serde_json::Value::from(key.as_str()));
				render(out, field, is_secret(key));
			}
			out.push('}');
		},
	}
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
