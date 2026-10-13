//! The exact prompt a delegated executor receives.
//!
//! Owner policy (2026-10-13): a delegated CLI runs the prompt OMP would have
//! used for its own agent run — the same rendered system prompt, instructions
//! and user turn, with the same banded slots — so the CLI behaves as OMP's own
//! execution would. The executor adds no paraphrase, summary or rewrite.
//!
//! The contract enforces that instead of trusting it:
//!
//! - [`DelegatedPrompt`] is built only from the already rendered
//!   `omp_proto::thread::v1::Item`s (and band digests) the kernel would send
//!   to a model. Non-text parts are refused, never dropped.
//! - It carries a [`Hash32`] [`digest`](DelegatedPrompt::digest) over role,
//!   length and bytes of every section plus the band digests.
//! - The backend echoes the digest of the bytes it actually forwarded in
//!   [`TurnAccepted`](super::TurnAccepted); the host compares it and ends the
//!   session on any difference ([`ExecutorError::PromptRewritten`]).
//!
//! [`ExecutorError::PromptRewritten`]: super::ExecutorError::PromptRewritten

use std::sync::Arc;

use omp_core::{Hash32, Str};
use omp_proto::thread::v1::{self as thread, Item, item, part};
use thiserror::Error;

use crate::prompt::RenderedPrompt;

/// Who a prompt section speaks as.
#[derive(
	Clone, Copy, Debug, Eq, Hash, PartialEq, strum::Display, strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum PromptRole {
	/// System, developer or instruction text.
	System,
	/// User text, including the synthetic volatile-band messages.
	User,
}

impl PromptRole {
	const fn tag(self) -> u8 {
		match self {
			Self::System => 1,
			Self::User => 2,
		}
	}
}

/// One message of the prompt, text only and unmodified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSection {
	/// Who the text speaks as.
	pub role: PromptRole,
	/// The exact text. The executor forwards these bytes verbatim.
	pub text: Str,
}

/// Why a rendered prompt could not become a [`DelegatedPrompt`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PromptConversionError {
	/// An item is not a message (a tool call or result has no place in a
	/// fresh delegated turn).
	#[error("prompt item {index} is not a message")]
	NotAMessage {
		/// Index into the concatenation of system items, volatile items.
		index: usize,
	},
	/// A message has a role a delegated turn cannot carry.
	#[error("prompt item {index} has an unsupported role")]
	UnsupportedRole {
		/// Index into the concatenation of system items, volatile items.
		index: usize,
	},
	/// A message has a non-text part. Dropping it would change the prompt.
	#[error("prompt item {index} has a non-text part")]
	NonTextPart {
		/// Index into the concatenation of system items, volatile items.
		index: usize,
	},
}

/// The complete prompt of one delegated turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegatedPrompt {
	sections: Arc<[PromptSection]>,
	bands:    [Hash32; 4],
	digest:   Hash32,
}

impl DelegatedPrompt {
	/// Builds the prompt from what the kernel would send a model:
	/// the rendered system items, the volatile-band messages that trail the
	/// durable window, and the user turn.
	///
	/// `bands` are the Frozen, Stable, Dynamic and Volatile digests of the
	/// rendering, in that order.
	///
	/// # Errors
	///
	/// [`PromptConversionError`] when an item cannot be carried as text.
	pub fn from_items(
		system: &[Item],
		volatile: &[Item],
		user_turn: Str,
		bands: [Hash32; 4],
	) -> Result<Self, PromptConversionError> {
		let mut sections = Vec::with_capacity(system.len() + volatile.len() + 1);
		for (index, item) in system.iter().chain(volatile).enumerate() {
			sections.push(section(index, item)?);
		}
		sections.push(PromptSection { role: PromptRole::User, text: user_turn });
		Ok(Self::from_sections(sections, bands))
	}

	/// Builds the prompt from a [`RenderedPrompt`].
	///
	/// `RenderedPrompt::items` already flattens all four bands in order
	/// (Frozen, Stable, Dynamic, Volatile), so no volatile items are passed
	/// separately. A caller that renders the way the kernel does, with
	/// `CanonicalPromptSource::system_items` plus `volatile_items`, uses
	/// [`DelegatedPrompt::from_items`] instead; either way the sections are
	/// exactly what OMP would have sent its own model.
	///
	/// # Errors
	///
	/// [`PromptConversionError`] when an item cannot be carried as text.
	pub fn from_rendered(
		rendered: &RenderedPrompt,
		user_turn: Str,
	) -> Result<Self, PromptConversionError> {
		let bands = rendered.bands.map(|band| Hash32::new(*band.as_bytes()));
		Self::from_items(&rendered.items, &[], user_turn, bands)
	}

	/// Builds the prompt from sections that are already plain text.
	#[must_use]
	pub fn from_sections(sections: impl Into<Arc<[PromptSection]>>, bands: [Hash32; 4]) -> Self {
		let sections = sections.into();
		let digest = digest_of(&sections, &bands);
		Self { sections, bands, digest }
	}

	/// The sections, in the order the executor must deliver them.
	#[must_use]
	pub fn sections(&self) -> &[PromptSection] {
		&self.sections
	}

	/// The Frozen, Stable, Dynamic and Volatile band digests of the rendering.
	#[must_use]
	pub const fn bands(&self) -> &[Hash32; 4] {
		&self.bands
	}

	/// The digest a backend must reproduce from the bytes it forwards.
	#[must_use]
	pub const fn digest(&self) -> Hash32 {
		self.digest
	}

	/// Computes the digest of arbitrary sections and bands, exactly as
	/// [`DelegatedPrompt::digest`] does. Backends call this over the bytes
	/// they are about to forward.
	#[must_use]
	pub fn digest_of(sections: &[PromptSection], bands: &[Hash32; 4]) -> Hash32 {
		digest_of(sections, bands)
	}
}

fn section(index: usize, item: &Item) -> Result<PromptSection, PromptConversionError> {
	let Some(item::Kind::Message(message)) = item.kind.as_ref() else {
		return Err(PromptConversionError::NotAMessage { index });
	};
	let role = if message.role == thread::Role::System as i32 {
		PromptRole::System
	} else if message.role == thread::Role::User as i32 {
		PromptRole::User
	} else {
		return Err(PromptConversionError::UnsupportedRole { index });
	};
	let mut text = String::new();
	for part in &message.parts {
		match part.kind.as_ref() {
			Some(part::Kind::Text(chunk)) => text.push_str(chunk),
			_ => return Err(PromptConversionError::NonTextPart { index }),
		}
	}
	Ok(PromptSection { role, text: Str::from(text) })
}

fn digest_of(sections: &[PromptSection], bands: &[Hash32; 4]) -> Hash32 {
	let mut hasher = Hash32::hasher();
	hasher.update((sections.len() as u64).to_le_bytes());
	for section in sections {
		hasher.update([section.role.tag()]);
		hasher.update((section.text.len() as u64).to_le_bytes());
		hasher.update(section.text.as_str().as_bytes());
	}
	for band in bands {
		hasher.update(band.as_bytes());
	}
	hasher.finalize()
}
