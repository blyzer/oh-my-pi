//! Filter dispatch table for built-in minimizer strategies.

use crate::{MinimizerCtx, MinimizerOutput};

pub mod cargo;
pub mod cpp;
pub mod dotnet;
pub mod generic;
pub mod go;
pub mod jvm;
pub mod rust_tools;

/// Whether a built-in Rust filter claims `program` with `subcommand`.
#[must_use]
pub fn supports(program: &str, subcommand: Option<&str>) -> bool {
	match program {
		"cargo" => cargo::supports(subcommand),
		"go" | "golangci-lint" => go::supports(program, subcommand),
		"cmake" | "ctest" | "ninja" | "gtest" | "gtest-parallel" => {
			cpp::supports(program, subcommand)
		},
		program if cpp::is_gtest_binary_name(program) => cpp::supports(program, subcommand),
		"dotnet" => dotnet::supports(program, subcommand),
		// JVM build tools: phase is decided inside jvm::filter (never by
		// ctx.subcommand, which mis-reports `mvn clean install` as `clean`), so
		// supports() claims every subcommand. Defensive `.cmd`/`.bat` arms cover
		// the case where normalize_program is bypassed.
		"mvn" | "mvnw" | "mvnw.cmd" | "gradle" | "gradlew" | "gradlew.bat" => {
			jvm::supports(program, subcommand)
		},
		"rustfmt" => rust_tools::supports(program, subcommand),
		_ => false,
	}
}

/// Apply the matching built-in filter.
#[must_use]
pub fn filter<'a>(ctx: &MinimizerCtx<'_>, input: &'a str, exit_code: i32) -> MinimizerOutput<'a> {
	match ctx.program {
		"cargo" => cargo::filter(ctx, input, exit_code),
		"go" | "golangci-lint" => go::filter(ctx, input, exit_code),
		"dotnet" => dotnet::filter(ctx, input, exit_code),
		"mvn" | "mvnw" | "mvnw.cmd" | "gradle" | "gradlew" | "gradlew.bat" => {
			jvm::filter(ctx, input, exit_code)
		},
		"cmake" | "ctest" | "ninja" | "gtest" | "gtest-parallel" => {
			cpp::filter(ctx, input, exit_code)
		},
		program if cpp::is_gtest_binary_name(program) => cpp::filter(ctx, input, exit_code),
		"rustfmt" => rust_tools::filter(ctx, input, exit_code),
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
