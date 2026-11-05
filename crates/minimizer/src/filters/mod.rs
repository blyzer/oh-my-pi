//! Filter dispatch table for built-in minimizer strategies.

use crate::{MinimizerCtx, MinimizerOutput};

pub mod cargo;
pub mod generic;
pub mod git;

/// Whether a built-in Rust filter claims `program` with `subcommand`.
#[must_use]
pub fn supports(program: &str, subcommand: Option<&str>) -> bool {
	match program {
		"git" | "yadm" => git::supports(subcommand),
		"cargo" => cargo::supports(subcommand),
		_ => false,
	}
}

/// Apply the matching built-in filter.
#[must_use]
pub fn filter<'a>(ctx: &MinimizerCtx<'_>, input: &'a str, exit_code: i32) -> MinimizerOutput<'a> {
	match ctx.program {
		"git" | "yadm" => git::filter(ctx, input, exit_code),
		"cargo" => cargo::filter(ctx, input, exit_code),
		_ => generic::filter(ctx, input, exit_code),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::MinimizerConfig;

	fn ctx<'a>(
		program: &'a str,
		subcommand: Option<&'a str>,
		command: &'a str,
		config: &'a MinimizerConfig,
	) -> MinimizerCtx<'a> {
		MinimizerCtx { program, subcommand, command, config }
	}

	#[test]
	fn pi_cli_names_are_not_supported() {
		assert!(!supports("rtk", None));
		assert!(!supports("pi", None));
	}

	#[test]
	fn unclaimed_program_falls_back_to_generic() {
		let config = MinimizerConfig::default();
		let out =
			filter(&ctx("make", None, "make", &config), "\x1b[1mbuild\x1b[0m\nbuild\ndone\n", 0);
		assert!(out.changed);
		assert_eq!(out.text, "build (×2)\ndone\n");
	}
}
