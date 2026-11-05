//! Declarative filter pipelines loaded from TOML.
//!
//! Companion to the Rust-native filters in [`filters`](super::filters). A
//! pipeline is a small, data-driven transform compiled from a TOML definition
//! that ships either as a built-in (concatenated at build time) or supplied
//! through the user's `sv_minimizer_filters` file via
//! [`MinimizerConfig::load_user_filters`](crate::MinimizerConfig::load_user_filters).
//!
//! ## Pipeline stages (applied in order)
//!
//! 1. `strip_ansi`           — remove ANSI CSI escape codes
//! 2. `replace`              — ordered regex substitutions, line-by-line
//! 3. `match_output`         — short-circuit to a one-line summary when the
//!    full output blob matches, honoring an optional `unless` anti-pattern
//! 4. `strip_lines_matching` / `keep_lines_matching` — a line survives iff it
//!    matches the keep set (when present) and does not match the strip set
//!    (when present)
//! 5. `replace_after`        — ordered regex substitutions, line-by-line,
//!    applied after line filtering and before truncation so substitutions that
//!    shorten lines (e.g. path compaction) see the full, untruncated text
//! 6. `truncate_lines_at`    — per-line Unicode-safe char cap
//! 7. `head_lines` / `tail_lines` — keep first/last N lines with a marker
//! 8. `max_lines`            — hard cap after head/tail
//! 9. `preserve_if_empty`    — keep the original input when filtering removes
//!    every line
//! 10. `on_empty`            — replace an empty result with a sentinel
//!
//! Pipelines never panic for the caller: regex compilation errors are
//! surfaced when the pipeline is loaded, and runtime application is total.

use std::borrow::Cow;

use regex::{Regex, RegexSet};
use serde::Deserialize;
use xutf::ToAnsiStripped as _;

use crate::primitives;

/// Raw TOML shape for a single filter definition.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PipelineDef {
	/// Human-readable one-liner. Not consumed at runtime.
	#[serde(default)]
	pub description:            Option<String>,
	/// Regex that selects which commands this pipeline claims. Matched against
	/// the first token of the command (post-wrapper stripping).
	pub match_command:          String,
	/// Optional regex matched against the detected subcommand. When absent,
	/// any subcommand is accepted.
	#[serde(default)]
	pub match_subcommand:       Option<String>,
	/// Remove ANSI/VT escape sequences first.
	#[serde(default)]
	pub strip_ansi:             bool,
	/// Ordered regex substitutions applied line by line before filtering.
	#[serde(default)]
	pub replace:                Vec<ReplaceDef>,
	/// Whole-output matches that short-circuit to a one-line message.
	#[serde(default)]
	pub match_output:           Vec<MatchOutputDef>,
	/// Drop lines matching any of these regexes.
	#[serde(default)]
	pub strip_lines_matching:   Vec<String>,
	/// Keep only lines matching one of these regexes.
	#[serde(default)]
	pub keep_lines_matching:    Vec<String>,
	/// Ordered regex substitutions applied after the strip/keep line filter
	/// and before `truncate_lines_at`, so substitutions that shorten lines
	/// (e.g. path compaction) see the full, untruncated text.
	#[serde(default)]
	pub replace_after:          Vec<ReplaceDef>,
	/// Per-line cap in Unicode scalars.
	pub truncate_lines_at:      Option<usize>,
	/// Keep the first N lines.
	pub head_lines:             Option<usize>,
	/// Keep the last N lines.
	pub tail_lines:             Option<usize>,
	/// Hard line cap after head/tail.
	pub max_lines:              Option<usize>,
	/// Sentinel returned when filtering leaves nothing.
	pub on_empty:               Option<String>,
	/// Return the original input unchanged when all filtering stages remove it.
	/// Useful for diagnostic filters that must not discard an unrecognized
	/// successful output, such as a compiler query response.
	#[serde(default)]
	pub preserve_if_empty:      bool,
	/// Apply only when the command exit code is in this list. Empty = always.
	#[serde(default)]
	pub only_on_exit:           Vec<i32>,
	/// Apply only when the command exit code is NOT in this list. Empty =
	/// always.
	#[serde(default)]
	pub except_on_exit:         Vec<i32>,
	/// Leave output of at most this many bytes untouched. Unset = always
	/// apply.
	pub keep_whole_up_to_bytes: Option<usize>,
}

/// One ordered regex substitution.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceDef {
	/// Regex matched against each line.
	pub pattern:     String,
	/// Replacement text; `$1`-style group references expand.
	pub replacement: String,
}

/// A whole-output short-circuit: when `pattern` matches, the output becomes
/// `message`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchOutputDef {
	/// Regex matched against the whole output.
	pub pattern: String,
	/// The one-line replacement.
	pub message: String,
	/// Anti-pattern: when it also matches, the rule does not fire.
	#[serde(default)]
	pub unless:  Option<String>,
}

/// Inline filter test embedded next to pipeline definitions via
/// `[[tests.NAME]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineTest {
	/// Test label.
	pub name:     String,
	/// Captured output fed to the pipeline.
	pub input:    String,
	/// Expected pipeline output.
	pub expected: String,
	/// Exit code the exit gates see; absent skips exit gating.
	#[serde(default)]
	pub exit:     Option<i32>,
}

/// On-disk schema for the builtin / user settings TOML.
#[derive(Debug, Deserialize, Default)]
pub struct PipelineFile {
	/// Schema version; absent means the current one.
	pub schema_version: Option<u32>,
	/// Filter definitions by name.
	#[serde(default)]
	pub filters:        std::collections::BTreeMap<String, PipelineDef>,
	/// Inline tests by filter name.
	#[serde(default)]
	pub tests:          std::collections::BTreeMap<String, Vec<PipelineTest>>,
}

/// The only `schema_version` this build reads.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// A definition file that cannot be loaded.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
	/// The contents are not TOML of the definition schema.
	#[error("[{source_label}] TOML parse error")]
	Toml {
		/// Which registry the file feeds (`builtin`, `user`).
		source_label: &'static str,
		/// The TOML parser's diagnostic.
		#[source]
		source:       toml::de::Error,
	},
	/// The file declares a schema version this build does not read.
	#[error(
		"[{source_label}] unsupported schema_version {version} (expected {SUPPORTED_SCHEMA_VERSION})"
	)]
	UnsupportedSchema {
		/// Which registry the file feeds (`builtin`, `user`).
		source_label: &'static str,
		/// The declared version.
		version:      u32,
	},
	/// One or more definitions have a regex that does not compile.
	#[error("[{source_label}] {} filter definition(s) do not compile", errors.len())]
	Compile {
		/// Which registry the file feeds (`builtin`, `user`).
		source_label: &'static str,
		/// Every failing pattern, in definition order.
		errors:       Vec<CompileError>,
	},
}

/// A definition regex that does not compile.
#[derive(Debug, thiserror::Error)]
#[error("filter '{filter}': invalid {key}")]
pub struct CompileError {
	/// Name of the filter definition.
	pub filter: String,
	/// TOML key holding the pattern (`match_command`, `replace`, ...).
	pub key:    &'static str,
	/// The regex compiler's diagnostic; it quotes the pattern.
	#[source]
	pub source: regex::Error,
}

#[derive(Debug)]
struct CompiledReplace {
	pattern:     Regex,
	replacement: String,
}

#[derive(Debug)]
struct CompiledMatchOutput {
	pattern: Regex,
	message: String,
	unless:  Option<Regex>,
}

/// A pipeline with every regex pre-compiled.
#[derive(Debug)]
pub struct CompiledPipeline {
	name:                   String,
	match_command:          Regex,
	match_subcommand:       Option<Regex>,
	strip_ansi:             bool,
	replace:                Vec<CompiledReplace>,
	match_output:           Vec<CompiledMatchOutput>,
	strip_lines:            Option<RegexSet>,
	keep_lines:             Option<RegexSet>,
	replace_after:          Vec<CompiledReplace>,
	truncate_lines_at:      Option<usize>,
	head_lines:             Option<usize>,
	tail_lines:             Option<usize>,
	max_lines:              Option<usize>,
	on_empty:               Option<String>,
	preserve_if_empty:      bool,
	only_on_exit:           Vec<i32>,
	except_on_exit:         Vec<i32>,
	keep_whole_up_to_bytes: Option<usize>,
}

fn compile_regex(filter: &str, key: &'static str, pattern: &str) -> Result<Regex, CompileError> {
	Regex::new(pattern).map_err(|source| CompileError { filter: filter.to_owned(), key, source })
}

fn compile_set(
	filter: &str,
	key: &'static str,
	patterns: &[String],
) -> Result<Option<RegexSet>, CompileError> {
	if patterns.is_empty() {
		return Ok(None);
	}
	RegexSet::new(patterns)
		.map(Some)
		.map_err(|source| CompileError { filter: filter.to_owned(), key, source })
}

/// Compile an ordered regex-substitution list; `key` names the TOML key
/// (`replace` or `replace_after`) in errors.
fn compile_replaces(
	filter: &str,
	rules: Vec<ReplaceDef>,
	key: &'static str,
) -> Result<Vec<CompiledReplace>, CompileError> {
	rules
		.into_iter()
		.map(|rule| {
			let pattern = compile_regex(filter, key, &rule.pattern)?;
			Ok(CompiledReplace { pattern, replacement: rule.replacement })
		})
		.collect()
}

/// Compile a raw TOML definition, naming the failing key on regex errors.
pub fn compile(name: String, def: PipelineDef) -> Result<CompiledPipeline, CompileError> {
	let match_command = compile_regex(&name, "match_command", &def.match_command)?;
	let match_subcommand = def
		.match_subcommand
		.as_deref()
		.map(|pattern| compile_regex(&name, "match_subcommand", pattern))
		.transpose()?;

	let replace = compile_replaces(&name, def.replace, "replace")?;
	let replace_after = compile_replaces(&name, def.replace_after, "replace_after")?;

	let match_output = def
		.match_output
		.into_iter()
		.map(|rule| {
			let pattern = compile_regex(&name, "match_output", &rule.pattern)?;
			let unless = rule
				.unless
				.as_deref()
				.map(|unless| compile_regex(&name, "match_output.unless", unless))
				.transpose()?;
			Ok(CompiledMatchOutput { pattern, message: rule.message, unless })
		})
		.collect::<Result<Vec<_>, CompileError>>()?;

	let strip_lines = compile_set(&name, "strip_lines_matching", &def.strip_lines_matching)?;
	let keep_lines = compile_set(&name, "keep_lines_matching", &def.keep_lines_matching)?;

	Ok(CompiledPipeline {
		name,
		match_command,
		match_subcommand,
		strip_ansi: def.strip_ansi,
		replace,
		match_output,
		strip_lines,
		keep_lines,
		replace_after,
		truncate_lines_at: def.truncate_lines_at,
		head_lines: def.head_lines,
		tail_lines: def.tail_lines,
		max_lines: def.max_lines,
		on_empty: def.on_empty,
		preserve_if_empty: def.preserve_if_empty,
		only_on_exit: def.only_on_exit,
		except_on_exit: def.except_on_exit,
		keep_whole_up_to_bytes: def.keep_whole_up_to_bytes,
	})
}

impl CompiledPipeline {
	/// Whether this pipeline claims the given `(program, subcommand)` pair.
	#[must_use]
	pub fn matches(&self, program: &str, subcommand: Option<&str>) -> bool {
		if !self.match_command.is_match(program) {
			return false;
		}
		if let Some(sub_rx) = self.match_subcommand.as_ref() {
			let sub = subcommand.unwrap_or("");
			if !sub_rx.is_match(sub) {
				return false;
			}
		}
		true
	}

	/// Whether this pipeline is gated off for the supplied exit code.
	#[must_use]
	pub fn skipped_by_exit(&self, exit_code: i32) -> bool {
		if !self.only_on_exit.is_empty() && !self.only_on_exit.contains(&exit_code) {
			return true;
		}
		if self.except_on_exit.contains(&exit_code) {
			return true;
		}
		false
	}

	/// Whether this pipeline leaves `input_bytes` of output untouched.
	#[must_use]
	pub fn skipped_by_size(&self, input_bytes: usize) -> bool {
		self
			.keep_whole_up_to_bytes
			.is_some_and(|max| input_bytes <= max)
	}

	/// Whether this pipeline, run on `input_bytes` of output from a run that
	/// exited with `exit_code`, drops whole lines (`max_lines`, `head_lines`,
	/// `tail_lines`).
	#[must_use]
	pub fn caps_lines(&self, exit_code: i32, input_bytes: usize) -> bool {
		!self.skipped_by_exit(exit_code)
			&& !self.skipped_by_size(input_bytes)
			&& (self.max_lines.is_some() || self.head_lines.is_some() || self.tail_lines.is_some())
	}

	/// Apply the full 10-stage pipeline to `input`.
	#[must_use]
	pub fn apply<'a>(&self, input: &'a str) -> Cow<'a, str> {
		// Stage 1: strip_ansi
		let stage1: Cow<'_, str> = if self.strip_ansi {
			Cow::Owned(input.to_ansi_stripped())
		} else {
			Cow::Borrowed(input)
		};

		// Stage 2: replace (ordered, line-by-line)
		let stage2 = apply_replaces(stage1, &self.replace);

		// Stage 3: match_output short-circuit
		if !self.match_output.is_empty() {
			for rule in &self.match_output {
				if !rule.pattern.is_match(&stage2) {
					continue;
				}
				if let Some(anti) = rule.unless.as_ref()
					&& anti.is_match(&stage2)
				{
					continue;
				}
				return Cow::Owned(rule.message.clone());
			}
		}

		// Stage 4: strip/keep lines (keep AND NOT strip; absent set = no
		// constraint)
		let stage4: Cow<'_, str> = if self.strip_lines.is_some() || self.keep_lines.is_some() {
			Cow::Owned(primitives::filter_lines_regex(
				&stage2,
				self.strip_lines.as_ref(),
				self.keep_lines.as_ref(),
			))
		} else {
			stage2
		};

		// Stage 5: replace_after (ordered, line-by-line) — post-filter so
		// only surviving lines are substituted, pre-truncate so shortening
		// substitutions (path compaction) see the full line.
		let stage5 = apply_replaces(stage4, &self.replace_after);

		// Stage 6: truncate each line
		let stage6: Cow<'_, str> = if let Some(n) = self.truncate_lines_at {
			let mut out = String::with_capacity(stage5.len());
			for line in stage5.lines() {
				out.push_str(&primitives::truncate_line(line, n));
				out.push('\n');
			}
			Cow::Owned(out)
		} else {
			stage5
		};

		// Stage 7: head + tail
		let stage7: Cow<'_, str> = match (self.head_lines, self.tail_lines) {
			(Some(h), Some(t)) => Cow::Owned(primitives::head_tail_lines(&stage6, h, t)),
			(Some(h), None) => Cow::Owned(primitives::head_lines_only(&stage6, h)),
			(None, Some(t)) => Cow::Owned(primitives::tail_lines_only(&stage6, t)),
			(None, None) => stage6,
		};

		// Stage 8: max_lines
		let stage8: Cow<'_, str> = if let Some(m) = self.max_lines {
			Cow::Owned(primitives::max_lines(&stage7, m))
		} else {
			stage7
		};

		// Stage 9: preserve the source when a keep-only filter removed every
		// line. This prevents a successful query/diagnostic command's
		// meaningful output from being replaced by the engine's generic "OK"
		// sentinel.
		if self.preserve_if_empty && stage8.trim().is_empty() {
			return Cow::Borrowed(input);
		}

		// Stage 10: on_empty
		if let Some(msg) = self.on_empty.as_deref()
			&& stage8.trim().is_empty()
		{
			return Cow::Owned(msg.to_string());
		}

		stage8
	}
}

/// Apply an ordered regex-substitution list line-by-line. Returns `input`
/// untouched (no allocation) when `rules` is empty.
fn apply_replaces<'a>(input: Cow<'a, str>, rules: &[CompiledReplace]) -> Cow<'a, str> {
	if rules.is_empty() {
		return input;
	}
	let mut out = String::with_capacity(input.len());
	for line in input.lines() {
		let mut current = Cow::Borrowed(line);
		for rule in rules {
			let replaced = rule
				.pattern
				.replace_all(&current, rule.replacement.as_str());
			if let Cow::Owned(s) = replaced {
				current = Cow::Owned(s);
			}
		}
		out.push_str(&current);
		out.push('\n');
	}
	Cow::Owned(out)
}

/// Return type of [`parse_file`]: the compiled pipelines alongside their
/// inline tests grouped by pipeline name.
pub type ParsedPipelineFile = (Vec<CompiledPipeline>, Vec<(String, Vec<PipelineTest>)>);

/// Compiled registry of all known pipelines, listed in priority order
/// (builtin last — user definitions win). Also carries the inline tests so
/// `verify()` can exercise them.
#[derive(Debug, Default)]
pub struct PipelineRegistry {
	/// Compiled pipelines in priority order.
	pub pipelines: Vec<CompiledPipeline>,
	/// Inline tests grouped by pipeline name.
	pub tests:     Vec<(String, Vec<PipelineTest>)>,
}

impl PipelineRegistry {
	/// Find the first pipeline that claims this `(program, subcommand)` pair.
	#[must_use]
	pub fn find(&self, program: &str, subcommand: Option<&str>) -> Option<&CompiledPipeline> {
		self
			.pipelines
			.iter()
			.find(|p| p.matches(program, subcommand))
	}
}

/// Parse and compile a full TOML file. Collects every compile error into one
/// [`PipelineError::Compile`]; a well-formed file with N filters returns
/// `Ok((Vec<CompiledPipeline>, tests))`.
pub fn parse_file(
	contents: &str,
	source_label: &'static str,
) -> Result<ParsedPipelineFile, PipelineError> {
	let file: PipelineFile =
		toml::from_str(contents).map_err(|source| PipelineError::Toml { source_label, source })?;

	if let Some(version) = file.schema_version
		&& version != SUPPORTED_SCHEMA_VERSION
	{
		return Err(PipelineError::UnsupportedSchema { source_label, version });
	}

	let mut compiled = Vec::with_capacity(file.filters.len());
	let mut errors = Vec::new();
	for (name, def) in file.filters {
		match compile(name, def) {
			Ok(pipeline) => compiled.push(pipeline),
			Err(error) => errors.push(error),
		}
	}
	if !errors.is_empty() {
		return Err(PipelineError::Compile { source_label, errors });
	}

	let tests = file.tests.into_iter().collect();
	Ok((compiled, tests))
}

/// Outcome for a single inline test.
#[derive(Debug, Clone)]
pub struct TestOutcome {
	/// Pipeline the test exercises.
	pub filter_name: String,
	/// Test label.
	pub test_name:   String,
	/// Whether `actual` equals `expected`.
	pub passed:      bool,
	/// What the pipeline produced.
	pub actual:      String,
	/// What the test expects.
	pub expected:    String,
}

/// Run every inline test in `registry` and return the outcomes.
#[must_use]
pub fn run_tests(registry: &PipelineRegistry) -> Vec<TestOutcome> {
	let mut out = Vec::new();
	for (filter_name, tests) in &registry.tests {
		let Some(pipeline) = registry.pipelines.iter().find(|p| &p.name == filter_name) else {
			for test in tests {
				out.push(TestOutcome {
					filter_name: filter_name.clone(),
					test_name:   test.name.clone(),
					passed:      false,
					actual:      format!("pipeline '{filter_name}' not found"),
					expected:    test.expected.clone(),
				});
			}
			continue;
		};
		for test in tests {
			if test.exit.is_some_and(|exit| pipeline.skipped_by_exit(exit))
				|| pipeline.skipped_by_size(test.input.len())
			{
				// Exit or size gate: the pipeline leaves this input alone, so
				// the expected output is the raw input unchanged.
				let passed = test.input == test.expected;
				out.push(TestOutcome {
					filter_name: filter_name.clone(),
					test_name: test.name.clone(),
					passed,
					actual: test.input.clone(),
					expected: test.expected.clone(),
				});
				continue;
			}
			let actual = pipeline.apply(&test.input).to_string();
			let passed = actual == test.expected;
			out.push(TestOutcome {
				filter_name: filter_name.clone(),
				test_name: test.name.clone(),
				passed,
				actual,
				expected: test.expected.clone(),
			});
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn compile_one(toml_src: &str) -> CompiledPipeline {
		let (mut pipelines, _) = parse_file(toml_src, "test").expect("parse + compile");
		pipelines.pop().expect("one pipeline")
	}

	#[test]
	fn pipeline_runs_all_stages() {
		let src = r#"
schema_version = 1
[filters.demo]
match_command = "^demo$"
strip_ansi = true
strip_lines_matching = ["^Downloading"]
truncate_lines_at = 10
max_lines = 5
on_empty = "demo: ok"

[[tests.demo]]
name = "basic"
input = "\u001b[31mfirst\u001b[0m\nDownloading foo\nA_really_long_line_indeed\n"
expected = "first\nA_really_l\u2026[+15]\n"
"#;
		let pipeline = compile_one(src);
		let out =
			pipeline.apply("\u{1b}[31mfirst\u{1b}[0m\nDownloading foo\nA_really_long_line_indeed\n");
		assert_eq!(out.as_ref(), "first\nA_really_l\u{2026}[+15]\n");
	}

	#[test]
	fn short_circuit_wins_over_later_stages() {
		let src = r#"
schema_version = 1
[filters.ok]
match_command = "^ok$"
[[filters.ok.match_output]]
pattern = "BUILD SUCCESSFUL"
message = "build: ok"
"#;
		let pipeline = compile_one(src);
		let out = pipeline.apply("noise\nBUILD SUCCESSFUL in 5s\n");
		assert_eq!(out.as_ref(), "build: ok");
	}

	#[test]
	fn unless_prevents_swallowing_errors() {
		let src = r#"
schema_version = 1
[filters.ok]
match_command = "^ok$"
[[filters.ok.match_output]]
pattern = "BUILD SUCCESSFUL"
message = "build: ok"
unless = "(?i)error|fail"
"#;
		let pipeline = compile_one(src);
		let out = pipeline.apply("BUILD SUCCESSFUL but later ERROR: oops\n");
		assert!(out.as_ref().contains("ERROR"));
	}

	#[test]
	fn strip_and_keep_combine_as_keep_and_not_strip() {
		let src = r#"
schema_version = 1
[filters.combo]
match_command = "^combo$"
keep_lines_matching = ["^> Task "]
strip_lines_matching = ["UP-TO-DATE$"]
"#;
		let pipeline = compile_one(src);
		let out = pipeline.apply("> Task :a UP-TO-DATE\n> Task :b\nDownloading dep\n");
		// Only lines matching the keep set AND not the strip set survive.
		assert_eq!(out.as_ref(), "> Task :b\n");
	}

	#[test]
	fn replace_after_substitutes_surviving_lines_post_filter() {
		let src = r#"
schema_version = 1
[filters.paths]
match_command = "^paths$"
keep_lines_matching = ["^src/"]
[[filters.paths.replace_after]]
pattern = "^src/very/long/prefix/"
replacement = ".../"
"#;
		let pipeline = compile_one(src);
		// The keep filter matches the ORIGINAL line (`^src/`); if the
		// substitution ran before filtering, the rewritten `.../main.rs`
		// would no longer match the keep set and be dropped.
		let out = pipeline.apply("src/very/long/prefix/main.rs\nnoise line\nsrc/lib.rs\n");
		assert_eq!(out.as_ref(), ".../main.rs\nsrc/lib.rs\n");
	}

	#[test]
	fn replace_after_runs_before_truncate_lines_at() {
		let src = r#"
schema_version = 1
[filters.shorten]
match_command = "^shorten$"
truncate_lines_at = 12
[[filters.shorten.replace_after]]
pattern = "^/workspace/project/deep/"
replacement = ""
"#;
		let pipeline = compile_one(src);
		// The raw line exceeds the 12-char cap; the substitution shortens it
		// below the cap first, so no truncation marker appears. If truncation
		// ran first, the pattern would no longer match the clipped line.
		let out = pipeline.apply("/workspace/project/deep/file.rs\n");
		assert_eq!(out.as_ref(), "file.rs\n");
	}

	#[test]
	fn preserve_if_empty_returns_original_when_filter_drops_every_line() {
		let src = r#"
schema_version = 1
[filters.keep]
match_command = "^keep$"
keep_lines_matching = ["^diagnostic:"]
preserve_if_empty = true
"#;
		let pipeline = compile_one(src);
		let input = "query result\n";
		assert_eq!(pipeline.apply(input).as_ref(), input);
	}

	#[test]
	fn on_empty_fires_when_output_is_blank() {
		let src = r#"
schema_version = 1
[filters.silent]
match_command = "^silent$"
strip_lines_matching = [".*"]
on_empty = "silent: ok"
"#;
		let pipeline = compile_one(src);
		let out = pipeline.apply("noise\nmore noise\n");
		assert_eq!(out.as_ref(), "silent: ok");
	}

	#[test]
	fn exit_gates_respected() {
		let src = r#"
schema_version = 1
[filters.okonly]
match_command = "^okonly$"
only_on_exit = [0]
"#;
		let pipeline = compile_one(src);
		assert!(!pipeline.skipped_by_exit(0));
		assert!(pipeline.skipped_by_exit(1));

		let src2 = r#"
schema_version = 1
[filters.notfail]
match_command = "^notfail$"
except_on_exit = [1, 2]
"#;
		let pipeline2 = compile_one(src2);
		assert!(!pipeline2.skipped_by_exit(0));
		assert!(pipeline2.skipped_by_exit(1));
	}

	#[test]
	fn unsupported_schema_errors() {
		let src = "schema_version = 99\n[filters.foo]\nmatch_command = \"^foo$\"\n";
		let err = parse_file(src, "bad").unwrap_err();
		assert!(matches!(err, PipelineError::UnsupportedSchema { version: 99, .. }), "{err:?}");
	}

	#[test]
	fn inline_tests_run_and_pass() {
		let src = r#"
schema_version = 1
[filters.gradle]
match_command = "^gradle$"
strip_lines_matching = ["^> Task :.*UP-TO-DATE$"]
on_empty = "gradle: ok"

[[tests.gradle]]
name = "strip UP-TO-DATE"
input = "> Task :a UP-TO-DATE\n> Task :b\n"
expected = "> Task :b\n"

[[tests.gradle]]
name = "empty becomes sentinel"
input = "> Task :a UP-TO-DATE\n"
expected = "gradle: ok"
"#;
		let (pipelines, tests) = parse_file(src, "test").expect("ok");
		let registry = PipelineRegistry { pipelines, tests };
		let outcomes = run_tests(&registry);
		assert_eq!(outcomes.len(), 2);
		for outcome in outcomes {
			assert!(
				outcome.passed,
				"test {} failed: actual={:?} expected={:?}",
				outcome.test_name, outcome.actual, outcome.expected
			);
		}
	}
}
