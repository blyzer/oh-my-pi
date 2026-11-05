//! Normalization for checker output formats.

mod cargo;
mod junit;
mod stylelint;
mod terraform;

use std::{fmt::Write as _, path::Path, sync::LazyLock};

use omp_core::{Str, sf};
use regex::Regex;

use super::types::{Diagnostic, Severity};

/// Code of the diagnostic an unreadable structured report degrades to.
const UNPARSEABLE: &str = "unparseable";

static LOCATION: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"^(?P<file>.*?):(?P<line>\d+)(?::(?P<column>\d+))?(?::|\s+-\s+|\s+)(?P<message>.+)$")
		.expect("static diagnostic location regex")
});
static RUST_ARROW: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"^\s*-->\s+(?P<file>.+?):(?P<line>\d+):(?P<column>\d+)\s*$")
		.expect("static Rust span regex")
});
static CODE_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(
		r"^(?P<severity>error|warning|warn|info|note)(?:\[(?P<code>[^]]+)\])?:\s*(?P<message>.+)$",
	)
	.expect("static severity regex")
});

/// Supported checker-output parser vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParserKind {
	/// `cargo check` and `cargo clippy` with `--message-format=json`.
	Rust,
	/// `cargo test --message-format=json`: compiler records and test panics.
	RustTest,
	/// Go compiler output.
	Go,
	/// `go test -json`.
	GoTest,
	/// `JUnit` XML report on stdout.
	#[serde(rename = "junit")]
	JUnit,
	/// staticcheck Unix output.
	Staticcheck,
	/// golangci-lint JSON.
	Golangci,
	/// Ruff JSON.
	Ruff,
	/// Pyright JSON.
	Pyright,
	/// Mypy text.
	Mypy,
	/// Pylint JSON.
	Pylint,
	/// Flake8 text.
	Flake8,
	/// Astral ty text.
	Ty,
	/// ESLint JSON.
	Eslint,
	/// Biome JSON.
	Biome,
	/// Oxlint Unix output.
	Oxlint,
	/// Deno lint JSON.
	DenoLint,
	/// Stylelint JSON.
	Stylelint,
	/// Rubocop JSON.
	Rubocop,
	/// PHPStan JSON.
	Phpstan,
	/// Psalm JSON.
	Psalm,
	/// SwiftLint JSON.
	Swiftlint,
	/// Dart machine output.
	Dart,
	/// Credo JSON.
	Credo,
	/// ShellCheck JSON.
	Shellcheck,
	/// HLint JSON.
	Hlint,
	/// Terraform validate JSON.
	Terraform,
	/// TFLint JSON.
	Tflint,
	/// Actionlint text.
	Actionlint,
	/// Generic file/line/column text or JSON.
	Generic,
}

/// Captured checker output.
pub struct ParserInput<'a> {
	/// Stable checker id.
	pub checker:      &'a str,
	/// Checker manifest root.
	pub checker_root: &'a Path,
	/// Project root used to relativize paths.
	pub project_root: &'a Path,
	/// Standard output.
	pub stdout:       &'a str,
	/// Standard error.
	pub stderr:       &'a str,
}

/// Parses one invocation into deduplicated project-relative diagnostics.
pub fn parse(kind: ParserKind, input: &ParserInput<'_>) -> Vec<Diagnostic> {
	let mut diagnostics = match kind {
		ParserKind::Rust | ParserKind::RustTest => cargo::parse(kind, input),
		ParserKind::JUnit => structured(input, junit::parse(input)),
		_ => generic(kind, input),
	};
	diagnostics.sort_by(|left, right| {
		left
			.file
			.cmp(&right.file)
			.then(left.line.cmp(&right.line))
			.then(left.column.cmp(&right.column))
			.then(left.message.cmp(&right.message))
	});
	diagnostics.dedup_by(|left, right| {
		left.file == right.file
			&& left.line == right.line
			&& left.column == right.column
			&& left.code == right.code
			&& left.message == right.message
	});
	diagnostics
}

fn generic(kind: ParserKind, input: &ParserInput<'_>) -> Vec<Diagnostic> {
	let combined = if input.stderr.is_empty() {
		input.stdout.to_owned()
	} else if input.stdout.is_empty() {
		input.stderr.to_owned()
	} else {
		format!("{}\n{}", input.stdout, input.stderr)
	};
	let mut diagnostics = Vec::new();
	for value in json_values(&combined) {
		parse_json_value(kind, &value, None, input, &mut diagnostics);
	}
	parse_text(kind, &combined, input, &mut diagnostics);
	diagnostics
}

/// Flattens a structured parser's outcome. An unreadable report becomes one
/// project-wide `unparseable` diagnostic carrying the reason and an output
/// excerpt, never an empty result that would read as a clean run.
fn structured<E: std::error::Error>(
	input: &ParserInput<'_>,
	outcome: Result<Vec<Diagnostic>, E>,
) -> Vec<Diagnostic> {
	let error = match outcome {
		Ok(diagnostics) => return diagnostics,
		Err(error) => error,
	};
	let mut message = String::new();
	let _ = write!(message, "{error}");
	let mut source = error.source();
	while let Some(cause) = source {
		let mark = message.len();
		let _ = write!(message, ": {cause}");
		// quick-xml repeats its source in its own message.
		if message[..mark].ends_with(&message[mark + 2..]) {
			message.truncate(mark);
		}
		source = cause.source();
	}
	let output = if input.stdout.trim().is_empty() {
		input.stderr
	} else {
		input.stdout
	};
	let output = excerpt(output).trim();
	if !output.is_empty() {
		message.push_str(": ");
		message.push_str(output);
	}
	vec![Diagnostic {
		checker:    Str::from(input.checker),
		file:       None,
		line:       None,
		column:     None,
		end_line:   None,
		end_column: None,
		code:       Some(sf!(UNPARSEABLE)),
		severity:   Severity::Error,
		message:    Str::from(message),
		suggestion: None,
	}]
}

/// First 12,000 characters of `output`: the raw output one diagnostic carries.
pub(super) fn excerpt(output: &str) -> &str {
	output
		.char_indices()
		.nth(12_000)
		.map_or(output, |(cut, _)| &output[..cut])
}

fn json_values(text: &str) -> Vec<serde_json::Value> {
	let mut values = Vec::new();
	if let Ok(value) = serde_json::from_str(text.trim()) {
		values.push(value);
		return values;
	}
	for line in text.lines() {
		let line = line.trim();
		if (line.starts_with('{') || line.starts_with('['))
			&& let Ok(value) = serde_json::from_str(line)
		{
			values.push(value);
		}
	}
	values
}

fn parse_json_value(
	kind: ParserKind,
	value: &serde_json::Value,
	file: Option<&str>,
	input: &ParserInput<'_>,
	output: &mut Vec<Diagnostic>,
) {
	match value {
		serde_json::Value::Array(values) => {
			for value in values {
				parse_json_value(kind, value, file, input, output);
			}
		},
		serde_json::Value::Object(object) => {
			if kind == ParserKind::GoTest
				&& let Some(text) = object.get("Output").and_then(serde_json::Value::as_str)
			{
				parse_text(kind, text, input, output);
			}
			if let Some(diagnostic) = object_diagnostic(kind, object, file, input) {
				output.push(diagnostic);
			}
			// ESLint names the file once, on the parent of its `messages`.
			let file = string_at(object, FILE_KEYS).or(file);
			for key in
				["diagnostics", "messages", "errors", "issues", "files", "results", "runs", "Issues"]
			{
				if let Some(value) = object.get(key) {
					parse_json_value(kind, value, file, input, output);
				}
			}
			if let Some(value) = object.get("message")
				&& value.is_object()
			{
				parse_json_value(kind, value, file, input, output);
			}
		},
		_ => {},
	}
}

const FILE_KEYS: &[&str] = &["filePath", "filename", "file", "path", "uri"];

fn object_diagnostic(
	kind: ParserKind,
	object: &serde_json::Map<String, serde_json::Value>,
	inherited_file: Option<&str>,
	input: &ParserInput<'_>,
) -> Option<Diagnostic> {
	let message = string_at(object, &["message", "description", "reason", "text", "title", "Text"])?;
	let file = string_at(object, FILE_KEYS)
		.or_else(|| {
			object
				.get("location")
				.and_then(|location| string_value(location, &["file", "path"]))
		})
		.or_else(|| {
			object
				.get("Pos")
				.and_then(|pos| string_value(pos, &["Filename"]))
		})
		.or_else(|| {
			first_span(object).and_then(|span| string_at(span, &["file_name", "file", "path"]))
		})
		.or(inherited_file)
		.and_then(|path| relative_path(path, input));
	let line = number_at(object, &["line", "lineNumber", "startLine"])
		.or_else(|| position_number(object, "range", "start", "line"))
		.or_else(|| position_number(object, "location", "start", "line"))
		.or_else(|| field_number(object, "location", "row"))
		.or_else(|| field_number(object, "Pos", "Line"))
		.or_else(|| first_span(object).and_then(|span| number_at(span, &["line_start", "line"])))
		.and_then(one_based(kind, Axis::Line));
	let column = number_at(object, &["column", "columnNumber", "startColumn"])
		.or_else(|| position_number(object, "range", "start", "character"))
		.or_else(|| position_number(object, "range", "start", "col"))
		.or_else(|| position_number(object, "location", "start", "column"))
		.or_else(|| field_number(object, "location", "column"))
		.or_else(|| field_number(object, "Pos", "Column"))
		.or_else(|| first_span(object).and_then(|span| number_at(span, &["column_start", "column"])))
		.and_then(one_based(kind, Axis::Column));
	let end_line = number_at(object, &["endLine"])
		.or_else(|| position_number(object, "range", "end", "line"))
		.or_else(|| position_number(object, "location", "end", "line"))
		.or_else(|| field_number(object, "end_location", "row"))
		.or_else(|| first_span(object).and_then(|span| number_at(span, &["line_end"])))
		.and_then(one_based(kind, Axis::Line));
	let end_column = number_at(object, &["endColumn"])
		.or_else(|| position_number(object, "range", "end", "character"))
		.or_else(|| position_number(object, "range", "end", "col"))
		.or_else(|| position_number(object, "location", "end", "column"))
		.or_else(|| field_number(object, "end_location", "column"))
		.or_else(|| first_span(object).and_then(|span| number_at(span, &["column_end"])))
		.and_then(one_based(kind, Axis::Column));
	let code =
		string_at(object, &["code", "ruleId", "rule", "symbol", "name", "category", "FromLinter"])
			.or_else(|| {
				object
					.get("code")
					.and_then(|value| string_value(value, &["code", "value"]))
			})
			.map(Str::from)
			.or_else(|| {
				// ShellCheck numbers its codes; `SC` is the prefix its own reports use.
				let code = object
					.get("code")?
					.as_u64()
					.filter(|_| kind == ParserKind::Shellcheck)?;
				Some(sf!("SC{code}"))
			});
	let severity = object
		.get("severity")
		.or_else(|| object.get("level"))
		.or_else(|| object.get("type"))
		.map(severity_value)
		.unwrap_or(Severity::Error);
	let suggestion = string_at(object, &["suggestion", "replacement", "help", "fix"])
		.or_else(|| object.get("children").and_then(first_help))
		.map(Str::from);
	Some(Diagnostic {
		checker: Str::from(input.checker),
		file,
		line,
		column,
		end_line,
		end_column,
		code,
		severity,
		message: Str::from(message),
		suggestion,
	})
}

fn first_span(
	object: &serde_json::Map<String, serde_json::Value>,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
	object
		.get("spans")?
		.as_array()?
		.iter()
		.find_map(serde_json::Value::as_object)
}

fn first_help(value: &serde_json::Value) -> Option<&str> {
	value.as_array()?.iter().find_map(|child| {
		let object = child.as_object()?;
		matches!(string_at(object, &["level"]), Some("help" | "note"))
			.then(|| string_at(object, &["message"]))
			.flatten()
	})
}

fn string_at<'a>(
	object: &'a serde_json::Map<String, serde_json::Value>,
	keys: &[&str],
) -> Option<&'a str> {
	keys
		.iter()
		.find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
}

fn string_value<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
	string_at(value.as_object()?, keys)
}

fn number_at(object: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<u32> {
	keys
		.iter()
		.find_map(|key| object.get(*key).and_then(number_value))
}

fn number_value(value: &serde_json::Value) -> Option<u32> {
	value
		.as_u64()
		.and_then(|value| value.try_into().ok())
		.or_else(|| value.as_str()?.parse().ok())
}

fn position_number(
	object: &serde_json::Map<String, serde_json::Value>,
	outer: &str,
	position: &str,
	field: &str,
) -> Option<u32> {
	object
		.get(outer)?
		.get(position)?
		.get(field)
		.and_then(number_value)
}

fn field_number(
	object: &serde_json::Map<String, serde_json::Value>,
	outer: &str,
	field: &str,
) -> Option<u32> {
	object.get(outer)?.get(field).and_then(number_value)
}

#[derive(Clone, Copy)]
enum Axis {
	Line,
	Column,
}

/// Converts a checker coordinate to 1-based. Each 0-based axis below was
/// observed in the checker's real JSON; every other checker numbers both axes
/// from 1, where 0 is no position.
fn one_based(kind: ParserKind, axis: Axis) -> impl Fn(u32) -> Option<u32> {
	let zero_based = match axis {
		Axis::Line => matches!(kind, ParserKind::Pyright),
		Axis::Column => {
			matches!(kind, ParserKind::Pyright | ParserKind::DenoLint | ParserKind::Pylint)
		},
	};
	move |value| {
		if zero_based {
			Some(value.saturating_add(1))
		} else {
			(value != 0).then_some(value)
		}
	}
}

fn severity_value(value: &serde_json::Value) -> Severity {
	if let Some(number) = value.as_u64() {
		return match number {
			2.. => Severity::Error,
			1 => Severity::Warning,
			_ => Severity::Info,
		};
	}
	match value
		.as_str()
		.unwrap_or_default()
		.to_ascii_lowercase()
		.as_str()
	{
		"error" | "fatal" | "failure" | "2" => Severity::Error,
		"warning" | "warn" | "convention" | "refactor" | "1" => Severity::Warning,
		_ => Severity::Info,
	}
}

fn parse_text(kind: ParserKind, text: &str, input: &ParserInput<'_>, output: &mut Vec<Diagnostic>) {
	let mut pending: Option<(Severity, Option<Str>, Str)> = None;
	for line in text.lines() {
		let line = strip_control(line);
		let line = line.trim();
		if line.is_empty() {
			continue;
		}
		if let Some(capture) = CODE_PREFIX.captures(line) {
			pending = Some((
				severity_text(&capture["severity"]),
				capture.name("code").map(|value| Str::from(value.as_str())),
				Str::from(&capture["message"]),
			));
			continue;
		}
		if let Some(capture) = RUST_ARROW.captures(line) {
			let (severity, code, message) =
				pending
					.take()
					.unwrap_or((Severity::Error, None, Str::from("compiler diagnostic")));
			output.push(Diagnostic {
				checker: Str::from(input.checker),
				file: relative_path(&capture["file"], input),
				line: capture["line"].parse().ok(),
				column: capture["column"].parse().ok(),
				end_line: None,
				end_column: None,
				code,
				severity,
				message,
				suggestion: None,
			});
			continue;
		}
		let Some(capture) = LOCATION.captures(line) else {
			continue;
		};
		let mut message = capture["message"].trim();
		let mut severity = Severity::Error;
		let mut code = None;
		if let Some((prefix, rest)) = message.split_once(':') {
			let prefix = prefix.trim();
			if matches!(
				prefix.to_ascii_lowercase().as_str(),
				"error" | "warning" | "warn" | "info" | "note"
			) {
				severity = severity_text(prefix);
				message = rest.trim();
			}
		}
		if let Some((candidate, rest)) = message.split_once(' ')
			&& candidate.len() <= 32
			&& candidate.chars().any(char::is_numeric)
		{
			code = Some(Str::from(candidate.trim_matches(['[', ']', ':'])));
			message = rest.trim_start_matches([':', ' ']);
		}
		output.push(Diagnostic {
			checker: Str::from(input.checker),
			file: relative_path(&capture["file"], input),
			line: capture["line"].parse().ok(),
			column: capture
				.name("column")
				.and_then(|value| value.as_str().parse().ok()),
			end_line: None,
			end_column: None,
			code,
			severity,
			message: Str::from(message),
			suggestion: None,
		});
	}
	if matches!(kind, ParserKind::RustTest | ParserKind::GoTest)
		&& output.is_empty()
		&& text.contains("panicked at")
	{
		output.push(Diagnostic {
			checker:    Str::from(input.checker),
			file:       None,
			line:       None,
			column:     None,
			end_line:   None,
			end_column: None,
			code:       None,
			severity:   Severity::Error,
			message:    Str::from(
				text
					.lines()
					.find(|line| line.contains("panicked at"))
					.unwrap_or("test panic"),
			),
			suggestion: None,
		});
	}
}

fn relative_path(path: &str, input: &ParserInput<'_>) -> Option<Str> {
	let path = path.trim().trim_start_matches("file://");
	if path.is_empty() || path == "<unknown>" {
		return None;
	}
	let path = Path::new(path);
	let absolute = if path.is_absolute() {
		path.to_path_buf()
	} else {
		input.checker_root.join(path)
	};
	let relative = absolute.strip_prefix(input.project_root).unwrap_or(path);
	Some(Str::from(relative.to_string_lossy().replace('\\', "/")))
}

fn severity_text(value: &str) -> Severity {
	match value.to_ascii_lowercase().as_str() {
		"error" | "fatal" => Severity::Error,
		"warning" | "warn" => Severity::Warning,
		_ => Severity::Info,
	}
}

fn strip_control(value: &str) -> String {
	let mut output = String::with_capacity(value.len());
	let mut escape = false;
	for character in value.chars() {
		if escape {
			if character.is_ascii_alphabetic() {
				escape = false;
			}
			continue;
		}
		if character == '\u{1b}' {
			escape = true;
		} else if !character.is_control() || character == '\t' {
			output.push(character);
		}
	}
	output
}

#[cfg(test)]
mod tests {
	use super::*;

	fn located(checker: &str, file: &str, span: [u32; 4], code: &str, message: &str) -> Diagnostic {
		let [line, column, end_line, end_column] = span;
		Diagnostic {
			checker:    Str::from(checker),
			file:       Some(Str::from(file)),
			line:       Some(line),
			column:     Some(column),
			end_line:   Some(end_line),
			end_column: Some(end_column),
			code:       Some(Str::from(code)),
			severity:   Severity::Error,
			message:    Str::from(message),
			suggestion: None,
		}
	}

	#[test]
	fn ruff_json_keeps_location_rows() {
		let root = Path::new("/private/tmp/walker-repro/py");
		let input = ParserInput {
			checker:      "ruff",
			checker_root: root,
			project_root: root,
			stdout:       include_str!("../../../../fixtures/ruff/check.json"),
			stderr:       "",
		};
		assert_eq!(parse(ParserKind::Ruff, &input), vec![
			located("ruff", "bad.py", [1, 8, 1, 10], "F401", "`os` imported but unused"),
			located(
				"ruff",
				"bad.py",
				[5, 5, 5, 6],
				"F841",
				"Local variable `x` is assigned to but never used"
			),
			located("ruff", "bad.py", [6, 12, 6, 26], "F821", "Undefined name `undefined_name`"),
		]);
	}

	#[test]
	fn eslint_json_messages_inherit_parent_file_path() {
		let root = Path::new("/private/tmp/walker-repro/js");
		let report = include_str!("../../../../fixtures/eslint/format-json.json");
		let input = |stdout| ParserInput {
			checker: "eslint",
			checker_root: root,
			project_root: root,
			stdout,
			stderr: "",
		};
		let expected = vec![
			located(
				"eslint",
				"bad.js",
				[1, 7, 1, 13],
				"no-unused-vars",
				"'unused' is assigned a value but never used.",
			),
			located("eslint", "bad.js", [3, 1, 3, 8], "no-undef", "'console' is not defined."),
			located("eslint", "bad.js", [3, 13, 3, 20], "no-undef", "'missing' is not defined."),
		];
		assert_eq!(parse(ParserKind::Eslint, &input(report)), expected);
		let unlocated =
			report.replacen(r#""filePath":"/private/tmp/walker-repro/js/bad.js","#, "", 1);
		let without_file = expected
			.into_iter()
			.map(|diagnostic| Diagnostic { file: None, ..diagnostic });
		assert_eq!(
			parse(ParserKind::Eslint, &input(unlocated.as_str())),
			without_file.collect::<Vec<_>>()
		);
	}

	fn input<'a>(checker: &'a str, root: &'a Path, stdout: &'a str) -> ParserInput<'a> {
		ParserInput { checker, checker_root: root, project_root: root, stdout, stderr: "" }
	}

	fn with_severity(severity: Severity, diagnostic: Diagnostic) -> Diagnostic {
		Diagnostic { severity, ..diagnostic }
	}

	fn file_level(checker: &str, file: &str, code: &str, message: &str) -> Diagnostic {
		Diagnostic {
			line: None,
			column: None,
			end_line: None,
			end_column: None,
			..located(checker, file, [0; 4], code, message)
		}
	}

	#[test]
	fn biome_json_reads_location_start_end_and_category() {
		let root = Path::new("/private/tmp/offsets-repro/js");
		let report = include_str!("../../../../fixtures/biome/check-reporter-json.json");
		let formatter = "Formatter would have printed the following content:";
		assert_eq!(parse(ParserKind::Biome, &input("biome", root, report)), vec![
			file_level("biome", "bad.js", "format", formatter),
			with_severity(
				Severity::Warning,
				located(
					"biome",
					"bad.js",
					[1, 1, 1, 4],
					"lint/style/useConst",
					"This let declares a variable that is only assigned once."
				)
			),
			with_severity(
				Severity::Warning,
				located(
					"biome",
					"bad.js",
					[1, 5, 1, 11],
					"lint/correctness/noUnusedVariables",
					"This variable unused is unused."
				)
			),
			located(
				"biome",
				"bad.js",
				[2, 5, 2, 9],
				"lint/correctness/noConstantCondition",
				"This condition always evaluates to the same value."
			),
			located(
				"biome",
				"bad.js",
				[3, 5, 3, 14],
				"lint/suspicious/noDebugger",
				"This is an unexpected use of the debugger statement."
			),
			file_level("biome", "package.json", "format", formatter),
		]);
	}

	#[test]
	fn deno_lint_json_keeps_one_based_line_and_shifts_col() {
		let root = Path::new("/private/tmp/offsets-repro/deno");
		let report = include_str!("../../../../fixtures/deno/lint-json.json");
		assert_eq!(parse(ParserKind::DenoLint, &input("deno-lint", root, report)), vec![
			located("deno-lint", "bad.ts", [1, 7, 1, 13], "no-unused-vars", "`unused` is never used"),
			located("deno-lint", "bad.ts", [2, 10, 2, 11], "no-unused-vars", "`f` is never used"),
			located(
				"deno-lint",
				"bad.ts",
				[3, 5, 3, 14],
				"no-debugger",
				"`debugger` statement is not allowed"
			),
		]);
	}

	#[test]
	fn pylint_json_shifts_zero_based_columns() {
		let root = Path::new("/private/tmp/offsets-repro/py");
		let report = include_str!("../../../../fixtures/pylint/output-format-json.json");
		let warning = |span, code, message| {
			with_severity(Severity::Warning, located("pylint", "bad.py", span, code, message))
		};
		assert_eq!(parse(ParserKind::Pylint, &input("pylint", root, report)), vec![
			Diagnostic {
				end_line: None,
				end_column: None,
				..warning([1, 1, 0, 0], "missing-module-docstring", "Missing module docstring")
			},
			warning([1, 1, 1, 10], "unused-import", "Unused import os"),
			warning(
				[4, 1, 4, 6],
				"missing-function-docstring",
				"Missing function or method docstring"
			),
			warning([5, 5, 5, 6], "unused-variable", "Unused variable 'x'"),
			located(
				"pylint",
				"bad.py",
				[6, 12, 6, 26],
				"undefined-variable",
				"Undefined variable 'undefined_name'"
			),
		]);
	}

	#[test]
	fn shellcheck_json_names_numeric_code() {
		let root = Path::new("/private/tmp/offsets-repro/sh");
		let report = include_str!("../../../../fixtures/shellcheck/format-json.json");
		assert_eq!(parse(ParserKind::Shellcheck, &input("shellcheck", root, report)), vec![
			with_severity(
				Severity::Info,
				located(
					"shellcheck",
					"bad.sh",
					[3, 8, 3, 10],
					"SC2086",
					"Double quote to prevent globbing and word splitting."
				)
			),
		]);
	}

	#[test]
	fn golangci_v2_json_reads_issues_pos() {
		let root = Path::new("/private/tmp/offsets-repro/go");
		let stdout = include_str!("../../../../fixtures/golangci/run-output-json-stdout.txt");
		let point = |line, column, code, message| Diagnostic {
			end_line: None,
			end_column: None,
			..located("golangci-lint", "main.go", [line, column, 0, 0], code, message)
		};
		assert_eq!(parse(ParserKind::Golangci, &input("golangci-lint", root, stdout)), vec![
			point(6, 11, "errcheck", "Error return value of `os.Remove` is not checked"),
			point(9, 6, "unused", "func unused is unused"),
		]);
	}

	#[test]
	fn pyright_json_reads_general_diagnostics() {
		let root = Path::new("/private/tmp/empty-result-repro/py");
		let report = include_str!("../../../../fixtures/pyright/outputjson.json");
		// Pyright indents a message's continuation lines with no-break spaces.
		let return_type = "Type \"str\" is not assignable to return type \
		                   \"int\"\n\u{a0}\u{a0}\"str\" is not assignable to \"int\"";
		let revealed = "Type of \"greet\" is \"(name: str) -> int\"";
		assert_eq!(parse(ParserKind::Pyright, &input("pyright", root, report)), vec![
			located("pyright", "bad.py", [2, 12, 2, 16], "reportReturnType", return_type),
			with_severity(Severity::Info, Diagnostic {
				code: None,
				..located("pyright", "bad.py", [5, 13, 5, 18], "", revealed)
			}),
			with_severity(
				Severity::Warning,
				located(
					"pyright",
					"bad.py",
					[6, 1, 6, 7],
					"reportUnusedExpression",
					"Expression value is unused"
				),
			),
			located(
				"pyright",
				"bad.py",
				[7, 7, 7, 21],
				"reportUndefinedVariable",
				"\"undefined_name\" is not defined"
			),
		]);
	}

	#[test]
	fn junit_report_flattens_and_unreadable_report_degrades_to_one_diagnostic() {
		let root = Path::new("/repo");
		let report = include_str!("../../../../fixtures/junit/nextest.xml");
		let input = |stdout| ParserInput {
			checker: "nextest",
			checker_root: root,
			project_root: root,
			stdout,
			stderr: "runner console",
		};
		assert_eq!(parse(ParserKind::JUnit, &input(report)).len(), 2);
		let cut = report
			.find("<testcase name=\"passes\"")
			.expect("passes testcase");
		let truncated = parse(ParserKind::JUnit, &input(&report[..cut]));
		assert_eq!(truncated.len(), 1);
		assert_eq!(truncated[0].file, None);
		assert_eq!(truncated[0].code.as_deref(), Some(UNPARSEABLE));
		assert_eq!(
			truncated[0].message.lines().next(),
			Some("report ends with 2 element(s) unclosed: <?xml version=\"1.0\" encoding=\"UTF-8\"?>")
		);
		let inside_tag = parse(ParserKind::JUnit, &input(&report[..cut + 9]));
		assert_eq!(
			inside_tag[0].message.lines().next(),
			Some(
				format!(
					"malformed XML at byte {cut}: syntax error: tag not closed: `>` not found before \
					 end of input: <?xml version=\"1.0\" encoding=\"UTF-8\"?>"
				)
				.as_str()
			)
		);
	}
}
