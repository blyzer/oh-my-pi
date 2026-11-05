//! cargo `--message-format=json` normalization for `cargo check`, `cargo
//! clippy` and `cargo test`.
//!
//! cargo writes one JSON record per stdout line and names its kind in
//! `reason`. Only a `compiler-message` carries a diagnostic, rustc's, under
//! `message`; every other record is stream metadata. rustc's `failure-note`
//! level ("For more information about this error...") closes a failed build
//! and is not a problem. The rest is text: cargo's own errors on stderr, a
//! failing build script's output, and the harness output `cargo test`
//! interleaves on stdout. A panic is read at the location its header names;
//! the shared text parser reads everything else, never a record.
//!
//! cargo ends every build it finishes with a `build-finished` record. Output
//! that names no diagnostic reads as clean only beside that record; without
//! it, cargo stopped before finishing a build, so the output is unreadable.
//!
//! Each rule is pinned by a recorded run under `fixtures/cargo`.

use std::{borrow::Cow, fmt::Write as _, sync::LazyLock};

use omp_core::{Str, StrMut};
use regex::Regex;
use smallvec::SmallVec;

use super::{Diagnostic, ParseError, ParserInput, ParserKind, Severity, parse_text, relative_path};

/// `thread '<name>' (<id>) panicked at <file>:<line>:<column>:`, the line std
/// prints above a panic message. Older toolchains print no `(<id>)`.
static PANIC: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(
		r"^thread '(?P<thread>[^']*)'(?: \(\d+\))? panicked at (?P<file>.+):(?P<line>\d+):(?P<column>\d+):$",
	)
	.expect("static panic header regex")
});

/// Normalizes one cargo invocation's stdout and stderr.
pub(super) fn parse(
	kind: ParserKind,
	input: &ParserInput<'_>,
) -> Result<Vec<Diagnostic>, ParseError> {
	let mut diagnostics = Vec::new();
	let mut finished = false;
	for stream in [input.stdout, input.stderr] {
		finished |= read(kind, stream, input, &mut diagnostics);
	}
	if finished || !diagnostics.is_empty() {
		Ok(diagnostics)
	} else {
		Err(ParseError::CargoUnfinished)
	}
}

/// Reads records and panics line by line, and hands each run of text between
/// them to the shared text parser whole, so an `error[...]` line still pairs
/// with the `-->` location under it. Returns whether the stream holds a
/// `build-finished` record.
fn read(
	kind: ParserKind,
	stream: &str,
	input: &ParserInput<'_>,
	output: &mut Vec<Diagnostic>,
) -> bool {
	let mut finished = false;
	let mut lines = stream.split_inclusive('\n').peekable();
	let (mut offset, mut text) = (0, 0);
	while let Some(raw) = lines.next() {
		let start = offset;
		offset += raw.len();
		let line = raw.trim();
		let diagnostic = if line.starts_with('{')
			&& let Ok(record) = serde_json::from_str::<Record<'_>>(line)
		{
			finished |= matches!(record.reason, Reason::BuildFinished);
			record.diagnostic(input)
		} else if line.starts_with("thread '")
			&& let Some(header) = PANIC.captures(line)
		{
			let indent = &raw[..raw.len() - raw.trim_start().len()];
			let mut message = StrMut::with_capacity(raw.len());
			let _ = write!(message, "thread '{}' panicked", &header["thread"]);
			let mut separator = ": ";
			while let Some(next) = lines.next_if(|next| !ends_panic(next.trim())) {
				offset += next.len();
				message.push_str(separator);
				message.push_str(next.strip_prefix(indent).unwrap_or(next).trim_end());
				separator = "\n";
			}
			Some(Diagnostic {
				checker:    Str::from(input.checker),
				file:       relative_path(&header["file"], input),
				line:       header["line"].parse().ok(),
				column:     header["column"].parse().ok(),
				end_line:   None,
				end_column: None,
				code:       None,
				severity:   Severity::Error,
				message:    message.freeze(),
				suggestion: None,
			})
		} else {
			continue;
		};
		if text < start {
			parse_text(kind, &stream[text..start], input, output);
		}
		output.extend(diagnostic);
		text = offset;
	}
	if text < stream.len() {
		parse_text(kind, &stream[text..], input, output);
	}
	finished
}

/// Whether a line ends the message under a panic header.
fn ends_panic(line: &str) -> bool {
	line.is_empty()
		|| line == "stack backtrace:"
		|| line.starts_with("note: run with `RUST_BACKTRACE=")
}

/// One cargo record. Only a `compiler-message` has a `message`.
#[derive(serde::Deserialize)]
struct Record<'a> {
	reason:  Reason,
	#[serde(borrow)]
	message: Option<Rustc<'a>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Reason {
	CompilerMessage,
	BuildFinished,
	#[serde(other)]
	Metadata,
}

/// rustc's JSON diagnostic; the fields this parser reads.
#[derive(serde::Deserialize)]
struct Rustc<'a> {
	#[serde(borrow)]
	message:  Cow<'a, str>,
	#[serde(borrow)]
	code:     Option<ErrorCode<'a>>,
	level:    Level,
	#[serde(borrow)]
	spans:    SmallVec<Span<'a>, 2>,
	#[serde(borrow)]
	children: SmallVec<Child<'a>, 4>,
}

#[derive(serde::Deserialize)]
struct ErrorCode<'a> {
	#[serde(borrow)]
	code: Cow<'a, str>,
}

/// One-based lines and columns; `column_end` is exclusive.
#[derive(serde::Deserialize)]
struct Span<'a> {
	#[serde(borrow)]
	file_name:    Cow<'a, str>,
	line_start:   u32,
	line_end:     u32,
	column_start: u32,
	column_end:   u32,
	is_primary:   bool,
}

#[derive(serde::Deserialize)]
struct Child<'a> {
	#[serde(borrow)]
	message: Cow<'a, str>,
	level:   Level,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Level {
	Error,
	#[serde(rename = "error: internal compiler error")]
	InternalCompilerError,
	Warning,
	Note,
	Help,
	FailureNote,
	#[serde(other)]
	Other,
}

impl Record<'_> {
	/// The diagnostic a `compiler-message` carries, at its primary span: rustc
	/// lists secondary spans (the move before a use after move) ahead of it.
	fn diagnostic(&self, input: &ParserInput<'_>) -> Option<Diagnostic> {
		let Self { reason: Reason::CompilerMessage, message: Some(rustc) } = self else {
			return None;
		};
		let severity = match rustc.level {
			Level::Error | Level::InternalCompilerError => Severity::Error,
			Level::Warning => Severity::Warning,
			Level::FailureNote => return None,
			Level::Note | Level::Help | Level::Other => Severity::Info,
		};
		let span = rustc.spans.iter().find(|span| span.is_primary);
		Some(Diagnostic {
			checker: Str::from(input.checker),
			file: span.and_then(|span| relative_path(&span.file_name, input)),
			line: span.map(|span| span.line_start),
			column: span.map(|span| span.column_start),
			end_line: span.map(|span| span.line_end),
			end_column: span.map(|span| span.column_end),
			code: rustc.code.as_ref().map(|code| Str::from(&*code.code)),
			severity,
			message: Str::from(&*rustc.message),
			suggestion: rustc
				.children
				.iter()
				.find(|child| matches!(child.level, Level::Help | Level::Note))
				.map(|child| Str::from(&*child.message)),
		})
	}
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use omp_core::Str;

	use super::super::{Diagnostic, ParserInput, ParserKind, Severity, parse};

	macro_rules! run {
		($case:literal) => {
			[
				include_str!(concat!("../../../../../fixtures/cargo/", $case, ".stdout")),
				include_str!(concat!("../../../../../fixtures/cargo/", $case, ".stderr")),
			]
		};
	}

	fn normalize(
		kind: ParserKind,
		checker: &str,
		krate: &str,
		[stdout, stderr]: [&str; 2],
	) -> Vec<Diagnostic> {
		let root = Path::new("/private/tmp/cargo-json-repro").join(krate);
		parse(kind, &ParserInput {
			checker,
			checker_root: &root,
			project_root: &root,
			stdout,
			stderr,
		})
		.expect("readable cargo output")
	}

	fn error(
		checker: &str,
		file: &str,
		[line, column]: [u32; 2],
		code: Option<&str>,
		message: &str,
	) -> Diagnostic {
		Diagnostic {
			checker:    Str::from(checker),
			file:       Some(Str::from(file)),
			line:       Some(line),
			column:     Some(column),
			end_line:   None,
			end_column: None,
			code:       code.map(Str::from),
			severity:   Severity::Error,
			message:    Str::from(message),
			suggestion: None,
		}
	}

	#[test]
	fn clean_runs_have_no_diagnostics() {
		assert_eq!(
			[
				normalize(ParserKind::Rust, "cargo-check", "clean", run!("check-clean")),
				normalize(ParserKind::Rust, "cargo-clippy", "clean", run!("clippy-clean")),
				normalize(ParserKind::RustTest, "cargo-test", "clean", run!("test-clean")),
			],
			[Vec::<Diagnostic>::new(), Vec::new(), Vec::new()]
		);
	}

	#[test]
	fn compiler_errors_keep_the_primary_span_code_and_level() {
		let expected = |checker: &str| {
			vec![
				Diagnostic {
					end_line: Some(6),
					end_column: Some(13),
					..error(checker, "src/lib.rs", [6, 2], Some("E0308"), "mismatched types")
				},
				Diagnostic {
					end_line: Some(4),
					end_column: Some(6),
					suggestion: Some(Str::from(
						"consider cloning the value if the performance cost is acceptable",
					)),
					..error(
						checker,
						"src/moved.rs",
						[4, 2],
						Some("E0382"),
						"borrow of moved value: `text`",
					)
				},
				Diagnostic {
					end_line: Some(2),
					end_column: Some(17),
					..error(
						checker,
						"src/util.rs",
						[2, 2],
						Some("E0425"),
						"cannot find value `undefined_value` in this scope",
					)
				},
			]
		};
		assert_eq!(
			[
				normalize(ParserKind::Rust, "cargo-check", "error", run!("check-error")),
				normalize(ParserKind::RustTest, "cargo-test", "error", run!("test-error")),
			],
			[expected("cargo-check"), expected("cargo-test")]
		);
	}

	#[test]
	fn clippy_lint_denied_by_dwarnings_is_an_error() {
		assert_eq!(normalize(ParserKind::Rust, "cargo-clippy", "clippy", run!("clippy-error")), [
			Diagnostic {
				end_line: Some(2),
				end_column: Some(18),
				suggestion: Some(Str::from(
					"for further information visit \
					 https://rust-lang.github.io/rust-clippy/rust-1.99.0/index.html#needless_return",
				)),
				..error(
					"cargo-clippy",
					"src/lib.rs",
					[2, 2],
					Some("clippy::needless_return"),
					"unneeded `return` statement",
				)
			}
		]);
	}

	#[test]
	fn panics_are_read_at_the_location_they_name() {
		assert_eq!(
			[
				normalize(ParserKind::RustTest, "cargo-test", "testfail", run!("test-fail")),
				normalize(ParserKind::Rust, "cargo-check", "buildpanic", run!("check-buildpanic")),
			],
			[
				vec![error(
					"cargo-test",
					"src/lib.rs",
					[9, 9],
					None,
					"thread 'tests::adds' panicked: assertion `left == right` failed\n  left: 4\n \
					 right: 5",
				)],
				vec![error(
					"cargo-check",
					"build.rs",
					[2, 5],
					None,
					"thread 'main' panicked: build script failed on purpose",
				)],
			]
		);
	}

	#[test]
	fn text_diagnostics_outside_records_still_parse() {
		assert_eq!(
			[
				normalize(ParserKind::Rust, "cargo-check", "manifest", run!("check-manifest")),
				normalize(ParserKind::RustTest, "cargo-test", "doctest", run!("test-doctest")),
			],
			[
				vec![error(
					"cargo-check",
					"Cargo.toml",
					[3, 11],
					None,
					"invalid type: integer `1`, expected SemVer version",
				)],
				vec![error("cargo-test", "src/lib.rs", [2, 14], Some("E0308"), "mismatched types")],
			]
		);
	}
}
