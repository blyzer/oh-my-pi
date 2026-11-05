//! Filter dispatch table for built-in minimizer strategies.

use crate::{MinimizerCtx, MinimizerOutput};

pub mod bun;
pub mod cargo;
pub mod cpp;
pub mod generic;
pub mod js_tools;
pub mod lint;
pub mod node_tests;
pub mod pkg;
pub mod python;
pub mod ruby;

/// Whether a built-in Rust filter claims `program` with `subcommand`.
#[must_use]
pub fn supports(program: &str, subcommand: Option<&str>) -> bool {
	match program {
		"cargo" => cargo::supports(subcommand),
		"cmake" | "ctest" | "ninja" | "gtest" | "gtest-parallel" => {
			cpp::supports(program, subcommand)
		},
		program if cpp::is_gtest_binary_name(program) => cpp::supports(program, subcommand),
		"tsc" | "eslint" | "biome" | "shellcheck" | "markdownlint" | "hadolint" | "yamllint"
		| "oxlint" | "pyright" | "basedpyright" => {
			lint::supports(subcommand) || lint::supports_program(program, subcommand)
		},
		"npm" | "pnpm" | "yarn" | "pip" | "pip3" | "bundle" | "brew" | "composer" | "poetry" => {
			pkg::supports(subcommand)
		},
		"uv" => pkg::supports(subcommand),
		"bun" | "bunx" => bun::supports(program, subcommand),
		"pytest" | "ruff" | "mypy" | "python" | "python3" | "py" => {
			python::supports(program, subcommand)
		},
		"rspec" | "rake" | "rails" | "rubocop" => ruby::supports(program, subcommand),
		"jest" | "vitest" | "playwright" => true,
		"next" | "prettier" | "prisma" => js_tools::supports(program, subcommand),
		_ => false,
	}
}

fn is_test_script_token(token: &str) -> bool {
	let token = token.trim_matches(|ch| matches!(ch, '\'' | '"' | '`'));
	matches!(token, "test" | "t" | "e2e" | "spec") || token.starts_with("test:")
}

/// The script/command word a `run`-style invocation targets: the first
/// non-flag token after the `run`/`-m`/`--module` marker. Returns `None` when
/// no marker (or no following word) is present.
///
/// Selecting only this word — instead of scanning the entire command line —
/// keeps tool/script names that appear merely as later arguments from
/// mis-routing output through a test/lint/wrapped-tool filter. Examples that
/// must NOT route as tests: `npm run build -- test`, `uv run echo pytest`.
fn run_invoked_word(command: &str) -> Option<&str> {
	let mut tokens = command
		.split(|ch: char| ch.is_whitespace() || matches!(ch, ';' | '|' | '&'))
		.filter(|tok| !tok.is_empty());
	tokens
		.by_ref()
		.find(|tok| matches!(*tok, "run" | "-m" | "--module"))?;
	tokens.find(|tok| !tok.starts_with('-'))
}

fn is_pkg_test_invocation(ctx: &MinimizerCtx<'_>) -> bool {
	matches!(ctx.subcommand, Some("test" | "t"))
		|| (matches!(ctx.subcommand, Some("run"))
			&& run_invoked_word(ctx.command).is_some_and(is_test_script_token))
}

fn is_pkg_lint_invocation(ctx: &MinimizerCtx<'_>) -> bool {
	matches!(ctx.subcommand, Some("run"))
		&& run_invoked_word(ctx.command).is_some_and(|word| {
			is_lint_script_token(word) || matches!(word, "tsc" | "eslint" | "biome")
		})
}

fn is_lint_script_token(token: &str) -> bool {
	let token = token.trim_matches(|ch| matches!(ch, '\'' | '"' | '`'));
	matches!(token, "lint" | "typecheck" | "type-check")
		|| token.starts_with("lint:")
		|| token.starts_with("typecheck:")
		|| token.starts_with("type-check:")
}

/// Apply the matching built-in filter.
#[must_use]
pub fn filter<'a>(ctx: &MinimizerCtx<'_>, input: &'a str, exit_code: i32) -> MinimizerOutput<'a> {
	match ctx.program {
		"cargo" => cargo::filter(ctx, input, exit_code),
		"cmake" | "ctest" | "ninja" | "gtest" | "gtest-parallel" => {
			cpp::filter(ctx, input, exit_code)
		},
		program if cpp::is_gtest_binary_name(program) => cpp::filter(ctx, input, exit_code),
		"tsc" | "eslint" | "biome" | "shellcheck" | "markdownlint" | "hadolint" | "yamllint"
		| "oxlint" | "pyright" | "basedpyright" => lint::filter(ctx, input, exit_code),
		"npm" | "pnpm" | "yarn" => {
			if is_pkg_test_invocation(ctx) {
				node_tests::filter(ctx, input, exit_code)
			} else if is_pkg_lint_invocation(ctx) {
				lint::filter(ctx, input, exit_code)
			} else {
				pkg::filter(ctx, input, exit_code)
			}
		},
		"pip" | "pip3" | "bundle" | "brew" | "composer" | "uv" | "poetry" => {
			pkg::filter(ctx, input, exit_code)
		},
		"bun" | "bunx" => bun::filter(ctx, input, exit_code),
		"pytest" | "ruff" | "mypy" | "python" | "python3" | "py" => {
			python::filter(ctx, input, exit_code)
		},
		"rspec" | "rake" | "rails" | "rubocop" => ruby::filter(ctx, input, exit_code),
		"jest" | "vitest" | "playwright" => node_tests::filter(ctx, input, exit_code),
		"next" | "prettier" | "prisma" => js_tools::filter(ctx, input, exit_code),
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

	#[test]
	fn run_invoked_word_picks_script_not_arguments() {
		assert_eq!(run_invoked_word("npm run build -- test"), Some("build"));
		assert_eq!(run_invoked_word("npm run test"), Some("test"));
		assert_eq!(run_invoked_word("npm run --silent test:unit"), Some("test:unit"));
		assert_eq!(run_invoked_word("uv run echo pytest"), Some("echo"));
		assert_eq!(run_invoked_word("uv run build -- pytest"), Some("build"));
		assert_eq!(run_invoked_word("uv run -- pytest"), Some("pytest"));
		assert_eq!(run_invoked_word("uv run python -m pytest"), Some("python"));
		assert_eq!(run_invoked_word("npm ci"), None);
	}

	#[test]
	fn pkg_test_routing_ignores_test_as_argument() {
		let config = MinimizerConfig::default();
		// a non-test script that merely passes `test` as an argument must not
		// route as a test
		assert!(!is_pkg_test_invocation(&ctx("npm", Some("run"), "npm run build -- test", &config)));
		assert!(is_pkg_test_invocation(&ctx("npm", Some("run"), "npm run test", &config)));
		assert!(is_pkg_test_invocation(&ctx("npm", Some("test"), "npm test", &config)));
	}

	#[test]
	fn pkg_lint_routing_ignores_tool_as_argument() {
		let config = MinimizerConfig::default();
		assert!(!is_pkg_lint_invocation(&ctx(
			"pnpm",
			Some("run"),
			"pnpm run build -- eslint",
			&config
		)));
		assert!(is_pkg_lint_invocation(&ctx("pnpm", Some("run"), "pnpm run lint", &config)));
		assert!(is_pkg_lint_invocation(&ctx("pnpm", Some("run"), "pnpm run tsc", &config)));
	}

	#[test]
	fn npm_test_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("npm", Some("test"), "npm test", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}

	#[test]
	fn npm_run_test_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("npm", Some("run"), "npm run test", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}

	#[test]
	fn npm_run_quoted_test_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("npm", Some("run"), "npm run \"test\"", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}

	#[test]
	fn pnpm_test_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("pnpm", Some("test"), "pnpm test", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}

	#[test]
	fn pnpm_run_test_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("pnpm", Some("run"), "pnpm run test", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}

	#[test]
	fn yarn_test_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("yarn", Some("test"), "yarn test", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}

	#[test]
	fn yarn_run_test_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("yarn", Some("run"), "yarn run test", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}

	#[test]
	fn npm_run_build_still_uses_pkg_filter() {
		let config = MinimizerConfig::default();
		let context = ctx("npm", Some("run"), "npm run build", &config);
		let out = filter(&context, "Resolving dependencies\nDownloaded foo\nerror: failed\n", 1).text;
		assert!(!out.contains("Resolving dependencies"));
		assert!(out.contains("error: failed"));
	}

	#[test]
	fn package_manager_lint_scripts_route_to_lint_filter() {
		let config = MinimizerConfig::default();
		let input = concat!(
			"src/app.ts:1:1: error TS2322: Type 'string' is not assignable to type 'number'.\n",
			"src/app.ts:2:1: error TS7006: Parameter 'x' implicitly has an 'any' type.\n",
		);

		for (program, command) in [
			("npm", "npm run lint"),
			("npm", "npm run typecheck"),
			("pnpm", "pnpm run lint:ci"),
			("yarn", "yarn run typecheck:ci"),
		] {
			let context = ctx(program, Some("run"), command, &config);
			let routed = filter(&context, input, 1).text;
			let expected = lint::filter(&context, input, 1).text;
			assert_eq!(routed, expected, "{command} should use lint filter");
			assert!(
				routed.contains("2 diagnostics in 1 files"),
				"{command} should condense lint output"
			);
		}
	}

	#[test]
	fn npm_t_routes_to_node_tests() {
		let config = MinimizerConfig::default();
		let context = ctx("npm", Some("t"), "npm t", &config);
		let out = filter(&context, "✓ ok\nFAIL app.test.ts\nTests 1 failed\n", 1).text;
		assert!(!out.contains("✓ ok"));
		assert!(out.contains("FAIL app.test.ts"));
	}
}
