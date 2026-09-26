//! Typed wire shapes for local and self-hosted model-listing endpoints.
//!
//! Every endpoint answers with loosely specified JSON that differs across
//! server versions, so decoding is charitable by construction:
//!
//! - every member is a [`Field`], which records whether the key was absent,
//!   `null`, present with an unexpected shape, or present with the expected
//!   one. A wrong-shaped member never fails its object; it is treated exactly
//!   like the `serde_json::Value` accessors (`as_str`, `as_u64`, ...) the
//!   decoders once used, including "first present key wins" fallback chains;
//! - counts accept JSON integers or decimal strings ([`Positive`]);
//! - unknown members are ignored and strings borrow from the response body
//!   unless the wire text carries escapes;
//! - listings are split into raw entries first ([`Rows`]), so one malformed
//!   entry is decoded (and rejected or skipped) on its own.

use std::{borrow::Cow, fmt, marker::PhantomData};

use serde::{
	Deserialize, Deserializer, Serialize,
	de::{IgnoredAny, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer},
};
use serde_json::value::RawValue;

/// One JSON member as the decoders observe it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Field<T> {
	/// The key is missing.
	#[default]
	Absent,
	/// The key is present with a `null` value.
	Null,
	/// The key is present with a value of an unexpected JSON shape.
	Mismatched,
	/// The key is present with a value of the expected shape.
	Value(T),
}

impl<T> Field<T> {
	/// Reports whether the key is present at all, including `null`.
	pub(super) const fn is_present(&self) -> bool {
		!matches!(self, Self::Absent)
	}

	/// Returns the value when it has the expected shape.
	pub(super) const fn value(&self) -> Option<&T> {
		match self {
			Self::Value(value) => Some(value),
			Self::Absent | Self::Null | Self::Mismatched => None,
		}
	}

	/// Returns the owned value when it has the expected shape.
	pub(super) fn into_value(self) -> Option<T> {
		match self {
			Self::Value(value) => Some(value),
			Self::Absent | Self::Null | Self::Mismatched => None,
		}
	}

	/// Returns this member when it is present with a non-`null` value.
	pub(super) const fn non_null(&self) -> Option<&Self> {
		match self {
			Self::Absent | Self::Null => None,
			Self::Mismatched | Self::Value(_) => Some(self),
		}
	}
}

impl<T> From<Option<T>> for Field<T> {
	fn from(value: Option<T>) -> Self {
		value.map_or(Self::Mismatched, Self::Value)
	}
}

/// Returns the value of the first present key; a present key with an
/// unexpected shape ends the chain without a value.
pub(super) fn first_present<'f, T: 'f>(
	fields: impl IntoIterator<Item = &'f Field<T>>,
) -> Option<&'f T> {
	fields
		.into_iter()
		.find(|field| field.is_present())
		.and_then(Field::value)
}

/// Returns the first positive count among `fields`, skipping keys whose
/// values are missing, zero, negative, fractional, or not numeric text.
pub(super) fn positive<'f>(fields: impl IntoIterator<Item = &'f Field<Positive>>) -> Option<u64> {
	fields
		.into_iter()
		.find_map(|field| field.value().map(|value| value.0))
}

/// Parses decimal text into a strictly positive count.
pub(super) fn positive_u64_text(value: &str) -> Option<u64> {
	value.trim().parse::<u64>().ok().filter(|value| *value > 0)
}

/// A value decodable from any JSON shape, yielding `None` for shapes it does
/// not accept. Rejected containers are drained, never failed.
pub(super) trait Shape<'de>: Sized {
	/// Accepts a JSON boolean.
	fn from_bool(_: bool) -> Option<Self> {
		None
	}

	/// Accepts a non-negative JSON integer.
	fn from_u64(_: u64) -> Option<Self> {
		None
	}

	/// Accepts a negative JSON integer.
	fn from_i64(_: i64) -> Option<Self> {
		None
	}

	/// Accepts a fractional or out-of-range JSON number.
	fn from_f64(_: f64) -> Option<Self> {
		None
	}

	/// Accepts transient (unescaped) JSON string text.
	fn from_str(_: &str) -> Option<Self> {
		None
	}

	/// Accepts JSON string text borrowed from the response body.
	fn from_borrowed_str(value: &'de str) -> Option<Self> {
		Self::from_str(value)
	}

	/// Accepts a JSON array.
	fn from_seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Option<Self>, A::Error> {
		while seq.next_element::<IgnoredAny>()?.is_some() {}
		Ok(None)
	}

	/// Accepts a JSON object.
	fn from_map<A: MapAccess<'de>>(mut map: A) -> Result<Option<Self>, A::Error> {
		while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
		Ok(None)
	}
}

struct FieldVisitor<T>(PhantomData<T>);

impl<'de, T: Shape<'de>> Visitor<'de> for FieldVisitor<T> {
	type Value = Field<T>;

	fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("any JSON value")
	}

	fn visit_bool<E>(self, value: bool) -> Result<Field<T>, E> {
		Ok(T::from_bool(value).into())
	}

	fn visit_u64<E>(self, value: u64) -> Result<Field<T>, E> {
		Ok(T::from_u64(value).into())
	}

	fn visit_i64<E>(self, value: i64) -> Result<Field<T>, E> {
		Ok(u64::try_from(value)
			.map_or_else(|_| T::from_i64(value), T::from_u64)
			.into())
	}

	fn visit_f64<E>(self, value: f64) -> Result<Field<T>, E> {
		Ok(T::from_f64(value).into())
	}

	fn visit_str<E>(self, value: &str) -> Result<Field<T>, E> {
		Ok(T::from_str(value).into())
	}

	fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Field<T>, E> {
		Ok(T::from_borrowed_str(value).into())
	}

	fn visit_string<E>(self, value: String) -> Result<Field<T>, E> {
		Ok(T::from_str(&value).into())
	}

	fn visit_unit<E>(self) -> Result<Field<T>, E> {
		Ok(Field::Null)
	}

	fn visit_none<E>(self) -> Result<Field<T>, E> {
		Ok(Field::Null)
	}

	fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Field<T>, D::Error> {
		Field::deserialize(deserializer)
	}

	fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Field<T>, A::Error> {
		T::from_seq(seq).map(Field::from)
	}

	fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Field<T>, A::Error> {
		T::from_map(map).map(Field::from)
	}
}

impl<'de, T: Shape<'de>> Deserialize<'de> for Field<T> {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_any(FieldVisitor(PhantomData))
	}
}

impl<'de: 'a, 'a> Shape<'de> for Cow<'a, str> {
	fn from_str(value: &str) -> Option<Self> {
		Some(Cow::Owned(value.to_owned()))
	}

	fn from_borrowed_str(value: &'de str) -> Option<Self> {
		Some(Cow::Borrowed(value))
	}
}

impl Shape<'_> for bool {
	fn from_bool(value: bool) -> Option<Self> {
		Some(value)
	}
}

/// Signed integer, from a JSON integer or decimal text.
impl Shape<'_> for i64 {
	fn from_u64(value: u64) -> Option<Self> {
		Self::try_from(value).ok()
	}

	fn from_i64(value: i64) -> Option<Self> {
		Some(value)
	}

	fn from_str(value: &str) -> Option<Self> {
		value.trim().parse().ok()
	}
}

/// Per-token price, from any JSON number or decimal text.
impl Shape<'_> for f64 {
	fn from_u64(value: u64) -> Option<Self> {
		Some(value as Self)
	}

	fn from_i64(value: i64) -> Option<Self> {
		Some(value as Self)
	}

	fn from_f64(value: f64) -> Option<Self> {
		Some(value)
	}

	fn from_str(value: &str) -> Option<Self> {
		value.trim().parse().ok()
	}
}

/// Strictly positive count from a JSON integer or decimal text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Positive(pub(super) u64);

impl Shape<'_> for Positive {
	fn from_u64(value: u64) -> Option<Self> {
		(value > 0).then_some(Self(value))
	}

	fn from_str(value: &str) -> Option<Self> {
		positive_u64_text(value).map(Self)
	}
}

/// Array whose elements keep their positions and individual shapes.
impl<'de, T: Shape<'de>> Shape<'de> for Vec<Field<T>> {
	fn from_seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Option<Self>, A::Error> {
		let mut values = Self::with_capacity(seq.size_hint().unwrap_or(0));
		while let Some(value) = seq.next_element::<Field<T>>()? {
			values.push(value);
		}
		Ok(Some(values))
	}
}

/// Implements [`Shape`] for derived structs that decode from JSON objects.
macro_rules! object_shape {
	($($ty:ident$(<$lifetime:lifetime>)?),+ $(,)?) => {$(
		impl<'de $(: $lifetime, $lifetime)?> Shape<'de> for $ty$(<$lifetime>)? {
			fn from_map<A: MapAccess<'de>>(map: A) -> Result<Option<Self>, A::Error> {
				Self::deserialize(MapAccessDeserializer::new(map)).map(Some)
			}
		}
	)+};
}

object_shape!(
	ModelEntry<'a>,
	ModelInfo<'a>,
	LiteLlmParams<'a>,
	LlamaMeta,
	LlamaStatus<'a>,
	Architecture,
	CapabilityFlags,
	OllamaShow<'a>,
	LlamaProps,
	GenerationSettings,
	GenerationParams,
);

/// Envelope members that may carry a model list, in precedence order.
#[derive(Clone, Copy, Deserialize)]
#[serde(field_identifier, rename_all = "lowercase")]
enum EnvelopeKey {
	Data,
	Models,
	Result,
	Items,
	#[serde(other)]
	Other,
}

/// Raw model entries of one listing response.
///
/// A listing is a bare array or an object whose `data`, `models`, `result`,
/// or `items` member (first match wins, searched recursively) holds the
/// array. Entries stay raw so each one is decoded independently.
#[derive(Debug)]
pub(super) struct Rows<'a>(pub(super) Vec<&'a RawValue>);

impl<'de: 'a, 'a> Shape<'de> for Rows<'a> {
	fn from_seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Option<Self>, A::Error> {
		let mut rows = Vec::with_capacity(seq.size_hint().unwrap_or(0));
		while let Some(row) = seq.next_element::<&'de RawValue>()? {
			rows.push(row);
		}
		Ok(Some(Self(rows)))
	}

	fn from_map<A: MapAccess<'de>>(mut map: A) -> Result<Option<Self>, A::Error> {
		let mut candidates: [Option<Self>; 4] = [None, None, None, None];
		while let Some(key) = map.next_key::<EnvelopeKey>()? {
			let slot = match key {
				EnvelopeKey::Data => 0,
				EnvelopeKey::Models => 1,
				EnvelopeKey::Result => 2,
				EnvelopeKey::Items => 3,
				EnvelopeKey::Other => {
					map.next_value::<IgnoredAny>()?;
					continue;
				},
			};
			// A repeated key replaces the earlier value, as JSON objects do.
			candidates[slot] = map.next_value::<Field<Self>>()?.into_value();
		}
		Ok(candidates.into_iter().flatten().next())
	}
}

/// Capability declaration, either a name list (`["vision", "thinking"]`) or
/// a flag object (`{"vision": true}`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Capabilities {
	/// Image input is declared.
	pub(super) vision:    bool,
	/// Visible reasoning is declared.
	pub(super) reasoning: bool,
}

impl<'de> Shape<'de> for Capabilities {
	fn from_seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Option<Self>, A::Error> {
		let mut capabilities = Self { vision: false, reasoning: false };
		while let Some(name) = seq.next_element::<Field<Cow<'de, str>>>()? {
			let Some(name) = name.value() else {
				continue;
			};
			capabilities.vision |=
				name.eq_ignore_ascii_case("image") || name.eq_ignore_ascii_case("vision");
			capabilities.reasoning |=
				name.eq_ignore_ascii_case("thinking") || name.eq_ignore_ascii_case("reasoning");
		}
		Ok(Some(capabilities))
	}

	fn from_map<A: MapAccess<'de>>(map: A) -> Result<Option<Self>, A::Error> {
		let flags = CapabilityFlags::deserialize(MapAccessDeserializer::new(map))?;
		Ok(Some(Self {
			vision:    first_present([&flags.image, &flags.vision]) == Some(&true),
			reasoning: first_present([&flags.thinking, &flags.reasoning]) == Some(&true),
		}))
	}
}

/// Capability flag object; the first present synonym decides each flag.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct CapabilityFlags {
	image:     Field<bool>,
	vision:    Field<bool>,
	thinking:  Field<bool>,
	reasoning: Field<bool>,
}

/// Input-modality name list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Modalities {
	/// `image` or `vision` is listed.
	pub(super) image: bool,
}

impl<'de> Shape<'de> for Modalities {
	fn from_seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Option<Self>, A::Error> {
		let mut image = false;
		while let Some(name) = seq.next_element::<Field<Cow<'de, str>>>()? {
			image |= name.value().is_some_and(|name| {
				name.eq_ignore_ascii_case("image") || name.eq_ignore_ascii_case("vision")
			});
		}
		Ok(Some(Self { image }))
	}
}

/// OpenRouter-style architecture object.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Architecture {
	/// Accepted input modalities.
	pub(super) input_modalities: Field<Modalities>,
}

/// Capability evidence shared by listing entries and metadata responses.
#[derive(Clone, Copy, Debug)]
pub(super) struct CapabilityEvidence {
	/// `capabilities` name list or flag object.
	pub(super) capabilities:       Field<Capabilities>,
	/// `input` modality list.
	pub(super) input:              Field<Modalities>,
	/// `input_modalities` list.
	pub(super) input_modalities:   Field<Modalities>,
	/// `architecture.input_modalities` list.
	pub(super) architecture_input: Field<Modalities>,
	/// `supports_vision` flag.
	pub(super) supports_vision:    Field<bool>,
	/// `supports_reasoning` flag.
	pub(super) supports_reasoning: Field<bool>,
}

/// Declares the capability-evidence members on a wire struct.
macro_rules! capability_evidence {
	($ty:ident$(<$lifetime:lifetime>)?) => {
		impl$(<$lifetime>)? $ty$(<$lifetime>)? {
			/// Capability evidence declared by this object.
			pub(super) fn evidence(&self) -> CapabilityEvidence {
				CapabilityEvidence {
					capabilities:       self.capabilities,
					input:              self.input,
					input_modalities:   self.input_modalities,
					architecture_input: self
						.architecture
						.value()
						.map_or(Field::Absent, |architecture| architecture.input_modalities),
					supports_vision:    self.supports_vision,
					supports_reasoning: self.supports_reasoning,
				}
			}
		}
	};
}

capability_evidence!(ModelEntry<'a>);
capability_evidence!(OllamaShow<'a>);
capability_evidence!(LlamaProps);

/// One model entry of any supported listing endpoint.
///
/// The same entry shape serves every endpoint family because the decoder
/// accepts each family's documented members wherever they appear:
///
/// - OpenAI-compatible `/v1/models` (vLLM, `OpenRouter`-style proxies): `id`,
///   `name`, `context_length`, `max_model_len`, `architecture`,
///   `supported_endpoint_types`;
/// - Ollama `/api/tags`: `model`, `name`;
/// - llama.cpp `/models`: `id`, `meta`, router-mode `status`;
/// - LM Studio `/api/v0/models`: `id`, `state`, `max_context_length`,
///   `loaded_context_length`, `capabilities`;
/// - `LiteLLM` `/model_group/info` and `/model/info`: `model_group`,
///   `model_name`, `providers`, `litellm_params`, `model_info`, per-token
///   costs, `supports_*` flags.
#[derive(Debug, Default, Deserialize)]
#[serde(default, bound(deserialize = "'de: 'a"))]
pub(super) struct ModelEntry<'a> {
	/// OpenAI-compatible model id.
	pub(super) id: Field<Cow<'a, str>>,
	/// Display or fallback model name.
	pub(super) name: Field<Cow<'a, str>>,
	/// Ollama wire model name.
	pub(super) model: Field<Cow<'a, str>>,
	/// `LiteLLM` public model group.
	pub(super) model_group: Field<Cow<'a, str>>,
	/// `LiteLLM` deployment model name.
	pub(super) model_name: Field<Cow<'a, str>>,
	/// Human-readable name.
	pub(super) display_name: Field<Cow<'a, str>>,
	/// Human-readable name (camel-case spelling).
	#[serde(rename = "displayName")]
	pub(super) display_name_camel: Field<Cow<'a, str>>,
	/// LM Studio load state (`loaded`, `not-loaded`).
	pub(super) state: Field<Cow<'a, str>>,
	/// Served or trained context window.
	pub(super) context_length: Field<Positive>,
	/// Context window (camel-case spelling).
	#[serde(rename = "contextWindow")]
	pub(super) context_window: Field<Positive>,
	/// LM Studio maximum context window.
	pub(super) max_context_length: Field<Positive>,
	/// LM Studio context window of the loaded instance.
	pub(super) loaded_context_length: Field<Positive>,
	/// vLLM maximum model length.
	pub(super) max_model_len: Field<Positive>,
	/// `LiteLLM` maximum input tokens.
	pub(super) max_input_tokens: Field<Positive>,
	/// Maximum output tokens.
	pub(super) max_output_tokens: Field<Positive>,
	/// Maximum output tokens (camel-case spelling).
	#[serde(rename = "maxTokens")]
	pub(super) max_tokens: Field<Positive>,
	/// `LiteLLM` nested model metadata.
	pub(super) model_info: Field<ModelInfo<'a>>,
	/// `LiteLLM` deployment parameters.
	pub(super) litellm_params: Field<LiteLlmParams<'a>>,
	/// `LiteLLM` provider names serving a model group.
	pub(super) providers: Field<ProviderNames>,
	/// `LiteLLM` provider-qualified base model.
	pub(super) base_model: Field<Cow<'a, str>>,
	/// Input price per token.
	pub(super) input_cost_per_token: Field<f64>,
	/// Output price per token.
	pub(super) output_cost_per_token: Field<f64>,
	/// Cache-read price per token.
	pub(super) cache_read_input_token_cost: Field<f64>,
	/// Cache-write price per token.
	pub(super) cache_creation_input_token_cost: Field<f64>,
	/// llama.cpp model metadata.
	pub(super) meta: Field<LlamaMeta>,
	/// llama.cpp router-mode instance status.
	pub(super) status: Field<LlamaStatus<'a>>,
	/// Proxy wire protocols serving the model.
	pub(super) supported_endpoint_types: Field<EndpointTypes>,
	/// Capability names or flags.
	pub(super) capabilities: Field<Capabilities>,
	/// Input modalities.
	pub(super) input: Field<Modalities>,
	/// Input modalities.
	pub(super) input_modalities: Field<Modalities>,
	/// Architecture with input modalities.
	pub(super) architecture: Field<Architecture>,
	/// Vision support flag.
	pub(super) supports_vision: Field<bool>,
	/// Reasoning support flag.
	pub(super) supports_reasoning: Field<bool>,
}

impl<'a> ModelEntry<'a> {
	/// Decodes one raw listing entry. `Ok(None)` means the entry is not a
	/// JSON object.
	pub(super) fn decode(raw: &'a RawValue) -> Result<Option<Self>, serde_json::Error> {
		serde_json::from_str::<Field<Self>>(raw.get()).map(Field::into_value)
	}
}

/// `LiteLLM` `model_info` object.
#[derive(Debug, Default, Deserialize)]
#[serde(default, bound(deserialize = "'de: 'a"))]
pub(super) struct ModelInfo<'a> {
	/// Maximum input tokens.
	pub(super) max_input_tokens: Field<Positive>,
	/// Maximum output tokens.
	pub(super) max_output_tokens: Field<Positive>,
	/// Provider-qualified base model.
	pub(super) base_model: Field<Cow<'a, str>>,
	/// Input price per token.
	pub(super) input_cost_per_token: Field<f64>,
	/// Output price per token.
	pub(super) output_cost_per_token: Field<f64>,
	/// Cache-read price per token.
	pub(super) cache_read_input_token_cost: Field<f64>,
	/// Cache-write price per token.
	pub(super) cache_creation_input_token_cost: Field<f64>,
}

/// `LiteLLM` `litellm_params` object.
#[derive(Debug, Default, Deserialize)]
#[serde(default, bound(deserialize = "'de: 'a"))]
pub(super) struct LiteLlmParams<'a> {
	/// Provider-qualified deployment model (`openai/gpt-4o`).
	pub(super) model:               Field<Cow<'a, str>>,
	/// Explicit provider override.
	pub(super) custom_llm_provider: Field<Cow<'a, str>>,
}

/// `LiteLLM` `providers` name list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProviderNames {
	/// At least one non-blank provider name is listed.
	pub(super) any:        bool,
	/// Every non-blank provider name is `openai`.
	pub(super) all_openai: bool,
}

impl<'de> Shape<'de> for ProviderNames {
	fn from_seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Option<Self>, A::Error> {
		let mut names = Self { any: false, all_openai: true };
		while let Some(name) = seq.next_element::<Field<Cow<'de, str>>>()? {
			let Some(name) = name
				.value()
				.map(|name| name.trim())
				.filter(|name| !name.is_empty())
			else {
				continue;
			};
			names.any = true;
			names.all_openai &= name.eq_ignore_ascii_case("openai");
		}
		Ok(Some(names))
	}
}

/// Proxy `supported_endpoint_types` list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EndpointTypes {
	/// Anthropic Messages is listed.
	pub(super) anthropic: bool,
	/// `OpenAI` chat is listed.
	pub(super) openai:    bool,
}

impl<'de> Shape<'de> for EndpointTypes {
	fn from_seq<A: SeqAccess<'de>>(mut seq: A) -> Result<Option<Self>, A::Error> {
		let mut types = Self { anthropic: false, openai: false };
		while let Some(name) = seq.next_element::<Field<Cow<'de, str>>>()? {
			let Some(name) = name.value() else {
				continue;
			};
			types.anthropic |= name.eq_ignore_ascii_case("anthropic");
			types.openai |= name.eq_ignore_ascii_case("openai");
		}
		Ok(Some(types))
	}
}

/// llama.cpp `meta` object.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct LlamaMeta {
	/// Runtime context window.
	pub(super) n_ctx:       Field<Positive>,
	/// Training context window.
	pub(super) n_ctx_train: Field<Positive>,
}

/// llama.cpp router-mode `status` object.
#[derive(Debug, Default, Deserialize)]
#[serde(default, bound(deserialize = "'de: 'a"))]
pub(super) struct LlamaStatus<'a> {
	/// Instance command-line arguments.
	pub(super) args:   Field<Vec<Field<LlamaArg<'a>>>>,
	/// INI-style preset text.
	pub(super) preset: Field<Cow<'a, str>>,
}

/// One llama.cpp router command-line argument.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum LlamaArg<'a> {
	/// String argument.
	Text(Cow<'a, str>),
	/// Non-negative integer argument.
	Count(u64),
}

impl LlamaArg<'_> {
	/// Interprets the argument as a positive count.
	pub(super) fn positive(&self) -> Option<u64> {
		match self {
			Self::Text(text) => positive_u64_text(text),
			Self::Count(count) => (*count > 0).then_some(*count),
		}
	}
}

impl<'de: 'a, 'a> Shape<'de> for LlamaArg<'a> {
	fn from_u64(value: u64) -> Option<Self> {
		Some(Self::Count(value))
	}

	fn from_str(value: &str) -> Option<Self> {
		Cow::from_str(value).map(Self::Text)
	}

	fn from_borrowed_str(value: &'de str) -> Option<Self> {
		Cow::from_borrowed_str(value).map(Self::Text)
	}
}

/// Ollama `POST /api/show` request.
#[derive(Debug, Serialize)]
pub(super) struct OllamaShowRequest<'a> {
	/// Wire model name.
	pub(super) model: &'a str,
}

/// Ollama `POST /api/show` response.
#[derive(Debug, Default, Deserialize)]
#[serde(default, bound(deserialize = "'de: 'a"))]
pub(super) struct OllamaShow<'a> {
	/// Modelfile `PARAMETER` lines (`num_ctx 8192`).
	pub(super) parameters:         Field<Cow<'a, str>>,
	/// GGUF metadata keyed by `<architecture>.<key>`.
	pub(super) model_info:         Field<OllamaModelInfo>,
	/// Context window.
	pub(super) context_length:     Field<Positive>,
	/// Capability names (`completion`, `vision`, `thinking`, `tools`).
	pub(super) capabilities:       Field<Capabilities>,
	/// Input modalities.
	pub(super) input:              Field<Modalities>,
	/// Input modalities.
	pub(super) input_modalities:   Field<Modalities>,
	/// Architecture with input modalities.
	pub(super) architecture:       Field<Architecture>,
	/// Vision support flag.
	pub(super) supports_vision:    Field<bool>,
	/// Reasoning support flag.
	pub(super) supports_reasoning: Field<bool>,
}

impl<'a> OllamaShow<'a> {
	/// Decodes a show response; a non-object body carries no metadata.
	pub(super) fn decode(payload: &'a [u8]) -> Result<Self, serde_json::Error> {
		serde_json::from_slice::<Field<Self>>(payload)
			.map(|show| show.into_value().unwrap_or_default())
	}
}

/// Context window from Ollama's `model_info`: the first key named
/// `context_length` or ending in `.context_length` decides.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct OllamaModelInfo {
	/// Positive context window of the deciding key.
	pub(super) context_length: Option<u64>,
}

impl<'de> Shape<'de> for OllamaModelInfo {
	fn from_map<A: MapAccess<'de>>(mut map: A) -> Result<Option<Self>, A::Error> {
		let mut deciding: Option<(Cow<'de, str>, Option<u64>)> = None;
		while let Some(key) = map.next_key::<Field<Cow<'de, str>>>()? {
			let key = key.into_value().unwrap_or_default();
			let decides = match &deciding {
				// A repeated key keeps its first position and its last value.
				Some((deciding, _)) => *deciding == key,
				None => key == "context_length" || key.ends_with(".context_length"),
			};
			if decides {
				let value = map.next_value::<Field<Positive>>()?;
				deciding = Some((key, value.value().map(|value| value.0)));
			} else {
				map.next_value::<IgnoredAny>()?;
			}
		}
		Ok(Some(Self { context_length: deciding.and_then(|(_, value)| value) }))
	}
}

/// llama.cpp `GET /props` response.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct LlamaProps {
	/// Per-slot defaults.
	pub(super) default_generation_settings: Field<GenerationSettings>,
	/// Server context window.
	pub(super) n_ctx: Field<Positive>,
	/// Training context window.
	pub(super) n_ctx_train: Field<Positive>,
	/// Context window.
	pub(super) context_length: Field<Positive>,
	/// Output limit; `-1` means unlimited.
	pub(super) max_tokens: Field<i64>,
	/// Output limit; `-1` means unlimited.
	pub(super) n_predict: Field<i64>,
	/// Capability names or flags.
	pub(super) capabilities: Field<Capabilities>,
	/// Input modalities.
	pub(super) input: Field<Modalities>,
	/// Input modalities.
	pub(super) input_modalities: Field<Modalities>,
	/// Architecture with input modalities.
	pub(super) architecture: Field<Architecture>,
	/// Vision support flag.
	pub(super) supports_vision: Field<bool>,
	/// Reasoning support flag.
	pub(super) supports_reasoning: Field<bool>,
}

impl LlamaProps {
	/// Decodes a props response; a non-object body carries no metadata.
	pub(super) fn decode(payload: &[u8]) -> Result<Self, serde_json::Error> {
		serde_json::from_slice::<Field<Self>>(payload)
			.map(|props| props.into_value().unwrap_or_default())
	}
}

/// llama.cpp `default_generation_settings` object.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct GenerationSettings {
	/// Slot context window.
	pub(super) n_ctx:  Field<Positive>,
	/// Sampling parameters.
	pub(super) params: Field<GenerationParams>,
}

/// llama.cpp slot sampling parameters.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct GenerationParams {
	/// Output limit; `-1` means unlimited.
	pub(super) max_tokens: Field<i64>,
	/// Output limit; `-1` means unlimited.
	pub(super) n_predict:  Field<i64>,
}
