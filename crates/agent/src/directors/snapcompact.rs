//! Snapcompact producer: renders the history a compaction hides into
//! provider-shaped PNG frames that the session CAS retains and the prompt
//! projection inlines as images after the compaction note.
//!
//! The [`CompactionDirector`](super::compaction::CompactionDirector) owns
//! when this runs and how the result is journaled. This module owns three
//! pure steps: the route capability guard, serializing the hidden messages
//! into renderer-ready archive text, and admitting the rendered archive
//! against the durable frame bounds. Every refusal is a typed [`Fallback`]
//! whose static text is the notice the director journals before it falls
//! back to the soft summary path.

use std::fmt::Write as _;

use omp_ai::{ContentPart, Message, ToolResultContent};
use omp_dom::Dom;
use omp_journal::data::{MAX_SNAPCOMPACT_FRAME_BYTES, MAX_SNAPCOMPACT_FRAMES};
use omp_snapcompact::archive::{
	self, Archive, ArchiveError, DIM_OFF, DIM_ON, LINE_BREAK, ShapeTarget, push_normalized,
};

use crate::{
	VisionMode,
	director::{RouteFacts, RouteIdentity},
	vision,
};

/// Compaction note preceding the retained frames in the synthetic summary
/// message. Deterministic, so replay and chained compactions reproduce it.
pub(crate) const PREAMBLE: &str = include_str!("../../prompts/compaction/snapcompact-archive.md");

/// Why a snapcompact request ran the soft summary instead. The static label
/// is the user-visible notice text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::IntoStaticStr)]
pub enum Fallback {
	/// The route's catalog input modalities exclude images.
	#[strum(serialize = "Snapcompact needs a model that accepts image input; compacted with a \
	                     soft summary instead.")]
	NoImageInput,
	/// `ai_vision off` withholds every image from the model.
	#[strum(serialize = "Snapcompact is unavailable while ai_vision is off; compacted with a soft \
	                     summary instead.")]
	VisionOff,
	/// The history needs more frames (or frame bytes) than the provider and
	/// the durable format admit.
	#[strum(serialize = "The history exceeds the snapcompact frame budget for this model; \
	                     compacted with a soft summary instead.")]
	FrameBudget,
	/// Imaging would not save the required token margin.
	#[strum(serialize = "Snapcompact frames would not save enough tokens here; compacted with a \
	                     soft summary instead.")]
	NoSavings,
	/// The bitmap renderer rejected a frame.
	#[strum(serialize = "Snapcompact could not render the history; compacted with a soft summary \
	                     instead.")]
	Renderer,
}

impl Fallback {
	/// The notice text journaled for this fallback.
	#[must_use]
	pub fn notice(self) -> &'static str {
		self.into()
	}
}

impl From<&ArchiveError> for Fallback {
	fn from(error: &ArchiveError) -> Self {
		match error {
			ArchiveError::Renderer(_) => Self::Renderer,
			ArchiveError::NoFrameBudget
			| ArchiveError::FrameBudgetExceeded { .. }
			| ArchiveError::DataBudgetExceeded { .. } => Self::FrameBudget,
			ArchiveError::InsufficientSavings { .. } => Self::NoSavings,
		}
	}
}

/// The capability guard: frames reach the model only when the route accepts
/// image input and the journaled `ai_vision` policy lets images flow.
///
/// # Errors
/// The [`Fallback`] naming the missing capability.
pub fn capability(dom: &Dom, route: &RouteFacts) -> Result<(), Fallback> {
	if !route.image_input {
		return Err(Fallback::NoImageInput);
	}
	match vision::mode(dom) {
		VisionMode::Off => Err(Fallback::VisionOff),
		VisionMode::Auto | VisionMode::On => Ok(()),
	}
}

/// Serializes projected messages into renderer-ready archive text.
///
/// Oldest first, each message opens with its `[role]` header after a
/// [`LINE_BREAK`]; tool calls print `[call NAME]` with compact JSON
/// arguments; tool output sits in a dim span. Reasoning is not archived
/// (providers never replay it as history), and media reads as `[image]`,
/// `[audio]`, or `[document]`.
#[must_use]
pub fn archive_text(messages: &[Message]) -> String {
	let mut out = String::with_capacity(
		messages
			.iter()
			.map(|message| message.content.len().saturating_mul(64))
			.fold(0_usize, usize::saturating_add),
	);
	for message in messages {
		if !out.is_empty() {
			out.push(LINE_BREAK);
		}
		out.push('[');
		out.push_str(<&str>::from(message.role));
		out.push(']');
		for part in message.content.iter() {
			push_part(&mut out, part);
		}
	}
	out
}

fn push_part(out: &mut String, part: &ContentPart) {
	match part {
		ContentPart::Text { text, .. } => {
			out.push(' ');
			push_normalized(out, text);
		},
		ContentPart::Reasoning { .. } | ContentPart::CachePoint(_) => {},
		ContentPart::Image(_) => out.push_str(" [image]"),
		ContentPart::Audio(_) => out.push_str(" [audio]"),
		ContentPart::Document(_) => out.push_str(" [document]"),
		ContentPart::ToolCall { name, arguments, .. } => {
			out.push_str(" [call ");
			push_normalized(out, name);
			// Compact JSON escapes every control character, so it is already
			// renderer-ready.
			let _ = write!(out, "] {}", arguments.as_value());
		},
		ContentPart::ToolResult { name, content, is_error, .. } => {
			out.push(' ');
			out.push(DIM_ON);
			out.push_str(if *is_error { "[error" } else { "[result" });
			if let Some(name) = name {
				out.push(' ');
				push_normalized(out, name);
			}
			out.push(']');
			for item in content.iter() {
				match item {
					ToolResultContent::Text(text) => {
						out.push(' ');
						push_normalized(out, text);
					},
					ToolResultContent::Json(value) => {
						let _ = write!(out, " {}", value.as_value());
					},
					ToolResultContent::Image(_) => out.push_str(" [image]"),
					ToolResultContent::Document(_) => out.push_str(" [document]"),
				}
			}
			out.push(DIM_OFF);
		},
	}
}

/// Images one message already sends: frames compete with them for the
/// provider's per-request image budget.
#[must_use]
pub fn image_count(message: &Message) -> usize {
	message
		.content
		.iter()
		.map(|part| match part {
			ContentPart::Image(_) => 1,
			ContentPart::ToolResult { content, .. } => content
				.iter()
				.filter(|item| matches!(item, ToolResultContent::Image(_)))
				.count(),
			_ => 0,
		})
		.fold(0_usize, usize::saturating_add)
}

/// Renders `text` into frames shaped for the route and admits the archive
/// against the durable bounds together with the `carried` frames an earlier
/// snapcompact already retained.
///
/// `existing_images` counts every image the rebuilt request sends besides
/// the new frames (carried frames included), so the provider image budget
/// holds for the whole request.
///
/// # Errors
/// The [`Fallback`] naming why the archive is not admissible.
pub fn render(
	text: &str,
	source_tokens: u64,
	identity: Option<&RouteIdentity>,
	existing_images: usize,
	carried: (usize, u64),
) -> Result<Archive, Fallback> {
	let target = ShapeTarget {
		api:      identity.map(|identity| identity.codec.as_str()),
		model_id: identity.map(|identity| identity.model.as_str()),
	};
	let provider = identity.map(|identity| identity.provider.as_str());
	let archive = archive::render_archive(text, source_tokens, target, provider, existing_images)
		.map_err(|error| {
			tracing::info!(%error, "snapcompact archive refused; falling back to a soft summary");
			Fallback::from(&error)
		})?;
	let (carried_frames, carried_bytes) = carried;
	let frames = carried_frames.saturating_add(archive.frames.len());
	let bytes =
		carried_bytes.saturating_add(u64::try_from(archive.savings.png_bytes).unwrap_or(u64::MAX));
	if frames > MAX_SNAPCOMPACT_FRAMES || bytes > MAX_SNAPCOMPACT_FRAME_BYTES {
		tracing::info!(
			frames,
			bytes,
			"snapcompact archive exceeds the durable frame bounds; falling back to a soft summary"
		);
		return Err(Fallback::FrameBudget);
	}
	Ok(archive)
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use omp_ai::{OpaqueJson, Role, ToolCallId};
	use omp_core::Str;

	use super::*;

	fn message(role: Role, content: Vec<ContentPart>) -> Message {
		Message { role, content: Arc::from(content), name: None }
	}

	fn text(text: &str) -> ContentPart {
		ContentPart::Text { text: Str::new(text), proof: None }
	}

	#[test]
	fn archive_text_heads_messages_and_dims_tool_output() {
		let call = ToolCallId::from("call-1");
		let messages = [
			message(Role::User, vec![text("fix   the\n\nbug")]),
			message(Role::Assistant, vec![
				ContentPart::Reasoning { text: Str::new_static("private"), proof: None },
				text("reading"),
				ContentPart::ToolCall {
					call:      call.clone(),
					name:      Str::new_static("read"),
					arguments: OpaqueJson::new(serde_json::json!({"path": "a\nb.rs"})),
					proof:     None,
				},
			]),
			message(Role::Tool, vec![ContentPart::ToolResult {
				call,
				name: Some(Str::new_static("read")),
				content: Arc::from([ToolResultContent::Text(Str::new_static("  fn main() {}\n"))]),
				is_error: false,
			}]),
		];
		assert_eq!(
			archive_text(&messages),
			"[user] fix the\u{2588}bug\u{2588}[assistant] reading [call read] \
			 {\"path\":\"a\\nb.rs\"}\u{2588}[tool] \u{e}[result read] fn main() {}\u{f}"
		);
	}

	#[test]
	fn capability_requires_route_image_input_and_a_vision_policy_that_sends_images() {
		let dom = Dom::new();
		assert_eq!(capability(&dom, &RouteFacts::default()), Err(Fallback::NoImageInput));
		let vision = RouteFacts { image_input: true, ..RouteFacts::default() };
		assert_eq!(capability(&dom, &vision), Ok(()));
	}

	#[test]
	fn fallback_notices_are_static_and_name_the_soft_path() {
		for fallback in [
			Fallback::NoImageInput,
			Fallback::VisionOff,
			Fallback::FrameBudget,
			Fallback::NoSavings,
			Fallback::Renderer,
		] {
			assert!(
				fallback
					.notice()
					.ends_with("compacted with a soft summary instead.")
			);
		}
	}
}
