//! Render-once fault boundary for builtin stderr text (ADR 0035 / PR #411
//! pattern): the one place a typed cause becomes part of a builtin's final,
//! bash/uutils-pinned user-facing message. Callers pass the typed error
//! straight through; the wording (`context: cause` or the bare cause) is
//! byte-identical to GNU/uutils output.

use std::fmt::Display;

/// Renders `context: cause` for a builtin's final stderr/usage text.
pub(crate) fn fault_in(context: impl Display, cause: impl Display) -> String {
	format!("{context}: {cause}")
}

/// Renders a bare cause for a builtin's final stderr/usage text.
pub(crate) fn fault(cause: impl Display) -> String {
	cause.to_string()
}
