//! The `keybindings` step: v1 `keybindings.yml` into `bind` lines appended to
//! the paired v2 profile's `config.cfg`.
//!
//! v1 (`config/keybindings.ts` on `main`) keeps a flat map
//! `action: chord | [chord, …]` in `<agent>/keybindings.yml`, else `.yaml`,
//! else a legacy `.json` (JSONC) it rewrites to yml. Pre-namespace action
//! names (`selectModel`) are renamed on load. A named profile merges the
//! default profile's map under its own, action by action. A user entry
//! replaces the action's default chords; `[]` leaves the action unbound.
//!
//! The import replays that onto v2's bind table: the default bind cfg, then
//! the target's own `config.cfg`. Each remapped action's command leaves the
//! chords the defaults gave it ([`strip_command`]) and joins the chords v1
//! listed, beside any contextual commands already there. The resulting
//! divergence is appended as `unbind`/`bind` lines under a header comment.
//! Chords the existing `config.cfg` already binds or unbinds are never
//! touched: the existing bind wins and the report says so. Actions v2 has no
//! command for, and values that are not chords, are reported and kept as
//! comments in the block.

use std::{
	fmt::{self, Write as _},
	io,
	path::{Path, PathBuf},
};

use omp_con::{ConError, Ctx, Source, Value, normalize_chord};
use omp_core::{FastHashSet, Str};
use serde::{
	Deserialize, Deserializer,
	de::{IgnoredAny, MapAccess, Visitor},
};
use smallvec::SmallVec;
use thiserror::Error;

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item,
	report::{Attention, NotMigratable, SkipReason},
};
use crate::{
	cfg::{ConfigFileLock, migrate_config_script, read_config},
	keybindings::{DEFAULT_BINDS, DEFAULT_BINDS_NAME, PI_ACTIONS, pi_action_command, strip_command},
};

/// v1 `KEYBINDING_NAME_MIGRATIONS`: pre-namespace action names renamed on
/// load.
const LEGACY_ACTION_NAMES: &[(&str, &str)] = &[
	("interrupt", "app.interrupt"),
	("clear", "app.clear"),
	("exit", "app.exit"),
	("suspend", "app.suspend"),
	("displayReset", "app.display.reset"),
	("cycleThinkingLevel", "app.thinking.cycle"),
	("cycleModelForward", "app.model.cycleForward"),
	("cycleModelBackward", "app.model.cycleBackward"),
	("selectModel", "app.model.select"),
	("selectModelTemporary", "app.model.selectTemporary"),
	("togglePlanMode", "app.plan.toggle"),
	("historySearch", "app.history.search"),
	("expandTools", "app.tools.expand"),
	("toggleThinking", "app.thinking.toggle"),
	("externalEditor", "app.editor.external"),
	("followUp", "app.message.followUp"),
	("retry", "app.retry"),
	("dequeue", "app.message.dequeue"),
	("pasteImage", "app.clipboard.pasteImage"),
	("pasteTextRaw", "app.clipboard.pasteTextRaw"),
	("copyLine", "app.clipboard.copyLine"),
	("copyPrompt", "app.clipboard.copyPrompt"),
	("newSession", "app.session.new"),
	("tree", "app.session.tree"),
	("fork", "app.session.fork"),
	("resume", "app.session.resume"),
	("observeSessions", "app.session.observe"),
	("toggleSTT", "app.stt.toggle"),
	("cursorUp", "tui.editor.cursorUp"),
	("cursorDown", "tui.editor.cursorDown"),
	("cursorLeft", "tui.editor.cursorLeft"),
	("cursorRight", "tui.editor.cursorRight"),
	("cursorWordLeft", "tui.editor.cursorWordLeft"),
	("cursorWordRight", "tui.editor.cursorWordRight"),
	("cursorLineStart", "tui.editor.cursorLineStart"),
	("cursorLineEnd", "tui.editor.cursorLineEnd"),
	("jumpForward", "tui.editor.jumpForward"),
	("jumpBackward", "tui.editor.jumpBackward"),
	("pageUp", "tui.editor.pageUp"),
	("pageDown", "tui.editor.pageDown"),
	("deleteCharBackward", "tui.editor.deleteCharBackward"),
	("deleteCharForward", "tui.editor.deleteCharForward"),
	("deleteWordBackward", "tui.editor.deleteWordBackward"),
	("deleteWordForward", "tui.editor.deleteWordForward"),
	("deleteToLineStart", "tui.editor.deleteToLineStart"),
	("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
	("yank", "tui.editor.yank"),
	("yankPop", "tui.editor.yankPop"),
	("undo", "tui.editor.undo"),
	("newLine", "tui.input.newLine"),
	("submit", "tui.input.submit"),
	("tab", "tui.input.tab"),
	("copy", "tui.input.copy"),
	("selectUp", "tui.select.up"),
	("selectDown", "tui.select.down"),
	("selectPageUp", "tui.select.pageUp"),
	("selectPageDown", "tui.select.pageDown"),
	("selectConfirm", "tui.select.confirm"),
	("selectCancel", "tui.select.cancel"),
	("toggleSessionNamedFilter", "app.session.togglePath"),
];

/// A failure reading v1 keybindings or appending them to `config.cfg`.
#[derive(Debug, Error)]
pub enum KeybindingsImportError {
	/// The v1 file could not be read.
	#[error("could not read {}", path.display())]
	Read {
		/// The v1 keybindings file.
		path:   PathBuf,
		/// Typed filesystem failure.
		#[source]
		source: io::Error,
	},
	/// The v1 yml/yaml file is not an action map.
	#[error("{} is not a v1 keybindings map", path.display())]
	Yaml {
		/// The v1 keybindings file.
		path:   PathBuf,
		/// Typed parse failure.
		#[source]
		source: serde_yaml::Error,
	},
	/// The legacy v1 JSON file is not an action map.
	#[error("{} is not a v1 keybindings map", path.display())]
	Json {
		/// The v1 keybindings file.
		path:   PathBuf,
		/// Typed parse failure.
		#[source]
		source: omp_core::slopjson::ParseError,
	},
	/// The target `config.cfg` could not be read, replayed, or written.
	#[error("could not update the binds in {}", path.display())]
	Config {
		/// The target `config.cfg`.
		path:   PathBuf,
		/// Typed command-stream failure.
		#[source]
		source: ConError,
	},
	/// The step's marker could not be written.
	#[error("could not record the keybindings import")]
	Marker(#[source] io::Error),
}

/// One v1 action's value, as v1's `toKeybindingsConfig` accepts it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum V1Keys {
	/// `action: ctrl+x`.
	One(Str),
	/// `action: [ctrl+x, …]`; `[]` unbinds the action.
	Many(Vec<Str>),
	/// `action: ~`: v1 keeps the defaults.
	Unset(()),
	/// Anything else, which v1 drops.
	Invalid(IgnoredAny),
}

/// One v1 keybindings file, in file order, with legacy names renamed.
#[derive(Debug, Default)]
struct V1Bindings(Vec<(Str, V1Keys)>);

impl V1Bindings {
	/// Sets `action` like a JS object assignment: an existing key keeps its
	/// position and takes the new value.
	fn set(&mut self, action: Str, keys: V1Keys) {
		match self.0.iter_mut().find(|(known, _)| *known == action) {
			Some(slot) => slot.1 = keys,
			None => self.0.push((action, keys)),
		}
	}
}

impl<'de> Deserialize<'de> for V1Bindings {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		struct Map;
		impl<'de> Visitor<'de> for Map {
			type Value = V1Bindings;

			fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
				formatter.write_str("a map of keybinding actions to chords")
			}

			fn visit_unit<E>(self) -> Result<V1Bindings, E> {
				Ok(V1Bindings::default())
			}

			fn visit_none<E>(self) -> Result<V1Bindings, E> {
				Ok(V1Bindings::default())
			}

			fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<V1Bindings, A::Error> {
				let mut bindings = V1Bindings(Vec::with_capacity(map.size_hint().unwrap_or(0)));
				while let Some((action, keys)) = map.next_entry::<Str, V1Keys>()? {
					let action = LEGACY_ACTION_NAMES
						.iter()
						.find_map(|(legacy, current)| (*legacy == action).then_some(*current))
						.map_or(action, Str::new_static);
					bindings.set(action, keys);
				}
				Ok(bindings)
			}
		}
		deserializer.deserialize_any(Map)
	}
}

fn read_bindings(path: &Path) -> Result<V1Bindings, KeybindingsImportError> {
	let text = std::fs::read_to_string(path)
		.map_err(|source| KeybindingsImportError::Read { path: path.to_owned(), source })?;
	if path
		.extension()
		.is_some_and(|extension| extension == "json")
	{
		omp_core::slopjson::from_str(&text)
			.map_err(|source| KeybindingsImportError::Json { path: path.to_owned(), source })
	} else {
		serde_yaml::from_str(&text)
			.map_err(|source| KeybindingsImportError::Yaml { path: path.to_owned(), source })
	}
}

/// One merged v1 action and the file it came from.
struct Binding<'p> {
	action:    Str,
	keys:      V1Keys,
	/// The v1 file the action's value came from.
	from:      &'p Path,
	/// Whether the action came from the default profile's file.
	inherited: bool,
}

/// What replaying the v1 map onto the target's binds produced.
struct Plan {
	entries: Vec<ImportEntry>,
	/// The block to append to `config.cfg`; empty when nothing changes.
	block:   String,
}

/// Where a statement runs within one chord's script: modal panels first,
/// then app actions, then base-editor fallbacks (the default cfg's order).
fn rank(statement: &str) -> u8 {
	if statement.starts_with("panel_") {
		0
	} else if statement.starts_with("ed_") {
		2
	} else {
		1
	}
}

/// `new` commands joined with the chord's `current` script: each rank keeps
/// the v1 commands ahead of the ones already bound.
fn merged_script(new: &[&'static str], current: Option<&str>) -> Str {
	let mut statements = SmallVec::<(u8, bool, &str), 6>::new();
	let existing = current
		.into_iter()
		.flat_map(|script| script.split(';'))
		.map(str::trim)
		.filter(|statement| !statement.is_empty());
	for (fresh, statement) in new
		.iter()
		.copied()
		.map(|statement| (true, statement))
		.chain(existing.map(|statement| (false, statement)))
	{
		if !statements.iter().any(|(_, _, known)| *known == statement) {
			statements.push((rank(statement), !fresh, statement));
		}
	}
	statements.sort_by_key(|(rank, existing, _)| (*rank, *existing));
	let mut script = String::with_capacity(32);
	for (_, _, statement) in statements {
		if !script.is_empty() {
			script.push_str("; ");
		}
		script.push_str(statement);
	}
	Str::new(script)
}

/// The target's effective binds (defaults, then `config.cfg`) and the chords
/// its `config.cfg` binds or unbinds.
fn current_binds(config: Option<&str>) -> Result<(Ctx, FastHashSet<Str>), ConError> {
	let load = |ctx: &Ctx| {
		ctx.exec_configs(
			&|name: &str| {
				Ok((name == "config.cfg")
					.then(|| config.map(Str::new))
					.flatten())
			},
			None,
		)
	};
	let ctx = Ctx::new();
	ctx.exec(DEFAULT_BINDS, Source::Config(Str::new_static(DEFAULT_BINDS_NAME)))?;
	ctx.seal_bind_defaults();
	load(&ctx)?;
	let mut owned = FastHashSet::default();
	if let Some((removed, changed)) = ctx.bind_diff() {
		owned.extend(removed);
		owned.extend(changed.into_iter().map(|(chord, _)| chord));
	}
	// A line rebinding a chord to its default script is still the owner's.
	let alone = Ctx::new();
	load(&alone)?;
	owned.extend(alone.binds().into_iter().map(|(chord, _)| chord));
	// The effective table is the baseline the appended block diffs against.
	ctx.seal_bind_defaults();
	Ok((ctx, owned))
}

/// A one-line comment carrying user text verbatim but for control
/// characters.
fn comment(block: &mut String, parts: fmt::Arguments<'_>) {
	let text = fmt::format(parts);
	block.push_str("// ");
	for ch in text.chars() {
		if ch.is_control() {
			block.extend(ch.escape_default());
		} else {
			block.push(ch);
		}
	}
	block.push('\n');
}

fn chord_list(chords: &[Str]) -> String {
	let mut list = String::with_capacity(chords.len() * 8);
	for chord in chords {
		if !list.is_empty() {
			list.push_str(", ");
		}
		list.push_str(chord.as_str());
	}
	list
}

fn plan(
	bindings: &[Binding<'_>],
	sources: (Option<&Path>, Option<&Path>),
	config: Option<&str>,
	mode: ImportMode,
) -> Result<Plan, ConError> {
	let (ctx, owned) = current_binds(config)?;
	let has_binds = !owned.is_empty();
	let imported = || match mode {
		ImportMode::Apply => ImportOutcome::Imported,
		ImportMode::DryRun => ImportOutcome::WouldImport,
	};
	let entry = |binding: &Binding<'_>, subject: Str, outcome| ImportEntry {
		step: ImportStep::Keybindings,
		item: V1Item::Keybindings,
		path: Some(binding.from.to_owned()),
		subject: Some(subject),
		outcome,
	};
	let chord_subject = |action: &str, chord: &str| {
		let mut subject = String::with_capacity(action.len() + chord.len() + 2);
		subject.push_str(action);
		subject.push_str(": ");
		subject.push_str(chord);
		Str::new(subject)
	};
	let mut entries = Vec::with_capacity(bindings.len());
	let mut notes = String::new();
	// Actions whose v1 entry replaces their default chords.
	let mut overridden = FastHashSet::<&str>::default();
	// Chords to install, in v1 order, each with its commands.
	let mut installs = Vec::<(Str, SmallVec<&'static str, 2>)>::new();
	for binding in bindings {
		let action = binding.action.as_str();
		if binding.inherited && has_binds {
			entries.push(entry(
				binding,
				binding.action.clone(),
				ImportOutcome::Skipped(SkipReason::TargetExists),
			));
			continue;
		}
		let chords = match &binding.keys {
			V1Keys::One(chord) => std::slice::from_ref(chord),
			V1Keys::Many(chords) => chords.as_slice(),
			V1Keys::Unset(()) => {
				entries.push(entry(binding, binding.action.clone(), ImportOutcome::NothingToImport));
				continue;
			},
			V1Keys::Invalid(_) => {
				comment(
					&mut notes,
					format_args!("not imported: v1 `{action}` is not a chord or chord list"),
				);
				entries.push(entry(
					binding,
					binding.action.clone(),
					ImportOutcome::NeedsAttention(Attention::InvalidKeybinding),
				));
				continue;
			},
		};
		let Some(command) = pi_action_command(action) else {
			comment(
				&mut notes,
				format_args!("not imported: v1 `{action}` ({}) has no v2 command", chord_list(chords)),
			);
			entries.push(entry(
				binding,
				binding.action.clone(),
				ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
			));
			continue;
		};
		if chords.is_empty() {
			overridden.insert(action);
			entries.push(entry(binding, binding.action.clone(), imported()));
			continue;
		}
		let mut seen = SmallVec::<Str, 4>::new();
		for chord in chords {
			let Ok(normalized) = normalize_chord(chord.as_str()) else {
				comment(
					&mut notes,
					format_args!("not imported: v1 `{action}` chord `{chord}` is not a key chord"),
				);
				entries.push(entry(
					binding,
					chord_subject(action, chord),
					ImportOutcome::NeedsAttention(Attention::InvalidKeybinding),
				));
				continue;
			};
			if seen.contains(&normalized) {
				continue;
			}
			seen.push(normalized.clone());
			if owned.contains(&normalized) {
				comment(
					&mut notes,
					format_args!("kept the existing bind for {normalized} over v1 `{action}`"),
				);
				entries.push(entry(
					binding,
					chord_subject(action, &normalized),
					ImportOutcome::Skipped(SkipReason::ChordBound),
				));
				continue;
			}
			overridden.insert(action);
			entries.push(entry(binding, chord_subject(action, &normalized), imported()));
			match installs.iter_mut().find(|(known, _)| *known == normalized) {
				Some((_, commands)) => commands.push(command),
				None => installs.push((normalized, SmallVec::from([command]))),
			}
		}
	}

	// A command leaves its default chords only once every action sharing it
	// (`tui.select.up` and `tui.editor.cursorUp` both run `ed_up`) is remapped.
	let mut stripped = SmallVec::<&'static str, 8>::new();
	for (_, command) in PI_ACTIONS
		.iter()
		.filter(|(action, _)| overridden.contains(action))
	{
		if stripped.contains(command)
			|| !PI_ACTIONS
				.iter()
				.filter(|(_, shared)| shared == command)
				.all(|(action, _)| overridden.contains(action))
		{
			continue;
		}
		stripped.push(command);
		strip_command(&ctx, command, |chord| owned.contains(chord))?;
	}
	for (chord, commands) in &installs {
		let current = ctx.bound(chord.as_str());
		ctx.bind(chord.clone(), merged_script(commands, current.as_deref()))?;
	}

	let mut block = String::new();
	let (removed, changed) = ctx.bind_diff().unwrap_or_default();
	if !removed.is_empty() || !changed.is_empty() || !notes.is_empty() {
		let (own, inherited) = sources;
		block.push_str("\n// Imported from omp v1 keybindings");
		if let Some(own) = own {
			let _ = write!(block, ": {}", own.display());
		}
		block.push('\n');
		if let Some(inherited) = inherited {
			comment(
				&mut block,
				format_args!("inherited from the v1 default profile: {}", inherited.display()),
			);
		}
		for chord in removed {
			let _ = writeln!(block, "unbind {}", Value::Str(chord));
		}
		for (chord, script) in changed {
			let _ = writeln!(block, "bind {} {}", Value::Str(chord), Value::Str(script));
		}
		block.push_str(&notes);
	}
	Ok(Plan { entries, block })
}

pub(super) fn import_keybindings(cx: &StepContext<'_>) -> Result<Vec<ImportEntry>, ImportError> {
	let own = cx.locate(V1Item::Keybindings);
	let inherited = cx.pair.source.locate_inherited(V1Item::Keybindings);
	let marker = ImportStep::Keybindings.marker(&cx.pair.target.config_dir);
	let finish = |entries| -> Result<Vec<ImportEntry>, ImportError> {
		if cx.mode == ImportMode::Apply {
			let source = own
				.as_deref()
				.or(inherited.as_deref())
				.and_then(Path::file_name)
				.and_then(|name| name.to_str());
			marker.set(source).map_err(KeybindingsImportError::Marker)?;
		}
		Ok(entries)
	};
	if own.is_none() && inherited.is_none() {
		return finish(vec![ImportEntry::new(
			ImportStep::Keybindings,
			V1Item::Keybindings,
			None,
			ImportOutcome::NothingToImport,
		)]);
	}

	// v1 `loadMergedKeybindingsConfig`: `{ ...inherited, ...profile }`.
	let mut bindings = Vec::<Binding<'_>>::new();
	for (path, is_inherited) in [(inherited.as_deref(), true), (own.as_deref(), false)] {
		let Some(path) = path else {
			continue;
		};
		for (action, keys) in read_bindings(path)?.0 {
			let binding = Binding { action, keys, from: path, inherited: is_inherited };
			match bindings
				.iter_mut()
				.find(|known| known.action == binding.action)
			{
				Some(slot) => *slot = binding,
				None => bindings.push(binding),
			}
		}
	}
	if bindings.is_empty() {
		return finish(vec![ImportEntry::new(
			ImportStep::Keybindings,
			V1Item::Keybindings,
			own.clone().or_else(|| inherited.clone()),
			ImportOutcome::NothingToImport,
		)]);
	}

	let path = cx.pair.target.config_dir.join("config.cfg");
	let config_error = |source| KeybindingsImportError::Config { path: path.clone(), source };
	let sources = (own.as_deref(), inherited.as_deref());
	let current = read_config(&path).map_err(config_error)?;
	let planned = plan(&bindings, sources, current.as_deref(), cx.mode).map_err(config_error)?;
	if cx.mode == ImportMode::DryRun || planned.block.is_empty() {
		return finish(planned.entries);
	}
	// Re-plan under the lock, against the text actually being extended.
	let transaction = ConfigFileLock::acquire(path.clone()).map_err(config_error)?;
	let raw = transaction.read().map_err(config_error)?;
	let current = raw
		.as_deref()
		.map(|text| migrate_config_script(&path, text))
		.transpose()
		.map_err(config_error)?;
	let planned = plan(&bindings, sources, current.as_deref(), cx.mode).map_err(config_error)?;
	let mut text = raw.unwrap_or_default();
	if !text.is_empty() && !text.ends_with('\n') {
		text.push('\n');
	}
	text.push_str(if text.is_empty() {
		planned.block.trim_start_matches('\n')
	} else {
		&planned.block
	});
	transaction.replace(&text).map_err(config_error)?;
	finish(planned.entries)
}
