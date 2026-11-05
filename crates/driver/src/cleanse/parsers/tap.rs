//! Test Anything Protocol normalization.
//!
//! Reads TAP 14 and TAP 13 streams (`TAP version 14` or `13`) and version-less
//! streams in the TAP 12 style of Perl's `Test::More`. Each failing test point,
//! `not ok` without a `SKIP` or `TODO` directive, becomes one diagnostic. Its
//! location comes from the YAML block that follows it when that block names
//! one; otherwise the failure keeps its message with `file: None`. TAP 14 makes
//! a missing plan, a test count other than the plan's, and `Bail out!` a failed
//! run, so each adds one file-less failure.
//!
//! A subtest is a 4-space indented document ended by its correlated test point,
//! and it counts only when that point fails. YAML blocks are read for `message`
//! or `error` text and a `location` or `at` position; every other key is
//! ignored. Comments, pragmas (strict mode included) and non-TAP lines are
//! ignored, and test point IDs are not checked against the plan's range.

use std::{borrow::Cow, mem};

use omp_core::{Str, sf};

use super::{Diagnostic, ParserInput, Severity, relative_path};

const PLAN: &str = "tap-plan";
const BAIL_OUT: &str = "tap-bail-out";

/// Why stdout could not be read as a TAP stream.
#[derive(Debug, thiserror::Error)]
pub enum TapError {
	/// Stdout holds no TAP line at all.
	#[error("stdout holds no TAP stream (no version line, plan, test point or Bail out!)")]
	NoStream,
	/// The stream declares a TAP version this parser does not read.
	#[error("TAP version {version} is not supported; expected 14, 13, or no version line")]
	Version {
		/// The declared version.
		version: Str,
	},
	/// A TAP document declares a second plan.
	#[error("line {line} declares a second plan for its TAP document")]
	SecondPlan {
		/// One-based line of the second plan.
		line: usize,
	},
	/// A test point follows its TAP document's closing plan.
	#[error("line {line} is a test point after its TAP document's closing plan")]
	PointAfterPlan {
		/// One-based line of the test point.
		line: usize,
	},
}

/// Reads stdout, never stderr, as one TAP stream.
pub(super) fn parse(input: &ParserInput<'_>) -> Result<Vec<Diagnostic>, TapError> {
	let mut stream = Stream {
		input,
		documents: vec![Document::default()],
		yaml: None,
		last: None,
		seen: false,
		bailed: false,
	};
	for (index, line) in input.stdout.lines().enumerate() {
		stream.line(index + 1, line.trim_end())?;
		if stream.bailed {
			break;
		}
	}
	stream.finish()
}

struct Stream<'a> {
	input:     &'a ParserInput<'a>,
	/// The top-level document, then each open subtest, innermost last.
	documents: Vec<Document>,
	yaml:      Option<Yaml<'a>>,
	/// The last test point, while a YAML block may still follow it.
	last:      Option<Target>,
	seen:      bool,
	bailed:    bool,
}

impl<'a> Stream<'a> {
	fn line(&mut self, number: usize, line: &'a str) -> Result<(), TapError> {
		let content = line.trim_start_matches(' ');
		let indent = line.len() - content.len();
		if let Some(yaml) = &mut self.yaml {
			if indent == yaml.indent && content == "..." {
				self.end_yaml();
			} else {
				yaml.lines.push(&line[indent.min(yaml.indent)..]);
			}
			return Ok(());
		}
		if content.is_empty() || content.starts_with('#') {
			return Ok(());
		}
		if content == "---"
			&& let Some(target) = self.last.filter(|target| target.indent + 2 == indent)
		{
			self.yaml = Some(Yaml { indent, failure: target.failure, lines: Vec::new() });
			return Ok(());
		}
		self.last = None;
		if !indent.is_multiple_of(4) {
			return Ok(());
		}
		let depth = indent / 4;
		match classify(content) {
			Line::Other => {},
			Line::Version(version) => {
				if depth == 0 && !self.seen {
					if !matches!(version, "13" | "14") {
						return Err(TapError::Version { version: Str::new(version) });
					}
					self.seen = true;
				}
			},
			Line::BailOut(reason) => {
				self.seen = true;
				self.bailed = true;
				let message = if reason.is_empty() {
					sf!("Bail out!")
				} else {
					sf!("Bail out! {reason}")
				};
				let bail_out = failure(self.input, Some(BAIL_OUT), message);
				self.document().failures.push(bail_out);
			},
			Line::Plan(count) => {
				// TAP 14 subtest rule 9: a shallower line inside an open subtest is not TAP.
				if depth >= self.depth() {
					self.seen = true;
					self.descend(depth);
					self.document().plan(number, count)?;
				}
			},
			Line::Point(point) => {
				self.seen = true;
				if depth < self.depth() {
					self.ascend(depth);
				} else {
					self.descend(depth);
				}
				let failure = self.point(number, &point)?;
				self.last = Some(Target { indent, failure });
			},
		}
		Ok(())
	}

	fn document(&mut self) -> &mut Document {
		self
			.documents
			.last_mut()
			.expect("the top-level document stays open")
	}

	const fn depth(&self) -> usize {
		self.documents.len() - 1
	}

	fn descend(&mut self, depth: usize) {
		while self.depth() < depth {
			self.documents.push(Document::default());
		}
	}

	/// Closes every subtest deeper than `depth`; their failures wait for the
	/// correlated test point.
	fn ascend(&mut self, depth: usize) {
		while self.depth() > depth {
			let subtest = self.documents.pop().expect("a subtest is open");
			let failures = subtest.finish(self.input, self.bailed);
			self.document().subtest.extend(failures);
		}
	}

	fn point(&mut self, number: usize, point: &TestPoint<'_>) -> Result<Option<usize>, TapError> {
		let input = self.input;
		let document = self.document();
		if document.closed {
			return Err(TapError::PointAfterPlan { line: number });
		}
		document.points += 1;
		let subtest = mem::take(&mut document.subtest);
		if !point.failed() {
			return Ok(None);
		}
		let name = if point.description.is_empty() {
			sf!("test point {}", point.id.unwrap_or(document.points))
		} else {
			Str::new(point.description.as_ref())
		};
		document
			.failures
			.extend(subtest.into_iter().map(|mut failure| {
				failure.message = sf!("{name} > {}", failure.message);
				failure
			}));
		document.failures.push(failure(input, None, name));
		Ok(Some(document.failures.len() - 1))
	}

	fn end_yaml(&mut self) {
		let Some(yaml) = self.yaml.take() else {
			return;
		};
		let Some(index) = yaml.failure else {
			return;
		};
		let input = self.input;
		if let Some(failure) = self.document().failures.get_mut(index) {
			annotate(failure, &yaml.lines.join("\n"), input);
		}
	}

	fn finish(mut self) -> Result<Vec<Diagnostic>, TapError> {
		self.end_yaml();
		if !self.seen {
			return Err(TapError::NoStream);
		}
		self.ascend(0);
		let top = self
			.documents
			.pop()
			.expect("the top-level document stays open");
		Ok(top.finish(self.input, self.bailed))
	}
}

#[derive(Clone, Copy)]
struct Target {
	indent:  usize,
	failure: Option<usize>,
}

struct Yaml<'a> {
	indent:  usize,
	failure: Option<usize>,
	lines:   Vec<&'a str>,
}

#[derive(Default)]
struct Document {
	planned:  Option<u32>,
	/// The plan followed test points, so no test point may follow it.
	closed:   bool,
	points:   u32,
	failures: Vec<Diagnostic>,
	/// Failures of the subtest that closed last, waiting for its correlated
	/// test point.
	subtest:  Vec<Diagnostic>,
}

impl Document {
	const fn plan(&mut self, line: usize, count: u32) -> Result<(), TapError> {
		if self.planned.is_some() {
			return Err(TapError::SecondPlan { line });
		}
		self.planned = Some(count);
		self.closed = self.points > 0;
		Ok(())
	}

	/// Every failure the document holds, plus its plan's verdict unless the run
	/// bailed out.
	fn finish(mut self, input: &ParserInput<'_>, bailed: bool) -> Vec<Diagnostic> {
		self.failures.append(&mut self.subtest);
		let verdict = match self.planned {
			_ if bailed => None,
			None => Some(sf!(
				"TAP document ended without a plan: the run was truncated or its producer crashed"
			)),
			Some(planned) if planned != self.points => Some(sf!(
				"TAP plan 1..{planned} expected {planned} test points but {} ran",
				self.points
			)),
			Some(_) => None,
		};
		self
			.failures
			.extend(verdict.map(|message| failure(input, Some(PLAN), message)));
		self.failures
	}
}

enum Line<'a> {
	Version(&'a str),
	Plan(u32),
	Point(TestPoint<'a>),
	BailOut(Cow<'a, str>),
	Other,
}

fn classify(content: &str) -> Line<'_> {
	if let Some(version) = content.strip_prefix("TAP version ") {
		Line::Version(version.trim())
	} else if let Some(count) = plan(content) {
		Line::Plan(count)
	} else if let Some(point) = TestPoint::parse(content) {
		Line::Point(point)
	} else if content
		.get(..9)
		.is_some_and(|prefix| prefix.eq_ignore_ascii_case("bail out!"))
	{
		Line::BailOut(unescape(content[9..].trim()))
	} else {
		Line::Other
	}
}

fn plan(content: &str) -> Option<u32> {
	let rest = content.strip_prefix("1..")?;
	let (count, tail) = rest.split_at(rest.bytes().take_while(u8::is_ascii_digit).count());
	let tail = tail.trim_start();
	if tail.is_empty() || tail.starts_with('#') {
		count.parse().ok()
	} else {
		None
	}
}

struct TestPoint<'a> {
	ok:          bool,
	id:          Option<u32>,
	description: Cow<'a, str>,
	/// Carries a `SKIP` or `TODO` directive, so it never counts as a failure.
	exempt:      bool,
}

impl<'a> TestPoint<'a> {
	fn parse(content: &'a str) -> Option<Self> {
		let (ok, rest) = match content.strip_prefix("not ok") {
			Some(rest) => (false, rest),
			None => (true, content.strip_prefix("ok")?),
		};
		if rest.starts_with(|character: char| !character.is_whitespace()) {
			return None;
		}
		let rest = rest.trim_start();
		let (id, rest) = rest.split_at(rest.bytes().take_while(u8::is_ascii_digit).count());
		let (description, exempt) = directive(rest);
		let description = description.trim();
		let description = description
			.strip_prefix('-')
			.map_or(description, str::trim_start);
		Some(Self { ok, id: id.parse().ok(), description: unescape(description), exempt })
	}

	const fn failed(&self) -> bool {
		!self.ok && !self.exempt
	}
}

/// Splits a test point's tail at its directive delimiter: the first unescaped
/// `#` that starts the tail or follows whitespace (TAP 14 "Directive"). An
/// unrecognized directive stays part of the description.
fn directive(text: &str) -> (&str, bool) {
	let bytes = text.as_bytes();
	let mut escaped = false;
	for (index, &byte) in bytes.iter().enumerate() {
		if escaped {
			escaped = false;
		} else if byte == b'\\' {
			escaped = true;
		} else if byte == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
			// TAP 14 accepts any suffix on the word (`# Skipped:`), so only its prefix
			// counts.
			let exempt = text[index + 1..].trim_start().get(..4).is_some_and(|word| {
				word.eq_ignore_ascii_case("skip") || word.eq_ignore_ascii_case("todo")
			});
			return if exempt {
				(&text[..index], true)
			} else {
				(text, false)
			};
		}
	}
	(text, false)
}

/// Resolves TAP 14 escapes: `\\` is `\` and `\#` is `#`.
fn unescape(text: &str) -> Cow<'_, str> {
	if !text.contains('\\') {
		return Cow::Borrowed(text);
	}
	let mut output = String::with_capacity(text.len());
	let mut characters = text.chars().peekable();
	while let Some(character) = characters.next() {
		if character == '\\'
			&& let Some(&escaped @ ('\\' | '#')) = characters.peek()
		{
			output.push(escaped);
			characters.next();
		} else {
			output.push(character);
		}
	}
	Cow::Owned(output)
}

fn failure(input: &ParserInput<'_>, code: Option<&'static str>, message: Str) -> Diagnostic {
	Diagnostic {
		checker: Str::from(input.checker),
		file: None,
		line: None,
		column: None,
		end_line: None,
		end_column: None,
		code: code.map(Str::new_static),
		severity: Severity::Error,
		message,
		suggestion: None,
	}
}

/// Folds a YAML diagnostic block into the failure of the test point it
/// follows; a block that is not valid YAML is kept verbatim in the message.
fn annotate(failure: &mut Diagnostic, block: &str, input: &ParserInput<'_>) {
	let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(block) else {
		let block = block.trim();
		if !block.is_empty() {
			failure.message = sf!("{}: {block}", failure.message);
		}
		return;
	};
	if let Some(detail) = ["message", "error"]
		.iter()
		.find_map(|key| value.get(*key)?.as_str())
		.map(str::trim)
		.filter(|detail| !detail.is_empty())
	{
		failure.message = sf!("{}: {detail}", failure.message);
	}
	if let Some((path, line, column)) = position(&value) {
		failure.file = relative_path(path, input);
		failure.line = line;
		failure.column = column;
	}
}

/// A `location` string (node:test) or an `at` string or mapping (the TAP 14
/// example, deno, tape, node-tap) as path, line and column.
fn position(value: &serde_yaml::Value) -> Option<(&str, Option<u32>, Option<u32>)> {
	if let Some(text) = value.get("location").and_then(serde_yaml::Value::as_str) {
		return Some(split_location(text));
	}
	let at = value.get("at")?;
	if let Some(text) = at.as_str() {
		return Some(split_location(text));
	}
	let file = ["file", "fileName", "filename"]
		.iter()
		.find_map(|key| at.get(*key)?.as_str())?;
	let number = |keys: [&str; 2]| keys.iter().find_map(|key| yaml_number(at.get(*key)?));
	Some((file, number(["line", "lineNumber"]), number(["column", "columnNumber"])))
}

fn yaml_number(value: &serde_yaml::Value) -> Option<u32> {
	value
		.as_u64()
		.and_then(|number| u32::try_from(number).ok())
		.or_else(|| value.as_str()?.parse().ok())
		.filter(|number| *number > 0)
}

/// Splits `path:line[:column]`, also inside tape's `name (path:line:column)`.
fn split_location(text: &str) -> (&str, Option<u32>, Option<u32>) {
	let text = text.trim();
	let text = text
		.strip_suffix(')')
		.and_then(|inner| inner.rsplit_once('('))
		.map_or(text, |(_, inner)| inner);
	let number = |part: &str| part.parse::<u32>().ok().filter(|number| *number > 0);
	let Some((rest, last)) = text.rsplit_once(':') else {
		return (text, None, None);
	};
	let Some(last) = number(last) else {
		return (text, None, None);
	};
	match rest
		.rsplit_once(':')
		.and_then(|(path, line)| Some((path, number(line)?)))
	{
		Some((path, line)) => (path, Some(line), Some(last)),
		None => (rest, Some(last), None),
	}
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;

	const NODE: &str = include_str!("../../../../../fixtures/tap/node.tap");
	const DENO: &str = include_str!("../../../../../fixtures/tap/deno.tap");
	const PERL: &str = include_str!("../../../../../fixtures/tap/perl.tap");
	const PERL_STDERR: &str = include_str!("../../../../../fixtures/tap/perl.stderr");
	const PERL_TRUNCATED: &str = include_str!("../../../../../fixtures/tap/perl-truncated.tap");
	const PERL_BAIL: &str = include_str!("../../../../../fixtures/tap/perl-bail.tap");
	const NO_PLAN: &str =
		"TAP document ended without a plan: the run was truncated or its producer crashed";

	type Row<'a> = (Option<&'a str>, Option<u32>, Option<u32>, Option<&'a str>, &'a str);

	fn run(root: &str, stdout: &str, stderr: &str) -> Result<Vec<Diagnostic>, TapError> {
		parse(&ParserInput {
			checker: "tap",
			checker_root: Path::new(root),
			project_root: Path::new(root),
			stdout,
			stderr,
		})
	}

	fn rows(diagnostics: &[Diagnostic]) -> Vec<Row<'_>> {
		diagnostics
			.iter()
			.map(|diagnostic| {
				(
					diagnostic.file.as_deref(),
					diagnostic.line,
					diagnostic.column,
					diagnostic.code.as_deref(),
					diagnostic.message.as_str(),
				)
			})
			.collect()
	}

	fn messages(stdout: &str) -> Vec<String> {
		run("/repo", stdout, "")
			.expect("TAP stream")
			.iter()
			.map(|diagnostic| diagnostic.message.as_str().to_owned())
			.collect()
	}

	#[test]
	fn node_yaml_locations_and_subtests_normalize() {
		let root = "/private/tmp/tap-work/node";
		assert_eq!(rows(&run(root, NODE, "").expect("node stream")), [
			(
				Some("outcomes.test.mjs"),
				Some(8),
				Some(1),
				None,
				"fails an assertion: Expected values to be strictly equal:\n\n2 !== 3"
			),
			(Some("outcomes.test.mjs"), Some(12), Some(1), None, "throws an error: boom"),
			(
				Some("outcomes.test.mjs"),
				Some(24),
				Some(3),
				None,
				"suite > nested fails: Expected values to be strictly deep-equal:\n+ actual - \
				 expected\n\n  {\n+   a: 1\n-   a: 2\n  }"
			),
			(Some("outcomes.test.mjs"), Some(22), Some(1), None, "suite: 1 subtest failed"),
			(
				Some("outcomes.test.mjs"),
				Some(33),
				Some(3),
				None,
				"setup > never runs: test did not finish before its parent and was cancelled"
			),
			(Some("outcomes.test.mjs"), Some(29), Some(1), None, "setup: setup failed"),
		]);
	}

	#[test]
	fn deno_json_flow_yaml_and_steps_normalize() {
		let root = "/private/tmp/tap-work/deno";
		assert_eq!(rows(&run(root, DENO, "error: Test failed\n").expect("deno stream")), [
			(
				Some("outcomes_test.ts"),
				Some(3),
				None,
				None,
				"fails an assertion: Error: expected 3\n    throw new Error(\"expected 3\");\n          ^\n    at file:///private/tmp/tap-work/deno/outcomes_test.ts:5:11"
			),
			(
				Some("outcomes_test.ts"),
				Some(13),
				None,
				None,
				"steps > step fails: Error: step failure\n    throw new Error(\"step failure\");\n          ^\n    at file:///private/tmp/tap-work/deno/outcomes_test.ts:14:11\n    at innerWrapped (ext:cli/40_test.js:300:11)\n    at exitSanitizer (ext:cli/40_test.js:201:33)\n    at Object.outerWrapped [as fn] (ext:cli/40_test.js:239:28)\n    at TestContext.step (ext:cli/40_test.js:855:37)\n    at file:///private/tmp/tap-work/deno/outcomes_test.ts:13:11"
			),
			(Some("outcomes_test.ts"), Some(11), None, None, "steps: 1 test step failed."),
		]);
	}

	/// `Test::More` prints failure details to stderr, which the parser does not
	/// read, so Perl failures keep their names with no file.
	#[test]
	fn perl_failures_keep_their_names_without_a_file() {
		let root = "/private/tmp/tap-work/perl";
		assert_eq!(rows(&run(root, PERL, PERL_STDERR).expect("perl stream")), [
			(None, None, None, None, "fails an assertion"),
			(None, None, None, None, "test point 5"),
		]);
	}

	#[test]
	fn truncated_and_bailed_streams_fail_without_a_file() {
		assert_eq!(rows(&run("/repo", PERL_TRUNCATED, "").expect("truncated stream")), [
			(None, None, None, None, "fails before the crash"),
			(None, None, None, Some("tap-plan"), NO_PLAN),
		]);
		assert_eq!(rows(&run("/repo", PERL_BAIL, "").expect("bailed stream")), [(
			None,
			None,
			None,
			Some("tap-bail-out"),
			"Bail out! database unavailable"
		)]);
	}

	#[test]
	fn failures_without_yaml_description_or_id_keep_a_message() {
		assert_eq!(messages("1..3\nnot ok\nnot ok 7\nnot ok - named\n"), [
			"test point 1",
			"test point 7",
			"named"
		]);
	}

	#[test]
	fn yaml_blocks_degrade_level_by_level() {
		let stream = concat!(
			"TAP version 14\n1..4\n",
			"not ok 1 - no location\n  ---\n  message: only text\n  ...\n",
			"not ok 2 - malformed yaml\n  ---\n  message: [unclosed\n  ...\n",
			"not ok 3 - string at\n  ---\n  at: 'Test.<anonymous> (/repo/test/a.js:5:7)'\n  ...\n",
			"not ok 4 - at without line\n  # a comment may sit between\n  ---\n  at:\n    file: \
			 test/b.js\n  ...\n",
		);
		assert_eq!(rows(&run("/repo", stream, "").expect("stream")), [
			(None, None, None, None, "no location: only text"),
			(None, None, None, None, "malformed yaml: message: [unclosed"),
			(Some("test/a.js"), Some(5), Some(7), None, "string at"),
			(Some("test/b.js"), None, None, None, "at without line"),
		]);
	}

	#[test]
	fn plan_violations_and_bail_out_are_file_less_failures() {
		assert_eq!(messages("1..3\nok 1\nnot ok 2 - real\n"), [
			"real",
			"TAP plan 1..3 expected 3 test points but 2 ran"
		]);
		assert_eq!(messages("ok 1\nok 2\n"), [NO_PLAN]);
		assert_eq!(messages("TAP version 14\n"), [NO_PLAN]);
		assert!(messages("TAP version 13\n1..0 # SKIP no database\n").is_empty());
		assert_eq!(messages("1..2\nok 1\n    Bail out! \\# child stopped\n"), [
			"Bail out! # child stopped"
		]);
	}

	#[test]
	fn directives_follow_tap14_escaping() {
		let stream = concat!(
			"1..5\n",
			"not ok 1 - skipped # SKIP no network\n",
			"not ok 2 - expected # TODO later\n",
			"not ok 3 - literal \\# TODO in the name\n",
			"not ok 4 - timed # TIME 3s\n",
			"ok 5 - unexpectedly passing # todo\n",
		);
		assert_eq!(messages(stream), ["literal # TODO in the name", "timed # TIME 3s"]);
	}

	/// TAP 14 subtest rules 4.2 and 9: a subtest counts through its correlated
	/// test point, and an unindented non-TAP line does not end it.
	#[test]
	fn subtests_count_through_their_correlated_test_point() {
		let stream = concat!(
			"1..3\n",
			"# Subtest: passing parent\n",
			"    not ok 1 - ignored child\n",
			"    1..1\n",
			"ok 1 - passing parent\n",
			"# Subtest: todo parent\n",
			"    not ok 1 - child of a todo\n",
			"    1..1\n",
			"not ok 2 - todo parent # TODO not yet\n",
			"# Subtest: failing parent\n",
			"    ok 1 - fine\n",
			"console output at the parent level\n",
			"    not ok 2 - child failure\n",
			"    1..2\n",
			"not ok 3 - failing parent\n",
		);
		assert_eq!(messages(stream), ["failing parent > child failure", "failing parent"]);
	}

	#[test]
	fn invalid_streams_are_errors_not_partial_results() {
		let error = |stdout: &str| run("/repo", stdout, "").expect_err("invalid stream");
		assert!(matches!(error(""), TapError::NoStream));
		assert!(matches!(error("compiling...\nall good\n"), TapError::NoStream));
		assert!(matches!(
			error("TAP version 15\n1..1\nok 1\n"),
			TapError::Version { version } if version.as_str() == "15"
		));
		assert!(matches!(error("1..1\nok 1\n1..1\n"), TapError::SecondPlan { line: 3 }));
		assert!(matches!(error("ok 1\n1..1\nok 2\n"), TapError::PointAfterPlan { line: 3 }));
	}
}
