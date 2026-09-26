//! Legacy settings documents folded into archive-layer convars.
//!
//! Two formats share this path: the early omp2 TOML settings (`omp config
//! migrate`) and v1's YAML `config.yml` (the v1 import). Both parse into one
//! [`LegacyMap`] tree. Each convar declares the document paths it takes over
//! through `"legacy.path"` metadata ([`LEGACY_PATH`]), and [`convert`] turns
//! the value found at one of those paths into the convar's typed value,
//! including the shape changes between the formats (inverted booleans,
//! percentages, millisecond counts, selector chains, path-scoped lists).
//!
//! A path is looked up the way v1's `getByPath` did: one map level per
//! `.`-separated segment. A `null` value is absent.

use std::{
	fmt, fs, io,
	path::{Path, PathBuf},
};

use omp_con::{ConError, Ctx, Kv, Origin, Span, TypeSpec, Value, ValueKind, VarView};
use omp_core::Str;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use thiserror::Error;

/// Metadata key naming a legacy document path a convar takes over.
pub const LEGACY_PATH: &str = "legacy.path";

/// The key the `toml` deserializer wraps a datetime in.
const TOML_DATETIME_KEY: &str = "$__toml_private_datetime";

/// One value of a legacy settings document.
#[derive(Clone, Debug, PartialEq)]
pub enum LegacyValue {
	/// YAML `null` or an empty value: treated as absent.
	Null,
	/// Boolean.
	Bool(bool),
	/// Integer that fits `i64`.
	Int(i64),
	/// Float (also integers beyond `i64`).
	Float(f64),
	/// String (TOML datetimes arrive as their text).
	Str(Str),
	/// Sequence.
	List(Vec<Self>),
	/// Nested table.
	Map(LegacyMap),
}

/// A legacy settings table, in document order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LegacyMap(Vec<(Str, LegacyValue)>);

impl LegacyValue {
	/// The string, when this is one.
	#[must_use]
	pub fn as_str(&self) -> Option<&str> {
		match self {
			Self::Str(value) => Some(value.as_str()),
			_ => None,
		}
	}

	/// The boolean, when this is one.
	#[must_use]
	pub const fn as_bool(&self) -> Option<bool> {
		match self {
			Self::Bool(value) => Some(*value),
			_ => None,
		}
	}

	/// The integer, when this is one.
	#[must_use]
	pub const fn as_int(&self) -> Option<i64> {
		match self {
			Self::Int(value) => Some(*value),
			_ => None,
		}
	}

	/// Any number as a float.
	#[must_use]
	pub const fn as_number(&self) -> Option<f64> {
		match self {
			Self::Int(value) => Some(*value as f64),
			Self::Float(value) => Some(*value),
			_ => None,
		}
	}

	/// The table, when this is one.
	#[must_use]
	pub const fn as_map(&self) -> Option<&LegacyMap> {
		match self {
			Self::Map(map) => Some(map),
			_ => None,
		}
	}
}

impl LegacyMap {
	/// Whether the table has no entries.
	#[must_use]
	pub const fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Entries in document order.
	pub fn iter(&self) -> impl ExactSizeIterator<Item = (&Str, &LegacyValue)> + Clone + '_ {
		self.0.iter().map(|(key, value)| (key, value))
	}

	/// The value stored under `key`.
	#[must_use]
	pub fn get(&self, key: &str) -> Option<&LegacyValue> {
		self
			.0
			.iter()
			.find(|(candidate, _)| candidate == key)
			.map(|(_, value)| value)
	}

	/// Stores `value` under `key`, replacing an earlier value in place.
	pub fn insert(&mut self, key: Str, value: LegacyValue) {
		match self.0.iter_mut().find(|(candidate, _)| *candidate == key) {
			Some((_, slot)) => *slot = value,
			None => self.0.push((key, value)),
		}
	}

	/// Folds `incoming` over this table: nested tables merge, every other
	/// value replaces.
	pub fn merge(&mut self, incoming: Self) {
		for (key, value) in incoming.0 {
			match (self.0.iter_mut().find(|(candidate, _)| *candidate == key), value) {
				(Some((_, LegacyValue::Map(target))), LegacyValue::Map(incoming)) => {
					target.merge(incoming);
				},
				(_, value) => self.insert(key, value),
			}
		}
	}

	/// The value at a `.`-separated path, one table level per segment. A
	/// `null` value is absent.
	#[must_use]
	pub fn value_at(&self, path: &str) -> Option<&LegacyValue> {
		let mut segments = path.split('.');
		let mut value = self.get(segments.next()?)?;
		for segment in segments {
			value = value.as_map()?.get(segment)?;
		}
		(*value != LegacyValue::Null).then_some(value)
	}
}

impl<'de> Deserialize<'de> for LegacyValue {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_any(ValueVisitor)
	}
}

impl<'de> Deserialize<'de> for LegacyMap {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		match LegacyValue::deserialize(deserializer)? {
			LegacyValue::Map(map) => Ok(map),
			LegacyValue::Null => Ok(Self::default()),
			_ => Err(de::Error::custom("expected a settings table")),
		}
	}
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
	type Value = LegacyValue;

	fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("a settings value")
	}

	fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
		Ok(LegacyValue::Bool(value))
	}

	fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
		Ok(LegacyValue::Int(value))
	}

	fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
		Ok(i64::try_from(value).map_or(LegacyValue::Float(value as f64), LegacyValue::Int))
	}

	fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
		Ok(LegacyValue::Float(value))
	}

	fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
		Ok(LegacyValue::Str(Str::new(value)))
	}

	fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
		Ok(LegacyValue::Str(Str::from(value)))
	}

	fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
		Ok(LegacyValue::Null)
	}

	fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
		Ok(LegacyValue::Null)
	}

	fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
		LegacyValue::deserialize(deserializer)
	}

	fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
		let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0));
		while let Some(value) = seq.next_element()? {
			values.push(value);
		}
		Ok(LegacyValue::List(values))
	}

	fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
		let mut table = LegacyMap::default();
		while let Some(LegacyKey(key)) = map.next_key()? {
			let value: LegacyValue = map.next_value()?;
			if key == TOML_DATETIME_KEY && table.is_empty() {
				return Ok(value);
			}
			table.insert(key, value);
		}
		Ok(LegacyValue::Map(table))
	}
}

/// A table key: YAML allows scalars other than strings.
struct LegacyKey(Str);

impl<'de> Deserialize<'de> for LegacyKey {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		struct KeyVisitor;

		impl Visitor<'_> for KeyVisitor {
			type Value = LegacyKey;

			fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
				formatter.write_str("a scalar settings key")
			}

			fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
				Ok(LegacyKey(Str::new(value)))
			}

			fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
				Ok(LegacyKey(Str::from(value)))
			}

			fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
				Ok(LegacyKey(Str::new_static(if value { "true" } else { "false" })))
			}

			fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
				Ok(LegacyKey(Str::from(value.to_string())))
			}

			fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
				Ok(LegacyKey(Str::from(value.to_string())))
			}

			fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
				Ok(LegacyKey(Str::from(value.to_string())))
			}
		}

		deserializer.deserialize_any(KeyVisitor)
	}
}

/// Why a legacy value does not convert into its convar's type.
#[derive(Debug, Error)]
pub enum LegacyValueError {
	/// A boolean was expected.
	#[error("expected a boolean")]
	ExpectedBool,
	/// An integer was expected.
	#[error("expected an integer")]
	ExpectedInt,
	/// A number was expected.
	#[error("expected a number")]
	ExpectedNumber,
	/// A string (or another scalar) was expected.
	#[error("expected a string")]
	ExpectedString,
	/// An enum variant name was expected.
	#[error("expected an enum name")]
	ExpectedEnum,
	/// `on` or `off` was expected.
	#[error("expected `on` or `off`")]
	ExpectedOnOff,
	/// A list was expected.
	#[error("expected a list")]
	ExpectedList,
	/// A table was expected.
	#[error("expected a table")]
	ExpectedTable,
	/// A duration string or millisecond count was expected.
	#[error("expected a duration")]
	ExpectedDuration,
	/// The duration text did not parse.
	#[error("invalid duration")]
	Duration(#[source] omp_core::DurationError),
	/// A millisecond or second count was negative.
	#[error("expected a non-negative count")]
	Negative,
	/// A kibibyte size does not fit a byte count.
	#[error("kilobyte value is out of range")]
	KibibytesOutOfRange,
	/// v2 has no such memory backend (v1 `hindsight`, `sharpshooter`).
	#[error("v2 has no `{backend}` memory backend")]
	UnsupportedMemoryBackend {
		/// The v1 backend name.
		backend: Str,
	},
}

/// A convar whose legacy value could not be folded.
#[derive(Debug, Error)]
pub enum LegacySettingsError {
	/// A legacy settings file could not be read.
	#[error("could not read {}", path.display())]
	Read {
		/// The file.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A legacy TOML settings file did not parse.
	#[error("{} is not valid TOML", path.display())]
	Toml {
		/// The file.
		path:   PathBuf,
		/// Parse failure.
		#[source]
		source: toml::de::Error,
	},
	/// A legacy YAML settings file did not parse.
	#[error("{} is not a valid YAML settings table", path.display())]
	Yaml {
		/// The file.
		path:   PathBuf,
		/// Parse failure.
		#[source]
		source: serde_yaml::Error,
	},
	/// The value at a legacy path does not convert into its convar's type.
	#[error("legacy setting `{path}` does not convert into `{var}`")]
	Value {
		/// The legacy document path.
		path:   Str,
		/// The convar it feeds.
		var:    Str,
		/// Why.
		#[source]
		source: LegacyValueError,
	},
	/// The convar rejected the converted value.
	#[error("`{var}` rejected the legacy value")]
	Set {
		/// The convar.
		var:    Str,
		/// The console's rejection.
		#[source]
		source: ConError,
	},
}

fn read_text(path: &Path) -> Result<Option<String>, LegacySettingsError> {
	match fs::read_to_string(path) {
		Ok(text) => Ok(Some(text)),
		Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
		Err(source) => Err(LegacySettingsError::Read { path: path.to_owned(), source }),
	}
}

/// Reads legacy TOML documents, later sources overriding earlier ones. Missing
/// files are skipped.
///
/// # Errors
///
/// Returns [`LegacySettingsError`] when a present file cannot be read or
/// parsed.
pub fn read_toml_documents(sources: &[PathBuf]) -> Result<LegacyMap, LegacySettingsError> {
	let mut document = LegacyMap::default();
	for source in sources {
		let Some(text) = read_text(source)? else {
			continue;
		};
		let incoming = toml::from_str::<LegacyMap>(&text)
			.map_err(|error| LegacySettingsError::Toml { path: source.clone(), source: error })?;
		document.merge(incoming);
	}
	Ok(document)
}

/// Reads one YAML settings document (v1 `config.yml`). An empty or missing
/// file is an empty table.
///
/// # Errors
///
/// Returns [`LegacySettingsError`] when the file cannot be read, does not
/// parse, or is not a table.
pub fn read_yaml_document(path: &Path) -> Result<LegacyMap, LegacySettingsError> {
	let Some(text) = read_text(path)? else {
		return Ok(LegacyMap::default());
	};
	if text.trim().is_empty() {
		return Ok(LegacyMap::default());
	}
	serde_yaml::from_str::<LegacyMap>(&text)
		.map_err(|source| LegacySettingsError::Yaml { path: path.to_owned(), source })
}

/// What one convar takes from a legacy document.
#[derive(Debug)]
pub struct VarFold {
	/// Every declared legacy path present in the document, in declaration
	/// order.
	pub paths:   Vec<Str>,
	/// The folded value (`None` when the present paths set nothing), or the
	/// first path that failed.
	pub outcome: Result<Option<Value>, (Str, LegacyValueError)>,
}

/// Folds every legacy path `var` declares, in declaration order: each present
/// path converts on top of what the earlier ones produced. `None` when the
/// document has none of them.
#[must_use]
pub fn fold_var(document: &LegacyMap, var: &VarView<'_>) -> Option<VarFold> {
	let mut paths = Vec::new();
	let mut current = None;
	for path in var.meta_all(LEGACY_PATH) {
		let Some(value) = document.value_at(path) else {
			continue;
		};
		let path = Str::new(path);
		match convert(&path, value, var, current.take()) {
			Ok(value) => current = value,
			Err(error) => {
				paths.push(path.clone());
				return Some(VarFold { paths, outcome: Err((path, error)) });
			},
		}
		paths.push(path);
	}
	(!paths.is_empty()).then_some(VarFold { paths, outcome: Ok(current) })
}

/// Folds a legacy document into `ctx`'s archive layer through every
/// convar's declared legacy paths.
///
/// # Errors
///
/// Returns [`LegacySettingsError`] for the first value that does not convert
/// or that its convar rejects.
pub fn fold_into(document: &LegacyMap, ctx: &Ctx) -> Result<(), LegacySettingsError> {
	for var in ctx.vars() {
		let Some(fold) = fold_var(document, &var) else {
			continue;
		};
		let value = fold
			.outcome
			.map_err(|(path, source)| LegacySettingsError::Value {
				path,
				var: Str::new(var.name),
				source,
			})?;
		if let Some(value) = value {
			ctx.set(var.name, value, Origin::Archive)
				.map_err(|source| LegacySettingsError::Set { var: Str::new(var.name), source })?;
		}
	}
	Ok(())
}

/// Folds legacy TOML documents (later sources override earlier) into a fresh
/// archive-layer context: the early omp2 `config.toml` migration.
///
/// # Errors
///
/// Returns [`LegacySettingsError`] when a source does not read or parse, or a
/// value does not convert.
pub fn fold_toml_sources(sources: &[PathBuf]) -> Result<Ctx, LegacySettingsError> {
	let document = read_toml_documents(sources)?;
	let ctx = Ctx::new();
	fold_into(&document, &ctx)?;
	Ok(ctx)
}

/// Converts the value at legacy `path` into `var`'s value.
///
/// `current` is what the var's earlier legacy paths already produced; paths
/// that merge into a block (`thinkingBudgets`, `task.prewalk`) build on it,
/// or on the default. `Ok(None)` sets nothing.
///
/// # Errors
///
/// Returns [`LegacyValueError`] when the value has the wrong shape.
pub fn convert(
	path: &str,
	value: &LegacyValue,
	var: &VarView<'_>,
	current: Option<Value>,
) -> Result<Option<Value>, LegacyValueError> {
	let ty = var.ty;
	match path {
		"display.hideToolActivity" | "hideThinkingBlock" => {
			let hidden = value.as_bool().ok_or(LegacyValueError::ExpectedBool)?;
			return Ok(Some(Value::Bool(!hidden)));
		},
		"completion.notify" | "error.notify" | "ask.notify" if value.as_str().is_some() => {
			return match value.as_str() {
				Some("on") => Ok(Some(Value::Bool(true))),
				Some("off") => Ok(Some(Value::Bool(false))),
				_ => Err(LegacyValueError::ExpectedOnOff),
			};
		},
		"compaction.thresholdPercent" => {
			let percent = value.as_number().ok_or(LegacyValueError::ExpectedNumber)?;
			return Ok(Some(Value::Float(percent / 100.0)));
		},
		"compaction.thresholdTokens" if value.as_str() == Some("default") => {
			return Ok(Some(Value::Int(-1)));
		},
		"task.isolation.enabled" => {
			let enabled = value.as_bool().ok_or(LegacyValueError::ExpectedBool)?;
			return Ok(Some(Value::Enum(Str::new_static(if enabled { "auto" } else { "none" }))));
		},
		"edit.mode" => {
			if let Some(revision) = value.as_str().and_then(edit_mode_revision) {
				return Ok(Some(Value::Str(Str::new_static(revision))));
			}
		},
		"providers.tinyModel"
		| "providers.memoryModel"
		| "providers.autoThinkingModel"
		| "providers.unexpectedStopModel"
			if value.as_str() == Some("online") =>
		{
			return Ok(Some(Value::Str(Str::new_static("@tiny"))));
		},
		"providers.fireworksTier" if value.as_str() == Some("standard") => {
			return Ok(Some(Value::Enum(Str::new_static("none"))));
		},
		"share.store" if value.as_str() == Some("blob") => {
			return Ok(Some(Value::Enum(Str::new_static("http"))));
		},
		"doubleEscapeAction" if value.as_str() == Some("rewind") => {
			return Ok(Some(Value::Str(Str::new_static("branch"))));
		},
		"memory.backend" => match value.as_str() {
			// v1's `local` pipeline is the one that wrote learned lessons; the v1
			// import carries those into Mnemopi, so `local` turns Mnemopi on.
			Some("local") => return Ok(Some(Value::Enum(Str::new_static("mnemopi")))),
			Some(backend @ ("hindsight" | "sharpshooter")) => {
				return Err(LegacyValueError::UnsupportedMemoryBackend { backend: Str::new(backend) });
			},
			_ => {},
		},
		"task.maxRuntimeMs" | "irc.timeoutMs" | "task.agentIdleTtlMs" => {
			if let Some(millis) = value.as_int() {
				let millis = u64::try_from(millis).map_err(|_| LegacyValueError::Negative)?;
				return Ok(Some(Value::Duration(if millis == 0 {
					Span::NEVER
				} else {
					Span::millis(millis)
				})));
			}
		},
		"tools.maxTimeout" => {
			if let Some(seconds) = value.as_int() {
				let seconds = u64::try_from(seconds).map_err(|_| LegacyValueError::Negative)?;
				return Ok(Some(Value::Duration(if seconds == 0 {
					Span::NEVER
				} else {
					Span::secs(seconds)
				})));
			}
		},
		"tools.artifactSpillThreshold" | "tools.artifactTailBytes" | "tools.artifactHeadBytes" => {
			let kibibytes = value.as_number().ok_or(LegacyValueError::ExpectedNumber)?;
			let bytes = kibibytes * 1024.0;
			if !bytes.is_finite() || bytes < 0.0 || bytes > i64::MAX as f64 {
				return Err(LegacyValueError::KibibytesOutOfRange);
			}
			return Ok(Some(Value::Int(bytes.round() as i64)));
		},
		// Record of selector chains: a v1 list is a fallback chain, which v2
		// spells comma-separated.
		"modelRoles" | "task.agentModelOverrides" | "task.agentPrewalk" | "task.agentAdvisor" => {
			return string_record(value).map(|kv| Some(Value::Kv(kv)));
		},
		"enabledModels" | "disabledProviders" => {
			return path_scoped(value).map(|entries| Some(Value::List(entries)));
		},
		"thinkingBudgets" => {
			let levels = value.as_map().ok_or(LegacyValueError::ExpectedTable)?;
			let mut budgets = block_base(current, var);
			for (level, budget) in levels.iter() {
				let budget = budget.as_number().ok_or(LegacyValueError::ExpectedNumber)?;
				kv_insert(&mut budgets, level.clone(), Value::Int(budget as i64));
			}
			return Ok(Some(Value::Kv(budgets)));
		},
		// v1's switch for the bundled generic `task` agent, which the per-agent
		// map (declared first, so it wins) spells `task: on`.
		"task.prewalk" => {
			let enabled = value.as_bool().ok_or(LegacyValueError::ExpectedBool)?;
			let mut overrides = block_base(current, var);
			if enabled && overrides.get("task").is_none() {
				overrides
					.0
					.push((Str::new_static("task"), Value::Str(Str::new_static("on"))));
				return Ok(Some(Value::Kv(overrides)));
			}
			return Ok((!overrides.is_empty()).then_some(Value::Kv(overrides)));
		},
		_ => {},
	}
	to_value(value, ty).map(Some)
}

fn edit_mode_revision(mode: &str) -> Option<&'static str> {
	Some(match mode {
		"apply_patch" => "apply_patch.1",
		"hashline" => "hl.1",
		"patch" => "patch.2",
		"replace" => "rep.2",
		"sloppy" => "sloppy.1",
		_ => return None,
	})
}

/// The block a merging path builds on: what earlier paths produced, else the
/// var's default.
fn block_base(current: Option<Value>, var: &VarView<'_>) -> Kv {
	match current.unwrap_or_else(|| var.default()) {
		Value::Kv(kv) => kv,
		_ => Kv::new(),
	}
}

fn kv_insert(kv: &mut Kv, key: Str, value: Value) {
	match kv.0.iter_mut().find(|(candidate, _)| *candidate == key) {
		Some((_, slot)) => *slot = value,
		None => kv.0.push((key, value)),
	}
}

/// A record of strings, lists joined by commas, and booleans as `on`/`off`.
fn string_record(value: &LegacyValue) -> Result<Kv, LegacyValueError> {
	let map = value.as_map().ok_or(LegacyValueError::ExpectedTable)?;
	map.iter()
		.filter(|(_, value)| **value != LegacyValue::Null)
		.map(|(key, value)| {
			let text = match value {
				LegacyValue::Str(text) => text.clone(),
				LegacyValue::Bool(true) => Str::new_static("on"),
				LegacyValue::Bool(false) => Str::new_static("off"),
				LegacyValue::List(items) => {
					let mut joined = String::new();
					for item in items {
						let item = item.as_str().ok_or(LegacyValueError::ExpectedString)?;
						if !joined.is_empty() {
							joined.push(',');
						}
						joined.push_str(item);
					}
					Str::from(joined)
				},
				_ => return Err(LegacyValueError::ExpectedString),
			};
			Ok((key.clone(), Value::Str(text)))
		})
		.collect::<Result<Vec<_>, _>>()
		.map(Kv)
}

/// v1's path-scoped string lists: a bare string, or a table naming path
/// prefixes and the values that apply under them.
fn path_scoped(value: &LegacyValue) -> Result<Vec<Value>, LegacyValueError> {
	let LegacyValue::List(entries) = value else {
		return Err(LegacyValueError::ExpectedList);
	};
	entries
		.iter()
		.map(|entry| match entry {
			LegacyValue::Str(text) => {
				Ok(Value::Kv(Kv(vec![(Str::new_static("value"), Value::Str(text.clone()))])))
			},
			LegacyValue::Map(fields) => fields
				.iter()
				.map(|(field, values)| {
					let field = match field.as_str() {
						"pathPrefix" => Str::new_static("path_prefix"),
						"pathPrefixes" => Str::new_static("path_prefixes"),
						_ => field.clone(),
					};
					let values = match values {
						LegacyValue::Str(text) => vec![Value::Str(text.clone())],
						LegacyValue::List(items) => items
							.iter()
							.map(|item| {
								item
									.as_str()
									.map(|text| Value::Str(Str::new(text)))
									.ok_or(LegacyValueError::ExpectedString)
							})
							.collect::<Result<_, _>>()?,
						_ => return Err(LegacyValueError::ExpectedString),
					};
					Ok((field, Value::List(values)))
				})
				.collect::<Result<Vec<_>, _>>()
				.map(|fields| Value::Kv(Kv(fields))),
			_ => Err(LegacyValueError::ExpectedTable),
		})
		.collect()
}

fn scalar_text(value: &LegacyValue) -> Option<Str> {
	match value {
		LegacyValue::Str(text) => Some(text.clone()),
		LegacyValue::Bool(flag) => Some(Str::new_static(if *flag { "true" } else { "false" })),
		LegacyValue::Int(number) => Some(Str::from(number.to_string())),
		LegacyValue::Float(number) => Some(Str::from(number.to_string())),
		LegacyValue::Null | LegacyValue::List(_) | LegacyValue::Map(_) => None,
	}
}

fn to_value(value: &LegacyValue, ty: &TypeSpec) -> Result<Value, LegacyValueError> {
	match ty.kind {
		ValueKind::Bool => value
			.as_bool()
			.map(Value::Bool)
			.ok_or(LegacyValueError::ExpectedBool),
		ValueKind::Int => value
			.as_int()
			.map(Value::Int)
			.ok_or(LegacyValueError::ExpectedInt),
		ValueKind::Float => value
			.as_number()
			.map(Value::Float)
			.ok_or(LegacyValueError::ExpectedNumber),
		ValueKind::Str => scalar_text(value)
			.map(Value::Str)
			.ok_or(LegacyValueError::ExpectedString),
		ValueKind::Enum => value
			.as_str()
			.map(|text| Value::Enum(Str::new(text)))
			.ok_or(LegacyValueError::ExpectedEnum),
		ValueKind::Duration => {
			let span = if let Some(text) = value.as_str() {
				text.parse::<Span>().map_err(LegacyValueError::Duration)?
			} else {
				let millis = value
					.as_int()
					.and_then(|millis| u64::try_from(millis).ok())
					.ok_or(LegacyValueError::ExpectedDuration)?;
				Span::millis(millis)
			};
			Ok(Value::Duration(span))
		},
		ValueKind::List => {
			let LegacyValue::List(values) = value else {
				return Err(LegacyValueError::ExpectedList);
			};
			let elem = ty.elem.unwrap_or(TypeSpec::STR);
			values
				.iter()
				.map(|value| {
					if elem.kind == ValueKind::Kv
						&& let Some(text) = value.as_str()
					{
						return Ok(Value::Kv(Kv(vec![(
							Str::new_static("value"),
							Value::Str(Str::new(text)),
						)])));
					}
					to_value(value, elem)
				})
				.collect::<Result<Vec<_>, _>>()
				.map(Value::List)
		},
		ValueKind::Kv => value
			.as_map()
			.ok_or(LegacyValueError::ExpectedTable)
			.map(|table| Value::Kv(untyped_kv(table))),
	}
}

fn untyped_kv(table: &LegacyMap) -> Kv {
	Kv(table
		.iter()
		.filter(|(_, value)| **value != LegacyValue::Null)
		.map(|(key, value)| (key.clone(), untyped_value(value)))
		.collect())
}

fn untyped_value(value: &LegacyValue) -> Value {
	match value {
		LegacyValue::Str(text) => Value::Str(text.clone()),
		LegacyValue::Int(number) => Value::Int(*number),
		LegacyValue::Float(number) => Value::Float(*number),
		LegacyValue::Bool(flag) => Value::Bool(*flag),
		LegacyValue::Null => Value::Str(Str::default()),
		LegacyValue::List(values) => Value::List(values.iter().map(untyped_value).collect()),
		LegacyValue::Map(table) => Value::Kv(untyped_kv(table)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn yaml(text: &str) -> LegacyMap {
		serde_yaml::from_str(text).expect("yaml")
	}

	fn var<'c>(ctx: &'c Ctx, name: &str) -> VarView<'c> {
		ctx.vars()
			.find(|var| var.name == name)
			.expect("registered convar")
	}

	fn fold(ctx: &Ctx, document: &LegacyMap, name: &str) -> Option<Value> {
		fold_var(document, &var(ctx, name))
			.expect("a legacy path is present")
			.outcome
			.expect("converts")
	}

	#[test]
	fn yaml_and_toml_parse_into_one_tree() {
		let from_yaml =
			yaml("retry:\n  enabled: false\n  maxRetries: 5\ntheme: ~\nbig: 18446744073709551615\n");
		assert_eq!(from_yaml.value_at("retry.enabled"), Some(&LegacyValue::Bool(false)));
		assert_eq!(from_yaml.value_at("retry.maxRetries"), Some(&LegacyValue::Int(5)));
		assert_eq!(from_yaml.value_at("theme"), None, "null is absent");
		assert_eq!(from_yaml.value_at("retry.enabled.deeper"), None);
		assert!(matches!(from_yaml.value_at("big"), Some(LegacyValue::Float(_))));
		let from_toml =
			toml::from_str::<LegacyMap>("when = 1979-05-27T07:32:00Z\n[retry]\nenabled = false\n")
				.expect("toml");
		assert_eq!(from_toml.value_at("retry.enabled"), Some(&LegacyValue::Bool(false)));
		assert_eq!(
			from_toml.value_at("when").and_then(LegacyValue::as_str),
			Some("1979-05-27T07:32:00Z")
		);
		assert!(serde_yaml::from_str::<LegacyMap>("- a\n- b\n").is_err(), "a list is not a table");
	}

	#[test]
	fn converters_reshape_legacy_values() {
		let ctx = Ctx::new();
		let document = yaml(concat!(
			"compaction:\n  thresholdPercent: 80\n",
			"task:\n  agentIdleTtlMs: 90000\n  maxRuntimeMs: 0\n",
			"modelRoles:\n  default: anthropic/claude-opus\n  smol: [openai/gpt-mini, google/flash]\n",
			"thinkingBudgets:\n  high: 20000\n",
			"memory:\n  backend: local\n",
		));
		assert_eq!(fold(&ctx, &document, "ai_compact_threshold"), Some(Value::Float(0.8)));
		assert_eq!(
			fold(&ctx, &document, "sv_task_agent_idle_ttl"),
			Some(Value::Duration(Span::millis(90_000)))
		);
		assert_eq!(fold(&ctx, &document, "sv_task_max_runtime"), Some(Value::Duration(Span::NEVER)));
		assert_eq!(
			fold(&ctx, &document, "ai_model_roles"),
			Some(Value::Kv(Kv(vec![
				(Str::new("default"), Value::Str(Str::new("anthropic/claude-opus"))),
				(Str::new("smol"), Value::Str(Str::new("openai/gpt-mini,google/flash"))),
			])))
		);
		let Some(Value::Kv(budgets)) = fold(&ctx, &document, "ai_thinking_budgets") else {
			panic!("budgets are a block");
		};
		assert_eq!(budgets.get("high"), Some(&Value::Int(20_000)));
		assert_eq!(budgets.get("minimal"), Some(&Value::Int(1_024)), "unset levels keep defaults");
		assert_eq!(
			fold(&ctx, &document, "ai_memory_backend"),
			Some(Value::Enum(Str::new_static("mnemopi")))
		);
		// Inverted booleans need no registered convar to prove.
		let shown = convert(
			"hideThinkingBlock",
			&LegacyValue::Bool(true),
			&var(&ctx, "ai_compact_threshold"),
			None,
		)
		.expect("converts");
		assert_eq!(shown, Some(Value::Bool(false)));
	}

	#[test]
	fn merging_paths_build_on_earlier_ones() {
		let ctx = Ctx::new();
		let document = yaml("task:\n  prewalk: true\n  agentPrewalk:\n    explore: off\n");
		assert_eq!(
			fold(&ctx, &document, "sv_task_agent_prewalk"),
			Some(Value::Kv(Kv(vec![
				(Str::new("explore"), Value::Str(Str::new("off"))),
				(Str::new("task"), Value::Str(Str::new("on"))),
			])))
		);
		let explicit = yaml("task:\n  prewalk: true\n  agentPrewalk:\n    task: smol\n");
		assert_eq!(
			fold(&ctx, &explicit, "sv_task_agent_prewalk"),
			Some(Value::Kv(Kv(vec![(Str::new("task"), Value::Str(Str::new("smol")))]))),
			"an explicit per-agent entry wins over the generic switch"
		);
		assert_eq!(fold(&ctx, &yaml("task:\n  prewalk: false\n"), "sv_task_agent_prewalk"), None);
	}

	#[test]
	fn path_scoped_lists_rename_prefix_fields() {
		let ctx = Ctx::new();
		let document = yaml(concat!(
			"enabledModels:\n",
			"  - anthropic/*\n",
			"  - pathPrefix: ~/work\n    models: openai/*\n",
		));
		assert_eq!(
			fold(&ctx, &document, "ai_model_enabled_models"),
			Some(Value::List(vec![
				Value::Kv(Kv(vec![(Str::new("value"), Value::Str(Str::new("anthropic/*")))])),
				Value::Kv(Kv(vec![
					(Str::new("path_prefix"), Value::List(vec![Value::Str(Str::new("~/work"))])),
					(Str::new("models"), Value::List(vec![Value::Str(Str::new("openai/*"))])),
				])),
			]))
		);
	}

	#[test]
	fn dropped_memory_backends_are_typed_errors() {
		let ctx = Ctx::new();
		let fold =
			fold_var(&yaml("memory:\n  backend: hindsight\n"), &var(&ctx, "ai_memory_backend"))
				.expect("present");
		assert!(matches!(
			fold.outcome,
			Err((_, LegacyValueError::UnsupportedMemoryBackend { ref backend })) if backend == "hindsight"
		));
	}
}
