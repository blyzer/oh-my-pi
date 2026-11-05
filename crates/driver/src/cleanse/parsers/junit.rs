//! JUnit XML report normalization: one diagnostic per `<failure>` or `<error>`.
//!
//! Each dialect rule is pinned by a recorded report under `fixtures/junit`.
//! nextest and jest-junit write every failed test as `<failure>` (panics,
//! aborts and `beforeAll` errors included), so the element is never read as a
//! test-versus-infrastructure split. pytest's xunit1 `line` is zero-based.
//! jest-junit starts `classname` and `name` with a space. Surefire writes a
//! bare `<testsuite>` root.

use std::borrow::Cow;

use omp_core::{Str, StrMut, sf};
use quick_xml::{
	Reader, XmlVersion,
	encoding::Decoder,
	escape::resolve_predefined_entity,
	events::{BytesRef, BytesStart, Event},
};

use super::{Diagnostic, ParserInput, Severity, relative_path};

/// `testsuites` name pytest hardcodes (`_pytest/junitxml.py`); its xunit1
/// `line` attribute is the zero-based `location[1]`.
const PYTEST_REPORT: &str = "pytest tests";

/// Why a JUnit report could not be read.
#[derive(Debug, thiserror::Error)]
pub(super) enum JunitError {
	#[error("no <testsuites> or <testsuite> element")]
	Missing,
	#[error("root element <{found}> is not <testsuites> or <testsuite>")]
	Root { found: Str },
	#[error("text outside the root element at byte {position}")]
	Text { position: u64 },
	#[error("report ends with {open} element(s) unclosed")]
	Truncated { open: usize },
	#[error("undefined entity `&{name};` at byte {position}")]
	Entity { name: Str, position: u64 },
	#[error("malformed XML at byte {position}")]
	Xml {
		position: u64,
		#[source]
		source:   quick_xml::Error,
	},
}

/// Reads the JUnit XML report on stdout; stderr is the runner's console.
///
/// `Ok` holds one diagnostic per `<failure>` or `<error>` in document order
/// and is empty when no test failed. Unreadable input is an `Err` naming
/// why, never a partial or guessed result. Concatenated reports, as `cat`
/// over per-class Surefire files produces, are read as a whole.
pub(super) fn parse(input: &ParserInput<'_>) -> Result<Vec<Diagnostic>, JunitError> {
	let mut reader = Reader::from_str(input.stdout);
	let mut report = Report {
		input,
		diagnostics: Vec::new(),
		depth: 0,
		roots: 0,
		pytest: false,
		case: None,
		failure: None,
	};
	loop {
		let start = reader.buffer_position();
		let event = reader
			.read_event()
			.map_err(|source| JunitError::Xml { position: reader.error_position(), source })?;
		let xml = |source| JunitError::Xml { position: start, source };
		match event {
			Event::Start(element) => report.open(&element, reader.decoder(), start)?,
			Event::Empty(element) => {
				report.open(&element, reader.decoder(), start)?;
				report.close();
			},
			Event::End(_) => report.close(),
			Event::Text(text) => {
				if report.depth == 0 && !text.iter().all(u8::is_ascii_whitespace) {
					return Err(JunitError::Text { position: start });
				}
				if let Some(body) = report.body() {
					body.push_str(
						&text
							.xml_content(XmlVersion::Implicit1_0)
							.map_err(|source| xml(source.into()))?,
					);
				}
			},
			Event::CData(data) => {
				if report.depth == 0 {
					return Err(JunitError::Text { position: start });
				}
				if let Some(body) = report.body() {
					body.push_str(
						&data
							.xml_content(XmlVersion::Implicit1_0)
							.map_err(|source| xml(source.into()))?,
					);
				}
			},
			Event::GeneralRef(reference) => {
				if report.depth == 0 {
					return Err(JunitError::Text { position: start });
				}
				if let Some(body) = report.body() {
					push_reference(body, &reference, start)?;
				}
			},
			Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {},
			Event::Eof => break,
		}
	}
	match (report.depth, report.roots) {
		(0, 0) => Err(JunitError::Missing),
		(0, _) => Ok(report.diagnostics),
		(open, _) => Err(JunitError::Truncated { open }),
	}
}

struct Report<'a> {
	input:       &'a ParserInput<'a>,
	diagnostics: Vec<Diagnostic>,
	/// Open elements; zero between concatenated reports.
	depth:       usize,
	roots:       usize,
	pytest:      bool,
	case:        Option<Case>,
	failure:     Option<Failure>,
}

struct Case {
	depth: usize,
	id:    Str,
	file:  Option<Str>,
	line:  Option<u32>,
}

struct Failure {
	depth:   usize,
	code:    Str,
	message: Option<String>,
	body:    String,
}

impl Report<'_> {
	fn open(
		&mut self,
		element: &BytesStart<'_>,
		decoder: Decoder,
		position: u64,
	) -> Result<(), JunitError> {
		let name = element.local_name();
		let name = name.as_ref();
		if self.depth == 0 {
			if !matches!(name, b"testsuites" | b"testsuite") {
				return Err(JunitError::Root { found: Str::from(&*String::from_utf8_lossy(name)) });
			}
			self.roots += 1;
			self.pytest = name == b"testsuites"
				&& attribute(element, b"name", decoder, position)?.as_deref() == Some(PYTEST_REPORT);
		}
		self.depth += 1;
		match name {
			b"testcase" if self.case.is_none() => {
				self.case = Some(self.case(element, decoder, position)?);
			},
			b"failure" | b"error" if self.failure.is_none() => {
				self.failure = Some(Failure {
					depth:   self.depth,
					code:    if name == b"failure" {
						sf!("failure")
					} else {
						sf!("error")
					},
					message: attribute(element, b"message", decoder, position)?.map(Cow::into_owned),
					body:    String::new(),
				});
			},
			_ => {},
		}
		Ok(())
	}

	fn close(&mut self) {
		if let Some(failure) = self.failure.take_if(|failure| failure.depth == self.depth) {
			self.emit(failure);
		}
		if self
			.case
			.as_ref()
			.is_some_and(|case| case.depth == self.depth)
		{
			self.case = None;
		}
		self.depth = self.depth.saturating_sub(1);
	}

	fn body(&mut self) -> Option<&mut String> {
		self.failure.as_mut().map(|failure| &mut failure.body)
	}

	fn case(
		&self,
		element: &BytesStart<'_>,
		decoder: Decoder,
		position: u64,
	) -> Result<Case, JunitError> {
		let classname = attribute(element, b"classname", decoder, position)?;
		let name = attribute(element, b"name", decoder, position)?;
		let classname = classname.as_deref().map_or("", str::trim);
		let name = name.as_deref().map_or("", str::trim);
		let id = if classname.is_empty() || classname == name {
			Str::from(name)
		} else if name.is_empty() {
			Str::from(classname)
		} else {
			sf!("{classname}::{name}")
		};
		let file = attribute(element, b"file", decoder, position)?
			.and_then(|file| relative_path(&file, self.input));
		let line = if file.is_some() {
			attribute(element, b"line", decoder, position)?
				.and_then(|line| line.trim().parse::<u32>().ok())
				.map(|line| {
					if self.pytest {
						line.saturating_add(1)
					} else {
						line
					}
				})
		} else {
			None
		};
		Ok(Case { depth: self.depth, id, file, line })
	}

	fn emit(&mut self, failure: Failure) {
		let (id, file, line) = self
			.case
			.as_ref()
			.map_or(("", None, None), |case| (case.id.as_str(), case.file.clone(), case.line));
		let body = failure.body.trim();
		let message = failure
			.message
			.as_deref()
			.map(str::trim)
			.filter(|message| !message.is_empty());
		let mut text = StrMut::with_capacity(id.len() + message.map_or(0, str::len) + body.len() + 3);
		if !id.is_empty() {
			text.push_str(id);
			text.push_str(": ");
		}
		// nextest and Surefire repeat the `message` attribute in the body.
		match message {
			Some(message) if body.is_empty() => text.push_str(message),
			Some(message) if !body.contains(message) => {
				text.push_str(message);
				text.push('\n');
				text.push_str(body);
			},
			_ if body.is_empty() => text.push_str(&failure.code),
			_ => text.push_str(body),
		}
		self.diagnostics.push(Diagnostic {
			checker: Str::from(self.input.checker),
			file,
			line,
			column: None,
			end_line: None,
			end_column: None,
			code: Some(failure.code),
			severity: Severity::Error,
			message: text.freeze(),
			suggestion: None,
		});
	}
}

fn attribute<'a>(
	element: &'a BytesStart<'_>,
	key: &[u8],
	decoder: Decoder,
	position: u64,
) -> Result<Option<Cow<'a, str>>, JunitError> {
	let xml = |source| JunitError::Xml { position, source };
	let Some(attribute) = element
		.try_get_attribute(key)
		.map_err(|source| xml(source.into()))?
	else {
		return Ok(None);
	};
	attribute
		.decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
		.map(Some)
		.map_err(xml)
}

fn push_reference(
	body: &mut String,
	reference: &BytesRef<'_>,
	position: u64,
) -> Result<(), JunitError> {
	let xml = |source| JunitError::Xml { position, source };
	if let Some(character) = reference.resolve_char_ref().map_err(xml)? {
		body.push(character);
		return Ok(());
	}
	let name = reference.decode().map_err(|source| xml(source.into()))?;
	let text = resolve_predefined_entity(&name)
		.ok_or_else(|| JunitError::Entity { name: Str::from(&*name), position })?;
	body.push_str(text);
	Ok(())
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;

	const PYTEST: &str = include_str!("../../../../../fixtures/junit/pytest.xml");
	const PYTEST_XUNIT1: &str = include_str!("../../../../../fixtures/junit/pytest-xunit1.xml");
	const NEXTEST: &str = include_str!("../../../../../fixtures/junit/nextest.xml");
	const NEXTEST_ABORT: &str = include_str!("../../../../../fixtures/junit/nextest-abort.xml");
	const JEST: &str = include_str!("../../../../../fixtures/junit/jest.xml");
	const SUREFIRE: &str = include_str!("../../../../../fixtures/junit/surefire.xml");
	const REPORTS: [&str; 6] = [PYTEST, PYTEST_XUNIT1, NEXTEST, NEXTEST_ABORT, JEST, SUREFIRE];
	const POM: &str = include_str!("../../../../../fixtures/junit/sources/junit5/pom.xml");
	const PACKAGE_JSON: &str =
		include_str!("../../../../../fixtures/junit/sources/jest/package.json");

	fn input<'a>(stdout: &'a str, stderr: &'a str) -> ParserInput<'a> {
		let root = Path::new("/repo");
		ParserInput { checker: "junit", checker_root: root, project_root: root, stdout, stderr }
	}

	fn read(stdout: &str) -> Result<Vec<Diagnostic>, JunitError> {
		parse(&input(stdout, ""))
	}

	fn heads(diagnostics: &[Diagnostic]) -> Vec<(Option<&str>, &str)> {
		diagnostics
			.iter()
			.map(|diagnostic| {
				(diagnostic.code.as_deref(), diagnostic.message.lines().next().unwrap_or_default())
			})
			.collect()
	}

	#[test]
	fn pytest_xunit2_failures_keep_their_message_without_a_location() {
		let diagnostics = read(PYTEST).expect("pytest report");
		assert_eq!(heads(&diagnostics), [
			(Some("failure"), "test_outcomes::test_fail: AssertionError: intentional failure"),
			(
				Some("error"),
				"test_outcomes::test_error_fixture: failed on setup with \"RuntimeError: intentional \
				 error\"",
			),
		]);
		assert!(
			diagnostics
				.iter()
				.all(|diagnostic| diagnostic.file.is_none())
		);
		assert!(
			diagnostics
				.iter()
				.all(|diagnostic| diagnostic.line.is_none())
		);
		assert!(
			diagnostics[0]
				.message
				.ends_with("\n\ntest_outcomes.py:3: AssertionError")
		);
		assert!(
			diagnostics[1]
				.message
				.ends_with("\n\ntest_outcomes.py:7: RuntimeError")
		);
	}

	#[test]
	fn pytest_xunit1_zero_based_line_becomes_one_based() {
		let diagnostics = read(PYTEST_XUNIT1).expect("pytest xunit1 report");
		let located: Vec<_> = diagnostics
			.iter()
			.map(|diagnostic| (diagnostic.file.as_deref(), diagnostic.line))
			.collect();
		assert_eq!(
			located,
			[(Some("test_outcomes.py"), Some(3)), (Some("test_outcomes.py"), Some(4))],
			"line=\"2\" is def test_fail, which the traceback reports as test_outcomes.py:3"
		);
		assert!(
			diagnostics[0]
				.message
				.ends_with("test_outcomes.py:3: AssertionError")
		);
		let other = PYTEST_XUNIT1.replacen("pytest tests", "other tests", 1);
		assert_eq!(read(&other).expect("renamed report")[0].line, Some(2));
	}

	#[test]
	fn nextest_panics_and_aborts_are_failures() {
		let diagnostics = read(NEXTEST).expect("nextest report");
		let shapes: Vec<_> = diagnostics
			.iter()
			.map(|diagnostic| {
				(diagnostic.code.as_deref(), diagnostic.severity, diagnostic.message.as_str())
			})
			.collect();
		assert_eq!(shapes, [
			(
				Some("failure"),
				Severity::Error,
				"junit_fixture::errors: thread 'errors' (23924076) panicked at \
				 src/lib.rs:12:15:\nintentional error\nnote: run with `RUST_BACKTRACE=1` environment \
				 variable to display a backtrace",
			),
			(
				Some("failure"),
				Severity::Error,
				"junit_fixture::fails: thread 'fails' (23924079) panicked at \
				 src/lib.rs:5:14:\nassertion `left == right` failed: intentional failure\n  left: 2\n \
				 right: 3\nnote: run with `RUST_BACKTRACE=1` environment variable to display a \
				 backtrace",
			),
		]);
		let aborted = read(NEXTEST_ABORT).expect("nextest abort report");
		assert_eq!(heads(&aborted), [(
			Some("failure"),
			"junit_abort::aborts: process aborted with signal 6 (SIGABRT)"
		)]);
		assert!(
			diagnostics
				.iter()
				.chain(&aborted)
				.all(|diagnostic| diagnostic.file.is_none())
		);
	}

	#[test]
	fn jest_names_lose_their_leading_space_and_before_all_is_a_failure() {
		let diagnostics = read(JEST).expect("jest report");
		assert_eq!(heads(&diagnostics), [
			(Some("failure"), "fails: Error: expect(received).toBe(expected) // Object.is equality"),
			(Some("failure"), "setup errors: Error: intentional error"),
		]);
	}

	#[test]
	fn surefire_bare_testsuite_root_reads() {
		let diagnostics = read(SUREFIRE).expect("surefire report");
		assert_eq!(heads(&diagnostics), [
			(
				Some("error"),
				"fixture.OutcomesTest::errors: java.lang.IllegalStateException: intentional error",
			),
			(
				Some("failure"),
				"fixture.OutcomesTest::fails: org.opentest4j.AssertionFailedError: intentional \
				 failure ==> expected: <3> but was: <2>",
			),
		]);
		assert!(
			diagnostics
				.iter()
				.all(|diagnostic| diagnostic.file.is_none()),
			"Java frames name a file, not a project path"
		);
	}

	#[test]
	fn report_without_failures_is_ok_and_empty() {
		let passing = "<testsuites name=\"pytest tests\"><testsuite name=\"pytest\" \
		               tests=\"1\"><testcase classname=\"test_outcomes\" name=\"test_pass\" \
		               /></testsuite></testsuites>";
		assert!(read(passing).expect("passing report").is_empty());
	}

	#[test]
	fn stderr_console_is_not_part_of_the_report() {
		let diagnostics = parse(&input(NEXTEST, "error: test run failed")).expect("nextest report");
		assert_eq!(diagnostics, read(NEXTEST).expect("nextest report"));
	}

	#[test]
	fn concatenated_reports_read_as_a_whole() {
		let separate: Vec<_> = REPORTS
			.iter()
			.flat_map(|report| read(report).expect("report"))
			.collect();
		assert_eq!(separate.len(), 11);
		assert_eq!(read(&REPORTS.concat()).expect("concatenated reports"), separate);
	}

	#[test]
	fn malformed_reports_return_a_reason() {
		assert!(matches!(read(""), Err(JunitError::Missing)));
		assert!(matches!(read("\n"), Err(JunitError::Missing)));
		assert!(matches!(read(POM), Err(JunitError::Root { found }) if found.as_str() == "project"));
		assert!(matches!(read(PACKAGE_JSON), Err(JunitError::Text { position: 0 })));
		let inside_tag = SUREFIRE
			.find("<testcase name=\"fails\"")
			.expect("fails testcase")
			+ 9;
		assert!(matches!(read(&SUREFIRE[..inside_tag]), Err(JunitError::Xml { .. })));
		let between = SUREFIRE
			.find("<testcase name=\"passes\"")
			.expect("passes testcase");
		assert!(matches!(read(&SUREFIRE[..between]), Err(JunitError::Truncated { open: 1 })));
		let console = format!("{NEXTEST}test result: FAILED. 1 passed; 2 failed");
		assert!(matches!(read(&console), Err(JunitError::Text { .. })));
	}

	#[test]
	fn every_truncated_report_is_an_error() {
		for report in REPORTS {
			let end = report.rfind('>').expect("root end tag") + 1;
			for (cut, _) in report.char_indices().take_while(|&(cut, _)| cut < end) {
				assert!(read(&report[..cut]).is_err(), "{cut}-byte prefix parsed");
			}
			assert_eq!(read(&report[..end]).expect("whole report"), read(report).expect("report"));
		}
	}
}
