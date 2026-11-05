//! Stylelint `--formatter=json` normalization: one diagnostic per rule
//! warning and per invalid rule option.
//!
//! Each rule is pinned by a recorded report under `fixtures/stylelint`.
//! Stylelint 16 and later write the report to stderr, on a clean run too; a
//! backend that merges both streams hands it over as stdout. A result names
//! its file once, as `source`. An invalid rule option names no file and
//! repeats on every result, so it becomes one project-wide diagnostic.

use std::borrow::Cow;

use omp_core::Str;

use super::{Diagnostic, ParserInput, Severity, relative_path};

/// Why a stylelint report could not be read.
#[derive(Debug, thiserror::Error)]
#[error("malformed stylelint JSON report")]
pub(super) struct StylelintError(#[from] serde_json::Error);

/// One linted file; `source` is absent when the CSS came from stdin.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LintResult<'a> {
	source:                  Option<Cow<'a, str>>,
	#[serde(borrow)]
	invalid_option_warnings: Vec<InvalidOption<'a>>,
	#[serde(borrow)]
	warnings:                Vec<Warning<'a>>,
}

/// A rule option stylelint rejected, so that rule did not run.
#[derive(serde::Deserialize)]
struct InvalidOption<'a> {
	#[serde(borrow)]
	text: Cow<'a, str>,
}

/// One rule violation; `endLine` and `endColumn` are exclusive.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Warning<'a> {
	line:       u32,
	column:     u32,
	end_line:   Option<u32>,
	end_column: Option<u32>,
	#[serde(borrow)]
	rule:       Cow<'a, str>,
	severity:   Severity,
	#[serde(borrow)]
	text:       Cow<'a, str>,
}

/// Reads the report on stderr, or on stdout when stderr is blank.
pub(super) fn parse(input: &ParserInput<'_>) -> Result<Vec<Diagnostic>, StylelintError> {
	let report = if input.stderr.trim().is_empty() {
		input.stdout
	} else {
		input.stderr
	};
	let results: Vec<LintResult<'_>> = serde_json::from_str(report)?;
	let checker = Str::from(input.checker);
	let mut diagnostics = Vec::new();
	for result in results {
		diagnostics.extend(
			result
				.invalid_option_warnings
				.into_iter()
				.map(|option| Diagnostic {
					checker:    checker.clone(),
					file:       None,
					line:       None,
					column:     None,
					end_line:   None,
					end_column: None,
					code:       None,
					severity:   Severity::Error,
					message:    Str::from(option.text),
					suggestion: None,
				}),
		);
		let file = result
			.source
			.and_then(|source| relative_path(&source, input));
		diagnostics.extend(result.warnings.into_iter().map(|warning| Diagnostic {
			checker:    checker.clone(),
			file:       file.clone(),
			line:       Some(warning.line),
			column:     Some(warning.column),
			end_line:   warning.end_line,
			end_column: warning.end_column,
			code:       Some(Str::from(warning.rule)),
			severity:   warning.severity,
			message:    Str::from(warning.text),
			suggestion: None,
		}));
	}
	Ok(diagnostics)
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use omp_core::Str;

	use super::super::{Diagnostic, ParserInput, ParserKind, Severity, UNPARSEABLE, parse};

	const REPORT: &str = include_str!("../../../../../fixtures/stylelint/format-json.json");
	const CLEAN: &str = include_str!("../../../../../fixtures/stylelint/format-json-clean.json");
	const INVALID_OPTION: &str =
		include_str!("../../../../../fixtures/stylelint/format-json-invalid-option.json");

	fn input<'a>(root: &'a Path, stdout: &'a str, stderr: &'a str) -> ParserInput<'a> {
		ParserInput { checker: "stylelint", checker_root: root, project_root: root, stdout, stderr }
	}

	fn warning(
		file: &str,
		start: [u32; 2],
		end: Option<[u32; 2]>,
		rule: &str,
		severity: Severity,
		text: &str,
	) -> Diagnostic {
		Diagnostic {
			checker: Str::from("stylelint"),
			file: Some(Str::from(file)),
			line: Some(start[0]),
			column: Some(start[1]),
			end_line: end.map(|[line, _]| line),
			end_column: end.map(|[_, column]| column),
			code: Some(Str::from(rule)),
			severity,
			message: Str::from(text),
			suggestion: None,
		}
	}

	#[test]
	fn stderr_report_yields_every_warning_at_its_source() {
		let root = Path::new("/private/tmp/empty-result-repro/css");
		let unknown = "Unknown property \"colr\" (property-no-unknown)";
		let expected = vec![
			warning(
				"bad.css",
				[2, 10],
				Some([2, 13]),
				"color-no-invalid-hex",
				Severity::Error,
				"Invalid hex color \"#ff\" (color-no-invalid-hex)",
			),
			warning("bad.css", [6, 3], Some([6, 7]), "property-no-unknown", Severity::Error, unknown),
			warning(
				"bad.css",
				[9, 3],
				Some([9, 5]),
				"block-no-empty",
				Severity::Warning,
				"Empty block (block-no-empty)",
			),
			warning(
				"broken.css",
				[1, 1],
				None,
				"CssSyntaxError",
				Severity::Error,
				"Unclosed block (CssSyntaxError)",
			),
		];
		assert_eq!(parse(ParserKind::Stylelint, &input(root, "", REPORT)), expected);
		assert_eq!(parse(ParserKind::Stylelint, &input(root, REPORT, "")), expected);
		let clean = Path::new("/private/tmp/empty-result-repro/css-clean");
		assert!(parse(ParserKind::Stylelint, &input(clean, "", CLEAN)).is_empty());
	}

	#[test]
	fn invalid_rule_option_is_one_project_wide_error() {
		let root = Path::new("/private/tmp/empty-result-repro/css-options");
		let option = "Unexpected option value \"maybe\" for rule \"color-no-invalid-hex\"";
		let unknown = "Unknown property \"colr\" (property-no-unknown)";
		assert_eq!(parse(ParserKind::Stylelint, &input(root, "", INVALID_OPTION)), vec![
			Diagnostic {
				file: None,
				line: None,
				column: None,
				code: None,
				..warning("", [0, 0], None, "", Severity::Error, option)
			},
			warning("bad.css", [6, 3], Some([6, 7]), "property-no-unknown", Severity::Error, unknown),
		]);
	}

	#[test]
	fn unreadable_report_degrades_to_one_unparseable_diagnostic() {
		let root = Path::new("/private/tmp/empty-result-repro/css");
		let cut = &REPORT[..REPORT.find("\"warnings\"").expect("first warnings key")];
		let diagnostics = parse(ParserKind::Stylelint, &input(root, "", cut));
		assert_eq!(diagnostics.len(), 1);
		assert_eq!(diagnostics[0].file, None);
		assert_eq!(diagnostics[0].code.as_deref(), Some(UNPARSEABLE));
		assert!(
			diagnostics[0]
				.message
				.starts_with("malformed stylelint JSON report: EOF while parsing")
		);
		assert!(diagnostics[0].message.ends_with(cut));
	}
}
