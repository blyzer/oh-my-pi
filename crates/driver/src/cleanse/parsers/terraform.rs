//! `terraform validate -json` normalization: one diagnostic per entry.
//!
//! Each rule is pinned by a recorded report under `fixtures/terraform`. The
//! report is on stdout. An entry's text is a `summary` headline and a
//! `detail` paragraph. Its `range` is optional: a missing provider names no
//! file. Lines and columns are one-based and `end` is exclusive.

#[cfg(test)]
mod tests {
	use std::path::Path;

	use omp_core::Str;

	use super::super::{Diagnostic, ParserInput, ParserKind, Severity, UNPARSEABLE, parse};

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
		assert_eq!(parse(ParserKind::Terraform, &input(root, REPORT)), vec![
			entry(Some([5, 10, 5, 26]), Severity::Warning, redundant),
			entry(Some([14, 11, 14, 22]), Severity::Error, undeclared),
			entry(Some([18, 27, 18, 28]), Severity::Error, arguments),
		]);
		let clean = Path::new("/private/tmp/empty-result-repro/tf-clean");
		assert!(parse(ParserKind::Terraform, &input(clean, CLEAN)).is_empty());
	}

	#[test]
	fn entry_without_range_keeps_its_message_project_wide() {
		let root = Path::new("/private/tmp/empty-result-repro/tf-noinit");
		let missing = "Missing required provider\nThis configuration requires provider \
		               registry.terraform.io/hashicorp/null, but that provider isn't available. You \
		               may be able to install it automatically by running:\n  terraform init";
		assert_eq!(parse(ParserKind::Terraform, &input(root, MISSING_PROVIDER)), vec![entry(
			None,
			Severity::Error,
			missing
		)]);
	}

	#[test]
	fn unreadable_report_degrades_to_one_unparseable_diagnostic() {
		let root = Path::new("/private/tmp/empty-result-repro/tf");
		let cut = &REPORT[..REPORT.find("\"diagnostics\"").expect("diagnostics key")];
		let diagnostics = parse(ParserKind::Terraform, &input(root, cut));
		assert_eq!(diagnostics.len(), 1);
		assert_eq!(diagnostics[0].file, None);
		assert_eq!(diagnostics[0].code.as_deref(), Some(UNPARSEABLE));
		assert!(
			diagnostics[0]
				.message
				.starts_with("malformed `terraform validate -json` report: EOF while parsing")
		);
		assert!(diagnostics[0].message.ends_with(cut.trim()));
	}
}
