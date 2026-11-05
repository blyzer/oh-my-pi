//! Opt-in shell-output minimizer.
//!
//! Condenses a shell command's captured stdout/stderr before the model sees
//! it. [`plan`] classifies the command with the shell parser, [`detect`]
//! names the program, and [`engine`] dispatches the capture to a Rust
//! [`filters`] module or to a declarative [`pipeline`] compiled from TOML.
//!
//! The engine is inert unless a [`MinimizerConfig`] explicitly opts in.

pub mod config;
pub mod detect;
pub mod engine;
pub mod filters;
pub mod primitives;

pub mod pipeline;

pub mod plan;

use std::borrow::Cow;

pub use config::{ConfigError, MinimizerConfig};

/// Per-invocation context passed to every filter.
#[derive(Debug, Clone)]
pub struct MinimizerCtx<'a> {
	/// Resolved program name (lowercased, e.g. `"git"`).
	pub program:    &'a str,
	/// Detected subcommand (lowercased, e.g. `"status"`), if any.
	pub subcommand: Option<&'a str>,
	/// Raw command string as the caller supplied it.
	pub command:    &'a str,
	/// Effective configuration.
	pub config:     &'a MinimizerConfig,
}

/// Output produced by a filter.
#[derive(Debug, Clone)]
pub struct MinimizerOutput<'a> {
	/// Rewritten output; borrows the capture when it passes through.
	pub text:          Cow<'a, str>,
	/// Whether the filter modified the input at all.
	pub changed:       bool,
	/// Byte length of the captured buffer before minimization.
	pub input_bytes:   usize,
	/// Byte length of `text` after minimization.
	pub output_bytes:  usize,
	/// Label for the dispatch path that produced this output (e.g. `"git"`,
	/// `"pipeline:gradle"`, or `"passthrough"`). For non-rewrite misses, this
	/// carries the reason label (e.g. `"compound"`, `"piped"`, `"parse-error"`,
	/// `"too-short"`, `"too-large"`, `"disabled"`, `"unknown"`, `"unsupported"`,
	/// `"pipeline-noop"`).
	pub filter:        &'static str,
	/// Original (un-minimized) capture, surfaced only when the filter
	/// actually rewrote the output. The caller is expected to persist it as an
	/// `artifact://` and splice that reference into [`text`](Self::text)
	/// before presenting it to the agent. The minimizer itself does not hold
	/// onto the original past this struct.
	pub original_text: Option<&'a str>,
}

impl<'a> MinimizerOutput<'a> {
	/// Pass-through constructor — the filter emits the original text unchanged.
	pub fn passthrough(text: impl Into<Cow<'a, str>>) -> Self {
		let text = text.into();
		let bytes = text.len();
		Self {
			text,
			changed: false,
			input_bytes: bytes,
			output_bytes: bytes,
			filter: "passthrough",
			original_text: None,
		}
	}

	/// Transformed output. Caller-supplied `input_bytes` lets the savings
	/// metric compare pre- and post-filter sizes.
	#[must_use]
	pub const fn transformed(text: String, input_bytes: usize) -> Self {
		let output_bytes = text.len();
		Self {
			text: Cow::Owned(text),
			changed: true,
			input_bytes,
			output_bytes,
			filter: "",
			original_text: None,
		}
	}

	/// Attach a `filter` label (e.g. `"git"`, `"pipeline:gradle"`) to an
	/// output for telemetry, including non-rewrite miss reasons.
	#[must_use]
	pub const fn labeled(mut self, filter: &'static str) -> Self {
		self.filter = filter;
		self
	}

	/// Record the original capture buffer on this output so the caller can
	/// persist it as a session artifact and surface an `artifact://`
	/// reference in [`text`](Self::text). No-op on passthrough outputs.
	#[must_use]
	pub const fn with_original(mut self, original: &'a str) -> Self {
		if self.changed {
			self.original_text = Some(original);
		}
		self
	}

	/// Replace the transformed text while keeping minimization telemetry
	/// coherent.
	#[must_use]
	pub fn with_text(mut self, text: impl Into<Cow<'a, str>>) -> Self {
		let text = text.into();
		self.output_bytes = text.len();
		self.text = text;
		self
	}

	/// Byte count saved by this filter (0 for passthrough).
	#[must_use]
	pub const fn bytes_saved(&self) -> usize {
		self.input_bytes.saturating_sub(self.output_bytes)
	}
}

/// Apply the configured filter pipeline to a captured buffer.
/// Returns the original text unchanged when minimization is disabled, no
/// filter matches, or a filter panics.
#[must_use]
pub fn apply<'a>(
	command: &str,
	captured: &'a str,
	exit_code: i32,
	config: &MinimizerConfig,
) -> MinimizerOutput<'a> {
	engine::apply(command, captured, exit_code, config)
}
