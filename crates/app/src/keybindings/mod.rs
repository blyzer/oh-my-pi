//! Command-stream keybindings as the chat actor consumes them.
//!
//! The default bind cfg and the legacy action table live in
//! [`omp_driver::keybindings`]; this module projects an executed context's
//! bind table for the terminal presentation.

pub mod config;

#[cfg(test)]
mod tests {
	use omp_driver::keybindings::{DEFAULT_BINDS, DEFAULT_BINDS_NAME};

	use super::*;

	#[test]
	fn ctrl_shift_d_is_a_literal_debug_binding() {
		let ctx = omp_con::Ctx::new();
		ctx.exec(
			DEFAULT_BINDS,
			omp_con::Source::Config(omp_core::Str::new_static(DEFAULT_BINDS_NAME)),
		)
		.expect("default bindings execute");
		let bindings = config::ConsoleKeybindings::from_ctx(&ctx).expect("bindings project");
		assert_eq!(bindings.command_for("ctrl+shift+d"), Some("debug"));
	}
}
