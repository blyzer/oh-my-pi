//! Normalization for checker output formats.

mod cargo;
mod junit;
mod sarif;
mod stylelint;
mod tap;
mod terraform;

use std::{error::Error as _, fmt::Write as _, path::Path, sync::LazyLock};

use omp_core::{Str, sf};
use regex::Regex;

pub use self::{
	junit::JunitError, sarif::SarifError, stylelint::StylelintError, tap::TapError,
	terraform::TerraformError,
};
use super::types::{CheckLogs, Diagnostic, Severity};

/// Code of the diagnostic unreadable checker output degrades to.
const UNPARSEABLE: &str = "unparseable";

/// Why a checker's output is not a readable instance of its declared format.
///
/// Each reason stays typed through the parse seam; [`unparseable`] renders it
/// once, into the diagnostic a repair worker reads.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
	/// cargo stopped before it finished a build: stdout holds no
	/// `build-finished` record, and no output line names a diagnostic.
	#[error("cargo wrote no `build-finished` record and no diagnostic")]
	CargoUnfinished,
	/// A checker that reports JSON wrote no JSON value and no diagnostic line;
	/// the source is why the whole output is not JSON.
	#[error("output holds no JSON report")]
	NoJson(#[source] serde_json::Error),
	/// The `JUnit` XML report could not be read.
	#[error(transparent)]
	Junit(#[from] JunitError),
	/// The SARIF log could not be read.
	#[error(transparent)]
	Sarif(#[from] SarifError),
	/// The TAP stream could not be read.
	#[error(transparent)]
	Tap(#[from] TapError),
	/// The Stylelint report could not be read.
	#[error(transparent)]
	Stylelint(#[from] StylelintError),
	/// The `terraform validate -json` report could not be read.
	#[error(transparent)]
	Terraform(#[from] TerraformError),
}

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
	/// Actionlint JSON (`-format '{{json .}}'`).
	Actionlint,
	/// SARIF 2.1.0 log on stdout.
	Sarif,
	/// TAP stream on stdout: TAP 14, TAP 13, or version-less TAP 12.
	Tap,
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
///
/// `Ok` holds every diagnostic the output reports, and is empty exactly when
/// the output is a report that names no problem. `Err` is output that is not
/// a readable instance of the declared format, so it never reads as a clean
/// run. A text format has no unreadable state: each line names a problem or
/// does not, and the process exit status says what an empty result means.
pub fn parse(kind: ParserKind, input: &ParserInput<'_>) -> Result<Vec<Diagnostic>, ParseError> {
	let mut diagnostics = match kind {
		ParserKind::Rust | ParserKind::RustTest => cargo::parse(kind, input)?,
		ParserKind::JUnit => junit::parse(input)?,
		ParserKind::Sarif => sarif::parse(input)?,
		ParserKind::Tap => tap::parse(input)?,
		ParserKind::Stylelint => stylelint::parse(input)?,
		ParserKind::Terraform => terraform::parse(input)?,
		ParserKind::GoTest
		| ParserKind::Golangci
		| ParserKind::Ruff
		| ParserKind::Pyright
		| ParserKind::Pylint
		| ParserKind::Eslint
		| ParserKind::Biome
		| ParserKind::DenoLint
		| ParserKind::Rubocop
		| ParserKind::Phpstan
		| ParserKind::Psalm
		| ParserKind::Swiftlint
		| ParserKind::Credo
		| ParserKind::Shellcheck
		| ParserKind::Hlint
		| ParserKind::Tflint
		| ParserKind::Actionlint => match walk(kind, input) {
			(diagnostics, Some(error)) if diagnostics.is_empty() => {
				return Err(ParseError::NoJson(error));
			},
			(diagnostics, _) => diagnostics,
		},
		ParserKind::Go
		| ParserKind::Staticcheck
		| ParserKind::Mypy
		| ParserKind::Flake8
		| ParserKind::Ty
		| ParserKind::Oxlint
		| ParserKind::Dart
		| ParserKind::Generic => walk(kind, input).0,
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
	Ok(diagnostics)
}

/// Reads every JSON value and every diagnostic text line of the combined
/// output, and returns why the output is not JSON when it holds no JSON value.
fn walk(kind: ParserKind, input: &ParserInput<'_>) -> (Vec<Diagnostic>, Option<serde_json::Error>) {
	let combined = if input.stderr.is_empty() {
		input.stdout.to_owned()
	} else if input.stdout.is_empty() {
		input.stderr.to_owned()
	} else {
		format!("{}\n{}", input.stdout, input.stderr)
	};
	let mut diagnostics = Vec::new();
	let not_json = match json_values(&combined) {
		Ok(values) => {
			for value in &values {
				parse_json_value(kind, value, None, input, &mut diagnostics);
			}
			None
		},
		Err(error) => Some(error),
	};
	parse_text(kind, &combined, input, &mut diagnostics);
	(diagnostics, not_json)
}

/// The one project-wide diagnostic unreadable output degrades to: the reason,
/// rendered once with its causes, then where the complete output is retained.
/// The diagnostic never carries the output itself, so none of it is cut.
pub(super) fn unparseable(
	checker: Str,
	error: &ParseError,
	logs: Option<&CheckLogs>,
) -> Diagnostic {
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
	if let Some(logs) = logs {
		logs.cite(&mut message);
	}
	Diagnostic {
		checker,
		file: None,
		line: None,
		column: None,
		end_line: None,
		end_column: None,
		code: Some(sf!(UNPARSEABLE)),
		severity: Severity::Error,
		message: Str::from(message),
		suggestion: None,
	}
}

/// Every JSON value in `text`: the whole text as one document, else each line
/// that holds one. `Err` is why the whole text is not JSON, when no line is.
fn json_values(text: &str) -> Result<Vec<serde_json::Value>, serde_json::Error> {
	let error = match serde_json::from_str(text.trim()) {
		Ok(value) => return Ok(vec![value]),
		Err(error) => error,
	};
	let mut values = Vec::new();
	for line in text.lines() {
		let line = line.trim();
		if (line.starts_with('{') || line.starts_with('['))
			&& let Ok(value) = serde_json::from_str(line)
		{
			values.push(value);
		}
	}
	if values.is_empty() {
		Err(error)
	} else {
		Ok(values)
	}
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
			for key in [
				"diagnostics",
				"messages",
				"errors",
				"issues",
				"files",
				"results",
				"runs",
				"Issues",
				"generalDiagnostics",
			] {
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

	fn read(kind: ParserKind, input: &ParserInput<'_>) -> Vec<Diagnostic> {
		parse(kind, input).expect("readable report")
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
		assert_eq!(read(ParserKind::Ruff, &input), vec![
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
		assert_eq!(read(ParserKind::Eslint, &input(report)), expected);
		let unlocated =
			report.replacen(r#""filePath":"/private/tmp/walker-repro/js/bad.js","#, "", 1);
		let without_file = expected
			.into_iter()
			.map(|diagnostic| Diagnostic { file: None, ..diagnostic });
		assert_eq!(
			read(ParserKind::Eslint, &input(unlocated.as_str())),
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
		assert_eq!(read(ParserKind::Biome, &input("biome", root, report)), vec![
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
		assert_eq!(read(ParserKind::DenoLint, &input("deno-lint", root, report)), vec![
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
		assert_eq!(read(ParserKind::Pylint, &input("pylint", root, report)), vec![
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
		assert_eq!(read(ParserKind::Shellcheck, &input("shellcheck", root, report)), vec![
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
		assert_eq!(read(ParserKind::Golangci, &input("golangci-lint", root, stdout)), vec![
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
		assert_eq!(read(ParserKind::Pyright, &input("pyright", root, report)), vec![
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
		assert_eq!(read(ParserKind::JUnit, &input(report)).len(), 2);
		let cut = report
			.find("<testcase name=\"passes\"")
			.expect("passes testcase");
		let error = parse(ParserKind::JUnit, &input(&report[..cut])).expect_err("cut report");
		assert!(matches!(error, ParseError::Junit(JunitError::Truncated { open: 2 })));
		let truncated = unparseable(sf!("nextest"), &error, None);
		assert_eq!(truncated.file, None);
		assert_eq!(truncated.code.as_deref(), Some(UNPARSEABLE));
		assert_eq!(truncated.message.as_str(), "report ends with 2 element(s) unclosed");
		let inside_tag =
			parse(ParserKind::JUnit, &input(&report[..cut + 9])).expect_err("cut inside a tag");
		assert_eq!(
			unparseable(sf!("nextest"), &inside_tag, None)
				.message
				.as_str(),
			format!(
				"malformed XML at byte {cut}: syntax error: tag not closed: `>` not found before end \
				 of input"
			)
		);
	}

	/// The shared walk reads nothing from text that names no problem, as it does
	/// for a clean run, so for a text format the exit status decides. SARIF and
	/// TAP keep the distinction at the seam: rejected output is an `Err` naming
	/// why, which degrades to one file-less diagnostic.
	#[test]
	fn sarif_and_tap_never_read_rejected_output_as_clean() {
		let root = Path::new("/repo");
		let input = |stdout, stderr| ParserInput {
			checker: "checker",
			checker_root: root,
			project_root: root,
			stdout,
			stderr,
		};
		let rows = |kind, input: &ParserInput<'_>| {
			let error = parse(kind, input).expect_err("rejected output");
			let diagnostic = unparseable(sf!("checker"), &error, None);
			vec![(diagnostic.file, diagnostic.code, diagnostic.message.as_str().to_owned())]
		};
		let rejected =
			|message: &str| vec![(None::<Str>, Some(sf!(UNPARSEABLE)), message.to_owned())];
		let garbage = input("checker crashed before writing its log", "");
		assert!(read(ParserKind::Generic, &garbage).is_empty());
		assert_eq!(
			rows(ParserKind::Sarif, &garbage),
			rejected("stdout is not a SARIF 2.1.0 log: expected value at line 1 column 1")
		);
		assert_eq!(
			rows(ParserKind::Tap, &garbage),
			rejected("stdout holds no TAP stream (no version line, plan, test point or Bail out!)")
		);
		assert_eq!(
			rows(ParserKind::Sarif, &input("", "error: unknown flag --sarif")),
			rejected("stdout is empty; expected a SARIF 2.1.0 log")
		);
		let clean_sarif =
			r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint"}},"results":[]}]}"#;
		assert!(read(ParserKind::Sarif, &input(clean_sarif, "")).is_empty());
		assert!(
			read(ParserKind::Tap, &input("TAP version 14\n1..1\nok 1 - passes\n", "")).is_empty()
		);
	}

	/// Recorded runs under `fixtures` at the seam. A clean run parses to an
	/// empty `Ok` and a report to its failures; the same output cut short, as a
	/// checker killed mid-write leaves it, is an `Err` naming why. Output that
	/// could not be read never looks like a run with zero failures.
	#[test]
	fn could_not_parse_is_distinguishable_from_zero_failures_at_the_seam() {
		let root = Path::new("/private/tmp/cleanse-seam");
		let run = |kind, stdout, stderr| {
			parse(kind, &ParserInput {
				checker: "checker",
				checker_root: root,
				project_root: root,
				stdout,
				stderr,
			})
		};
		let half = |output: &'static str| {
			let middle = output.len() / 2;
			&output[..(0..=middle)
				.rev()
				.find(|&cut| output.is_char_boundary(cut))
				.unwrap_or(0)]
		};
		let cut = |kind, stdout, stderr| {
			run(kind, half(stdout), half(stderr)).expect_err("output cut short")
		};

		for (kind, stdout, stderr) in [
			(
				ParserKind::Rust,
				include_str!("../../../../fixtures/cargo/check-clean.stdout"),
				include_str!("../../../../fixtures/cargo/check-clean.stderr"),
			),
			(
				ParserKind::Rust,
				include_str!("../../../../fixtures/cargo/clippy-clean.stdout"),
				include_str!("../../../../fixtures/cargo/clippy-clean.stderr"),
			),
			(
				ParserKind::RustTest,
				include_str!("../../../../fixtures/cargo/test-clean.stdout"),
				include_str!("../../../../fixtures/cargo/test-clean.stderr"),
			),
		] {
			assert!(
				run(kind, stdout, stderr)
					.expect("clean cargo run")
					.is_empty(),
				"{kind:?}"
			);
			assert!(matches!(cut(kind, stdout, stderr), ParseError::CargoUnfinished));
		}
		let stylelint = include_str!("../../../../fixtures/stylelint/format-json-clean.json");
		assert!(
			run(ParserKind::Stylelint, "", stylelint)
				.expect("clean run")
				.is_empty()
		);
		assert!(matches!(cut(ParserKind::Stylelint, "", stylelint), ParseError::Stylelint(_)));
		let terraform = include_str!("../../../../fixtures/terraform/validate-json-clean.json");
		assert!(
			run(ParserKind::Terraform, terraform, "")
				.expect("clean run")
				.is_empty()
		);
		assert!(matches!(cut(ParserKind::Terraform, terraform, ""), ParseError::Terraform(_)));

		for (kind, report) in [
			(ParserKind::Ruff, include_str!("../../../../fixtures/ruff/check.json")),
			(ParserKind::Eslint, include_str!("../../../../fixtures/eslint/format-json.json")),
			(ParserKind::Pyright, include_str!("../../../../fixtures/pyright/outputjson.json")),
			(ParserKind::Biome, include_str!("../../../../fixtures/biome/check-reporter-json.json")),
			(ParserKind::DenoLint, include_str!("../../../../fixtures/deno/lint-json.json")),
			(ParserKind::Pylint, include_str!("../../../../fixtures/pylint/output-format-json.json")),
			(ParserKind::Shellcheck, include_str!("../../../../fixtures/shellcheck/format-json.json")),
			(
				ParserKind::Golangci,
				include_str!("../../../../fixtures/golangci/run-output-json-stdout.txt"),
			),
		] {
			assert!(!run(kind, report, "").expect("recorded report").is_empty(), "{kind:?}");
			assert!(matches!(cut(kind, report, ""), ParseError::NoJson(_)), "{kind:?}");
		}
		let junit = include_str!("../../../../fixtures/junit/nextest.xml");
		assert!(
			!run(ParserKind::JUnit, junit, "")
				.expect("recorded report")
				.is_empty()
		);
		assert!(matches!(cut(ParserKind::JUnit, junit, ""), ParseError::Junit(_)));
		let sarif = include_str!("../../../../fixtures/sarif/ruff.sarif");
		assert!(
			!run(ParserKind::Sarif, sarif, "")
				.expect("recorded log")
				.is_empty()
		);
		assert!(matches!(cut(ParserKind::Sarif, sarif, ""), ParseError::Sarif(_)));
		// Test::More's diagnostics alone, without the TAP stream they annotate.
		let perl = include_str!("../../../../fixtures/tap/perl.stderr");
		assert!(matches!(
			run(ParserKind::Tap, perl, "").expect_err("no TAP stream"),
			ParseError::Tap(TapError::NoStream)
		));
	}
}
