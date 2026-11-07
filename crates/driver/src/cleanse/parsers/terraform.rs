//! `terraform validate -json` normalization: one diagnostic per entry.
//!
//! Each rule is pinned by a recorded report under `fixtures/terraform`. The
//! report is on stdout. An entry's text is a `summary` headline and a
//! `detail` paragraph. Its `range` is optional: a missing provider names no
//! file. Lines and columns are one-based and `end` is exclusive.

use std::borrow::Cow;

use omp_core::{Str, sf};

use super::{Diagnostic, ParserInput, Severity, relative_path};

/// Why a `terraform validate -json` report could not be read.
#[derive(Debug, thiserror::Error)]
#[error("malformed `terraform validate -json` report")]
pub struct TerraformError(#[from] serde_json::Error);

#[derive(serde::Deserialize)]
struct Report<'a> {
	#[serde(borrow)]
	diagnostics: Vec<Entry<'a>>,
}

#[derive(serde::Deserialize)]
struct Entry<'a> {
	severity: Severity,
	#[serde(borrow)]
	summary:  Cow<'a, str>,
	#[serde(borrow, default)]
	detail:   Cow<'a, str>,
	#[serde(borrow)]
	range:    Option<Range<'a>>,
}

#[derive(serde::Deserialize)]
struct Range<'a> {
	#[serde(borrow)]
	filename: Cow<'a, str>,
	start:    Position,
	end:      Position,
}

#[derive(serde::Deserialize)]
struct Position {
	line:   u32,
	column: u32,
}

/// Reads the report on stdout.
pub(super) fn parse(input: &ParserInput<'_>) -> Result<Vec<Diagnostic>, TerraformError> {
	let report: Report<'_> = serde_json::from_str(input.stdout)?;
	let checker = Str::from(input.checker);
	Ok(report
		.diagnostics
		.into_iter()
		.map(|entry| {
			let range = entry.range.as_ref();
			Diagnostic {
				checker:    checker.clone(),
				file:       range.and_then(|range| relative_path(&range.filename, input)),
				line:       range.map(|range| range.start.line),
				column:     range.map(|range| range.start.column),
				end_line:   range.map(|range| range.end.line),
				end_column: range.map(|range| range.end.column),
				code:       None,
				severity:   entry.severity,
				message:    if entry.detail.is_empty() {
					Str::from(entry.summary)
				} else {
					sf!("{}\n{}", entry.summary, entry.detail)
				},
				suggestion: None,
			}
		})
		.collect())
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use omp_core::Str;

	use super::super::{
		CheckLogs, Diagnostic, ParseError, ParserInput, ParserKind, Severity, UNPARSEABLE, parse,
		unparseable,
	};

	const REPORT: &str = include_str!("../../../../../fixtures/terraform/validate-json.json");
	const MISSING_PROVIDER: &str =
		include_str!("../../../../../fixtures/terraform/validate-json-missing-provider.json");
	const CLEAN: &str = include_str!("../../../../../fixtures/terraform/validate-json-clean.json");

	fn input<'a>(root: &'a Path, stdout: &'a str) -> ParserInput<'a> {
		ParserInput {
			checker: "terraform-validate",
			checker_root: root,
			project_root: root,
			stdout,
			stderr: "",
		}
	}

	fn entry(span: Option<[u32; 4]>, severity: Severity, message: &str) -> Diagnostic {
		Diagnostic {
			checker: Str::from("terraform-validate"),
			file: span.map(|_| Str::from("main.tf")),
			line: span.map(|[line, ..]| line),
			column: span.map(|[_, column, ..]| column),
			end_line: span.map(|[.., line, _]| line),
			end_column: span.map(|[.., column]| column),
			code: None,
			severity,
			message: Str::from(message),
			suggestion: None,
		}
	}

	#[test]
	fn validate_report_joins_summary_and_detail_at_each_range() {
		let root = Path::new("/private/tmp/empty-result-repro/tf");
		let redundant = "Redundant ignore_changes element\nAdding an attribute name to \
		                 ignore_changes tells Terraform to ignore future changes to the argument in \
		                 configuration after the object has been created, retaining the value \
		                 originally configured.\n\nThe attribute id is decided by the provider \
		                 alone and therefore there can be no configured value to compare with. \
		                 Including this attribute in ignore_changes has no effect. Remove the \
		                 attribute from ignore_changes to quiet this warning.";
		let undeclared = "Reference to undeclared input variable\nAn input variable with the name \
		                  \"missing\" has not been declared. This variable can be declared with a \
		                  variable \"missing\" {} block.";
		let arguments = "Too many function arguments\nFunction \"upper\" expects only 1 argument(s).";
		let read = |input| parse(ParserKind::Terraform, &input).expect("terraform report");
		assert_eq!(read(input(root, REPORT)), vec![
			entry(Some([5, 10, 5, 26]), Severity::Warning, redundant),
			entry(Some([14, 11, 14, 22]), Severity::Error, undeclared),
			entry(Some([18, 27, 18, 28]), Severity::Error, arguments),
		]);
		let clean = Path::new("/private/tmp/empty-result-repro/tf-clean");
		assert!(read(input(clean, CLEAN)).is_empty());
	}

	#[test]
	fn entry_without_range_keeps_its_message_project_wide() {
		let root = Path::new("/private/tmp/empty-result-repro/tf-noinit");
		let missing = "Missing required provider\nThis configuration requires provider \
		               registry.terraform.io/hashicorp/null, but that provider isn't available. You \
		               may be able to install it automatically by running:\n  terraform init";
		let report = parse(ParserKind::Terraform, &input(root, MISSING_PROVIDER));
		assert_eq!(report.expect("terraform report"), vec![entry(None, Severity::Error, missing)]);
	}

	#[test]
	fn unreadable_report_degrades_to_one_unparseable_diagnostic() {
		let root = Path::new("/private/tmp/empty-result-repro/tf");
		let cut = &REPORT[..REPORT.find("\"diagnostics\"").expect("diagnostics key")];
		let error = parse(ParserKind::Terraform, &input(root, cut)).expect_err("cut report");
		assert!(matches!(error, ParseError::Terraform(_)));
		let logs = CheckLogs::of(cut.as_bytes(), b"");
		let diagnostic = unparseable(Str::from("terraform-validate"), &error, Some(&logs));
		assert_eq!(diagnostic.file, None);
		assert_eq!(diagnostic.code.as_deref(), Some(UNPARSEABLE));
		assert!(
			diagnostic
				.message
				.starts_with("malformed `terraform validate -json` report: EOF while parsing")
		);
		// The report is cited whole, by address, and the empty stderr not at all.
		let citation = format!("\n[stdout: artifact://sha256/{}; {} bytes]", logs.stdout, cut.len());
		assert!(diagnostic.message.ends_with(&citation), "{}", diagnostic.message);
		assert!(!diagnostic.message.contains("stderr"));
	}
}
