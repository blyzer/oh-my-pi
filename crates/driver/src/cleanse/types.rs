//! Cleanse checker, diagnostic, scheduling, and command records.

use std::{fmt::Write as _, path::PathBuf};

use omp_core::Str;
use omp_journal::blob::BlobRef;

use super::parsers::{ParseError, ParserKind, unparseable};

/// Code of the diagnostic a checker that failed without reporting one
/// degrades to.
const CHECKER_FAILED: &str = "checker-failed";

/// Severity normalized across checker formats.
#[derive(
	Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
	/// Informational diagnostic.
	#[default]
	Info,
	/// Warning diagnostic.
	Warning,
	/// Error diagnostic.
	Error,
}

/// One normalized actionable checker result.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Diagnostic {
	/// Checker identity.
	pub checker:    Str,
	/// Project-relative path.
	pub file:       Option<Str>,
	/// One-based start line.
	pub line:       Option<u32>,
	/// One-based start column.
	pub column:     Option<u32>,
	/// One-based inclusive end line.
	pub end_line:   Option<u32>,
	/// One-based exclusive end column: one past the last character, as checkers
	/// report it.
	pub end_column: Option<u32>,
	/// Checker-specific rule code.
	pub code:       Option<Str>,
	/// Normalized severity.
	pub severity:   Severity,
	/// Human-readable problem.
	pub message:    Str,
	/// Optional machine-provided fix suggestion.
	pub suggestion: Option<Str>,
}

/// Whether a checker may mutate the tree while running.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CheckerEffect {
	/// Safe to execute concurrently.
	#[default]
	ReadOnly,
	/// Must execute serially after read-only checkers.
	Mutating,
}

/// One discovered checker invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checker {
	/// Stable picker id.
	pub id:       Str,
	/// Display label.
	pub label:    Str,
	/// Language family.
	pub language: Str,
	/// Manifest root.
	pub cwd:      PathBuf,
	/// Resolved executable.
	pub binary:   PathBuf,
	/// Arguments excluding the executable.
	pub args:     Vec<Str>,
	/// Output parser.
	pub parser:   ParserKind,
	/// Execution effect.
	pub effect:   CheckerEffect,
	/// Whether this is a project test suite.
	pub test:     bool,
}

/// One checker run's complete standard streams, retained in the project blob
/// store that `artifact://sha256/<hex>` addresses resolve from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckLogs {
	/// Standard output.
	pub stdout: BlobRef,
	/// Standard error.
	pub stderr: BlobRef,
}

impl CheckLogs {
	/// Appends, for each non-empty stream, the line naming where its complete
	/// bytes are retained: `[<stream>: artifact://sha256/<hex>; <n> bytes]`.
	pub(super) fn cite(&self, message: &mut String) {
		for (stream, blob) in [("stdout", self.stdout), ("stderr", self.stderr)] {
			if blob.size != 0 {
				let _ = write!(message, "\n[{stream}: artifact://sha256/{blob}; {} bytes]", blob.size);
			}
		}
	}
}

#[cfg(test)]
impl CheckLogs {
	/// The references `BlobStore::put` returns for these streams.
	pub(super) fn of(stdout: &[u8], stderr: &[u8]) -> Self {
		let blob =
			|bytes: &[u8]| BlobRef { hash: omp_core::Hash32::sum(bytes), size: bytes.len() as u64 };
		Self { stdout: blob(stdout), stderr: blob(stderr) }
	}
}

/// Complete output of one checker.
#[derive(Debug)]
pub struct CheckResult {
	/// Checker metadata.
	pub checker:   Checker,
	/// Process exit code; `None` when the process ended without one.
	pub exit_code: Option<i32>,
	/// Every diagnostic the output reports, or why the output could not be
	/// read.
	pub parsed:    Result<Vec<Diagnostic>, ParseError>,
	/// The complete output, when the runner retained it.
	pub logs:      Option<CheckLogs>,
}

impl CheckResult {
	/// Actionable diagnostics: those the output reports, or one project-wide
	/// diagnostic when the run established no result, because its output could
	/// not be read or the checker failed without reporting a diagnostic.
	pub fn diagnostics(&self) -> Vec<Diagnostic> {
		let reported = match &self.parsed {
			Ok(reported) => reported,
			Err(error) => {
				return vec![unparseable(self.checker.id.clone(), error, self.logs.as_ref())];
			},
		};
		if !reported.is_empty() || self.exit_code == Some(0) {
			return reported.clone();
		}
		let mut message = match self.exit_code {
			Some(code) => format!("Checker exited with status {code} without reporting a diagnostic"),
			None => String::from("Checker ended without an exit status and reported no diagnostic"),
		};
		if let Some(logs) = &self.logs {
			logs.cite(&mut message);
		}
		vec![Diagnostic {
			checker:    self.checker.id.clone(),
			file:       None,
			line:       None,
			column:     None,
			end_line:   None,
			end_column: None,
			code:       Some(Str::new_static(CHECKER_FAILED)),
			severity:   Severity::Error,
			message:    Str::from(message),
			suggestion: None,
		}]
	}
}

/// Discovered checker omitted because its executable was unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkippedCheck {
	/// Display label.
	pub label:    Str,
	/// Language family.
	pub language: Str,
	/// Omission reason.
	pub reason:   Str,
}

/// Aggregate checker pass.
#[derive(Debug, Default)]
pub struct Report {
	/// Per-checker outcomes.
	pub checks:      Vec<CheckResult>,
	/// Flattened actionable diagnostics of every check.
	pub diagnostics: Vec<Diagnostic>,
	/// Unavailable checkers.
	pub skipped:     Vec<SkippedCheck>,
}

/// Every issue attached to one file; scheduling never splits this group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIssues {
	/// Project-relative file, absent for project-wide failures.
	pub file:        Option<Str>,
	/// Complete diagnostics for the file.
	pub diagnostics: Vec<Diagnostic>,
	/// Severity/detail weight.
	pub weight:      u64,
}

/// File-disjoint assignment for one continuously scheduled repair child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assignment {
	/// Stable zero-based assignment index.
	pub index:  usize,
	/// Whole-file issue groups.
	pub groups: Vec<FileIssues>,
	/// Total scheduling weight.
	pub weight: u64,
}

/// User-facing cleanse options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanseArgs {
	/// Maximum file-disjoint repair agents. Default: 32.
	pub agents:  usize,
	/// Repair/discovery model selector. Default: `@smol`.
	pub model:   Str,
	/// Include configured project tests.
	pub tests:   bool,
	/// Run every discovered checker without a picker.
	pub all:     bool,
	/// Free-form checker request.
	pub request: Option<Str>,
}

impl Default for CleanseArgs {
	fn default() -> Self {
		Self { agents: 32, model: "@smol".into(), tests: false, all: false, request: None }
	}
}

/// Interactive picker result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetChoice {
	/// Run every checker.
	All,
	/// Run one checker id.
	Checker(Str),
	/// Use model-driven checker discovery.
	Request(Str),
	/// User cancelled.
	Cancel,
}

/// Observable cleanse completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanseStatus {
	/// No diagnostics remain.
	Clean,
	/// Verification still reports diagnostics.
	Unresolved,
	/// No runnable checker exists.
	Unsupported,
	/// Signal or picker cancellation.
	Cancelled,
}

/// CLI exit and final report.
#[derive(Debug)]
pub struct CleanseExit {
	/// Exactly 0, 1, or 130.
	pub code:          u8,
	/// Terminal status.
	pub status:        CleanseStatus,
	/// Verification report.
	pub report:        Report,
	/// At most 50 remaining file groups for display.
	pub remainder:     Vec<FileIssues>,
	/// Number of additional file groups omitted from display.
	pub omitted_files: usize,
}
