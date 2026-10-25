//! Error type shared by every edit engine.
//!
//! The `Display` output of each variant is the exact model-facing message the
//! TypeScript implementation threw; models are trained on these strings, so
//! engines must reproduce them byte-for-byte.

use std::path::PathBuf;

/// Failure raised while parsing, locating, staging, or writing an edit.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
	/// The wire payload could not be parsed. `line` is the 1-indexed payload
	/// line when the parser knows it.
	#[error("{message}")]
	Parse {
		/// Message.
		message: String,
		/// Line.
		line:    Option<u32>,
	},
	/// The payload parsed but could not be applied (missing file, overlap,
	/// invalid op, auto-generated target, no-op, …).
	// lintx-allow: error-str-payload model-facing prose trained byte-for-byte; rendered once via
	// `EditError::apply`
	#[error("{0}")]
	Apply(String),
	/// A search anchor could not be located in the target text.
	// lintx-allow: error-str-payload model-facing prose trained byte-for-byte; rendered once via
	// `EditError::matched`
	#[error("{0}")]
	Match(String),
	/// Filesystem failure reading a target.
	#[error("{source}")]
	Io {
		/// Path.
		path:   PathBuf,
		#[source]
		/// Source.
		source: std::io::Error,
	},
	/// The host writer callback failed; carries its message verbatim.
	// lintx-allow: error-str-payload host writer message relayed verbatim to the model
	#[error("{0}")]
	Writer(String),
}

impl EditError {
	/// Parse failure without a line number; renders the cause once at this
	/// model-facing fault boundary (ADR 0035 / PR #411 pattern).
	pub fn parse(cause: impl std::fmt::Display) -> Self {
		Self::Parse { message: cause.to_string(), line: None }
	}

	/// Parse failure at a known payload line.
	pub fn parse_at(message: impl Into<String>, line: u32) -> Self {
		Self::Parse { message: message.into(), line: Some(line) }
	}

	/// Apply failure; renders the cause once at this model-facing fault
	/// boundary (ADR 0035 / PR #411 pattern).
	pub fn apply(cause: impl std::fmt::Display) -> Self {
		Self::Apply(cause.to_string())
	}

	/// Match failure; renders the cause once at this model-facing fault
	/// boundary (ADR 0035 / PR #411 pattern).
	pub fn matched(cause: impl std::fmt::Display) -> Self {
		Self::Match(cause.to_string())
	}

	/// Whether this is `File not found` (ENOENT) for the given path.
	pub fn is_not_found(&self) -> bool {
		matches!(self, Self::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
	}
}

/// Convenience alias used throughout the crate.
pub type EditResult<T> = Result<T, EditError>;

/// Renders a failure once into the serialized, model-facing diagnostic string
/// kept by previews and memoized parse results (ADR 0035 / PR #411 pattern).
pub(crate) fn fault(cause: impl std::fmt::Display) -> String {
	cause.to_string()
}
