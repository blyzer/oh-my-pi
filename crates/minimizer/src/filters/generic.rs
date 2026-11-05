//! Generic fallback transforms.

use xutf::ToAnsiStripped as _;

use crate::{MinimizerCtx, MinimizerOutput, primitives};

/// Strip ANSI escapes, collapse repeated lines, and cap error-sized output.
#[must_use]
pub fn filter<'a>(_ctx: &MinimizerCtx<'_>, input: &'a str, _exit_code: i32) -> MinimizerOutput<'a> {
	let stripped = input.to_ansi_stripped();
	let deduped = primitives::dedup_consecutive_lines(&stripped);
	let text = if deduped.lines().count() > primitives::CapClass::Errors.lines() {
		primitives::head_tail_cap(&deduped, primitives::CapClass::Errors)
	} else {
		deduped
	};
	if text == input {
		MinimizerOutput::passthrough(input)
	} else {
		MinimizerOutput::transformed(text, input.len())
	}
}
