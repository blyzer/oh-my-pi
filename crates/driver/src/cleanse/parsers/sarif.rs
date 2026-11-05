//! SARIF 2.1.0 log normalization.
//!
//! Each `runs[].results[]` entry the log reports as a current problem becomes
//! one diagnostic. Every level below a result is optional: a result whose
//! location does not resolve to a file keeps its message with `file: None`.
//! A run whose `results` is null or absent, or whose invocation reports
//! `executionSuccessful: false`, adds one file-less failure, so a tool that
//! failed never reads as a clean run. Invocation `ruleConfigurationOverrides`,
//! `message.id` lookups and rules of tool extensions are not consulted.

use std::{collections::BTreeMap, fmt::Write as _};

use omp_core::{Str, sf};
use url::Url;

use super::{Diagnostic, ParserInput, Severity, relative_path};

const RUN_FAILED: &str = "sarif-run-failed";
/// `uriBaseId` indirections followed before a base counts as unresolvable,
/// which bounds a cyclic `originalUriBaseIds` table.
const MAX_BASE_DEPTH: u8 = 8;

/// Why stdout could not be read as a SARIF log.
#[derive(Debug, thiserror::Error)]
pub(super) enum SarifError {
	#[error("stdout is empty; expected a SARIF 2.1.0 log")]
	Empty,
	#[error("stdout is not a SARIF 2.1.0 log")]
	Json(#[from] serde_json::Error),
	#[error("SARIF version {version} is not supported; expected 2.1.0")]
	Version { version: Str },
}

/// Reads stdout, never stderr, as one SARIF 2.1.0 log.
pub(super) fn parse(input: &ParserInput<'_>) -> Result<Vec<Diagnostic>, SarifError> {
	let text = input.stdout.trim();
	if text.is_empty() {
		return Err(SarifError::Empty);
	}
	let log = serde_json::from_str::<Log>(text).map_err(|error| {
		serde_json::from_str::<Header>(text)
			.ok()
			.and_then(|header| supported(&header.version).err())
			.unwrap_or(SarifError::Json(error))
	})?;
	supported(&log.version)?;
	let Some(runs) = log.runs else {
		return Ok(vec![run_failed(
			input,
			sf!(
				"SARIF log has no runs: its producer failed to populate them (runs is null or absent)"
			),
		)]);
	};
	let root = Url::from_directory_path(input.checker_root).ok();
	let mut diagnostics = Vec::new();
	for run in &runs {
		diagnostics.extend(run.failure().map(|message| run_failed(input, message)));
		let rules = run.rules();
		for result in run
			.results
			.iter()
			.flatten()
			.filter(|result| result.is_problem())
		{
			diagnostics.push(result.diagnostic(run, rules, input, root.as_ref()));
		}
	}
	Ok(diagnostics)
}

fn supported(version: &str) -> Result<(), SarifError> {
	if version == "2.1.0" {
		Ok(())
	} else {
		Err(SarifError::Version { version: Str::new(version) })
	}
}

fn run_failed(input: &ParserInput<'_>, message: Str) -> Diagnostic {
	Diagnostic {
		checker: Str::from(input.checker),
		file: None,
		line: None,
		column: None,
		end_line: None,
		end_column: None,
		code: Some(sf!(RUN_FAILED)),
		severity: Severity::Error,
		message,
		suggestion: None,
	}
}

/// Ensures a base URI names a directory, so joining keeps its last segment.
fn directory(mut url: Url) -> Url {
	if !url.path().ends_with('/') {
		let path = format!("{}/", url.path());
		url.set_path(&path);
	}
	url
}

/// Just the `version` of a log, read when the full shape does not parse.
#[derive(serde::Deserialize)]
struct Header {
	version: Str,
}

/// SARIF 2.1.0 §3.13 `sarifLog`; every type below keeps only the fields
/// diagnostics use and ignores the rest.
#[derive(serde::Deserialize)]
struct Log {
	version: Str,
	runs:    Option<Vec<Run>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Run {
	tool:                  Option<Tool>,
	results:               Option<Vec<SarifResult>>,
	invocations:           Option<Vec<Invocation>>,
	artifacts:             Option<Vec<Artifact>>,
	original_uri_base_ids: Option<BTreeMap<Str, ArtifactLocation>>,
}

impl Run {
	fn driver(&self) -> Option<&Component> {
		self.tool.as_ref()?.driver.as_ref()
	}

	fn rules(&self) -> &[Rule] {
		self
			.driver()
			.and_then(|driver| driver.rules.as_deref())
			.unwrap_or_default()
	}

	/// Why this run's results cannot stand for a complete analysis, if they
	/// cannot: §3.14.23 makes null or absent `results` a failed start, and
	/// §3.20.14 `executionSuccessful: false` a failed run.
	fn failure(&self) -> Option<Str> {
		let reason = if self.results.is_none() {
			"has no results (null or absent): the tool failed to start or to begin its analysis"
		} else if self
			.invocations
			.iter()
			.flatten()
			.any(|invocation| invocation.execution_successful == Some(false))
		{
			"reports executionSuccessful false"
		} else {
			return None;
		};
		let tool = self
			.driver()
			.and_then(|driver| driver.name.as_deref())
			.unwrap_or("an unnamed tool");
		let mut message = format!("SARIF run of {tool} {reason}");
		for invocation in self.invocations.iter().flatten() {
			if let Some(text) = invocation.process_start_failure_message.as_deref() {
				let _ = write!(message, "; {text}");
			}
			for notification in invocation.tool_execution_notifications.iter().flatten() {
				if notification.level == Some(Level::Error)
					&& let Some(text) = notification.message.as_ref().and_then(Message::text)
				{
					let _ = write!(message, "; {text}");
				}
			}
		}
		Some(Str::from(message))
	}

	fn path(
		&self,
		location: &ArtifactLocation,
		input: &ParserInput<'_>,
		root: Option<&Url>,
	) -> Option<Str> {
		let location = if location.uri.is_some() {
			location
		} else {
			let index = usize::try_from(location.index?).ok()?;
			self.artifacts.as_deref()?.get(index)?.location.as_ref()?
		};
		let uri = location
			.uri
			.as_deref()
			.filter(|uri| !uri.trim().is_empty())?;
		let url = self.resolve(uri, location.uri_base_id.as_deref(), root, 0)?;
		if url.scheme() != "file" {
			return None;
		}
		relative_path(&url.to_file_path().ok()?.to_string_lossy(), input)
	}

	/// Resolves `uri` through its `uriBaseId` chain in `originalUriBaseIds`
	/// (§3.4.4). A relative reference with no declared base resolves against
	/// `root`, the checker's working directory.
	fn resolve(
		&self,
		uri: &str,
		base_id: Option<&str>,
		root: Option<&Url>,
		depth: u8,
	) -> Option<Url> {
		match Url::parse(uri) {
			Ok(url) => Some(url),
			Err(url::ParseError::RelativeUrlWithoutBase) => {
				let base = base_id
					.filter(|_| depth < MAX_BASE_DEPTH)
					.and_then(|id| self.original_uri_base_ids.as_ref()?.get(id))
					.and_then(|base| {
						self.resolve(base.uri.as_deref()?, base.uri_base_id.as_deref(), root, depth + 1)
					})
					.map(directory);
				match base {
					Some(base) => base.join(uri).ok(),
					None => root?.join(uri).ok(),
				}
			},
			Err(_) => None,
		}
	}
}

#[derive(serde::Deserialize)]
struct Tool {
	driver: Option<Component>,
}

#[derive(serde::Deserialize)]
struct Component {
	name:  Option<Str>,
	rules: Option<Vec<Rule>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
	id:                    Option<Str>,
	short_description:     Option<Message>,
	default_configuration: Option<Configuration>,
}

#[derive(serde::Deserialize)]
struct Configuration {
	level: Option<Level>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
	rule_id:        Option<Str>,
	rule_index:     Option<i64>,
	rule:           Option<RuleReference>,
	kind:           Option<Kind>,
	level:          Option<Level>,
	message:        Option<Message>,
	locations:      Option<Vec<Location>>,
	suppressions:   Option<Vec<Suppression>>,
	baseline_state: Option<BaselineState>,
	fixes:          Option<Vec<Fix>>,
}

impl SarifResult {
	/// Whether the log reports this result as a current, unsuppressed problem:
	/// §3.27.9 `kind`, §3.27.23 `suppressions` and §3.27.24 `baselineState`
	/// each mark results that are not one.
	fn is_problem(&self) -> bool {
		!matches!(self.kind, Some(Kind::Pass | Kind::NotApplicable | Kind::Informational))
			&& self.baseline_state != Some(BaselineState::Absent)
			&& !self.suppressions.iter().flatten().any(|suppression| {
				matches!(suppression.status, None | Some(SuppressionStatus::Accepted))
			})
	}

	fn descriptor<'r>(&self, rules: &'r [Rule]) -> Option<&'r Rule> {
		self
			.rule_index
			.or_else(|| self.rule.as_ref()?.index)
			.and_then(|index| usize::try_from(index).ok())
			.and_then(|index| rules.get(index))
			.or_else(|| {
				let id = self
					.rule_id
					.as_deref()
					.or_else(|| self.rule.as_ref()?.id.as_deref())?;
				rules.iter().find(|rule| rule.id.as_deref() == Some(id))
			})
	}

	fn diagnostic(
		&self,
		run: &Run,
		rules: &[Rule],
		input: &ParserInput<'_>,
		root: Option<&Url>,
	) -> Diagnostic {
		let descriptor = self.descriptor(rules);
		let code = self
			.rule_id
			.as_ref()
			.or_else(|| self.rule.as_ref()?.id.as_ref())
			.or_else(|| descriptor?.id.as_ref());
		let message = self
			.message
			.as_ref()
			.and_then(Message::text)
			.or_else(|| descriptor?.short_description.as_ref()?.text())
			.or(code)
			.map_or_else(|| sf!("SARIF result without a message"), Str::trim);
		let physical = self
			.locations
			.iter()
			.flatten()
			.find_map(|location| location.physical_location.as_ref());
		let region = physical.and_then(|physical| physical.region.as_ref());
		let position =
			|pick: fn(&Region) -> Option<u32>| region.and_then(pick).filter(|value| *value > 0);
		Diagnostic {
			checker: Str::from(input.checker),
			file: physical
				.and_then(|physical| physical.artifact_location.as_ref())
				.and_then(|location| run.path(location, input, root)),
			line: position(|region| region.start_line),
			column: position(|region| region.start_column),
			end_line: position(|region| region.end_line),
			end_column: position(|region| region.end_column),
			code: code.cloned(),
			severity: self.severity(descriptor),
			message,
			suggestion: self
				.fixes
				.iter()
				.flatten()
				.find_map(|fix| fix.description.as_ref()?.text())
				.map(Str::trim),
		}
	}

	/// §3.27.10: an absent level comes from the rule's default configuration,
	/// else `warning`, unless `kind` marks the result as no failure (`none`).
	fn severity(&self, descriptor: Option<&Rule>) -> Severity {
		let level = self.level.unwrap_or_else(|| match self.kind {
			None | Some(Kind::Fail) => descriptor
				.and_then(|rule| rule.default_configuration.as_ref()?.level)
				.unwrap_or(Level::Warning),
			Some(_) => Level::None,
		});
		match level {
			Level::Error => Severity::Error,
			Level::Warning => Severity::Warning,
			Level::Note | Level::None => Severity::Info,
		}
	}
}

#[derive(serde::Deserialize)]
struct RuleReference {
	id:    Option<Str>,
	index: Option<i64>,
}

#[derive(serde::Deserialize)]
struct Message {
	text:     Option<Str>,
	markdown: Option<Str>,
}

impl Message {
	fn text(&self) -> Option<&Str> {
		[&self.text, &self.markdown]
			.into_iter()
			.flatten()
			.find(|text| !text.trim().is_empty())
	}
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Location {
	physical_location: Option<PhysicalLocation>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhysicalLocation {
	artifact_location: Option<ArtifactLocation>,
	region:            Option<Region>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactLocation {
	uri:         Option<Str>,
	uri_base_id: Option<Str>,
	index:       Option<i64>,
}

#[derive(serde::Deserialize)]
struct Artifact {
	location: Option<ArtifactLocation>,
}

/// One-based lines and columns (§3.30), so zero means none.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Region {
	start_line:   Option<u32>,
	start_column: Option<u32>,
	end_line:     Option<u32>,
	end_column:   Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Invocation {
	execution_successful:          Option<bool>,
	process_start_failure_message: Option<Str>,
	tool_execution_notifications:  Option<Vec<Notification>>,
}

#[derive(serde::Deserialize)]
struct Notification {
	level:   Option<Level>,
	message: Option<Message>,
}

#[derive(serde::Deserialize)]
struct Suppression {
	status: Option<SuppressionStatus>,
}

#[derive(serde::Deserialize)]
struct Fix {
	description: Option<Message>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum Level {
	None,
	Note,
	Warning,
	Error,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum Kind {
	Pass,
	Open,
	Informational,
	NotApplicable,
	Review,
	Fail,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum BaselineState {
	New,
	Unchanged,
	Updated,
	Absent,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum SuppressionStatus {
	Accepted,
	UnderReview,
	Rejected,
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;

	const RUFF: &str = include_str!("../../../../../fixtures/sarif/ruff.sarif");
	const BIOME: &str = include_str!("../../../../../fixtures/sarif/biome.sarif");
	const GOLANGCI: &str = include_str!("../../../../../fixtures/sarif/golangci-lint.sarif");

	type Row<'a> = (Option<&'a str>, Option<u32>, Option<u32>, Option<&'a str>, Severity, &'a str);

	fn run(
		checker_root: &str,
		project_root: &str,
		stdout: &str,
	) -> Result<Vec<Diagnostic>, SarifError> {
		parse(&ParserInput {
			checker: "sarif",
			checker_root: Path::new(checker_root),
			project_root: Path::new(project_root),
			stdout,
			stderr: "",
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
					diagnostic.severity,
					diagnostic.message.as_str(),
				)
			})
			.collect()
	}

	fn results(results: &str) -> String {
		format!(
			r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"lint"}}}},"results":[{results}]}}]}}"#
		)
	}

	#[test]
	fn ruff_file_uris_relativize_and_keep_fixes() {
		let root = "/private/tmp/sarif-work/ruff";
		let diagnostics = run(root, root, RUFF).expect("ruff log");
		assert_eq!(rows(&diagnostics), [
			(
				Some("broken.py"),
				Some(1),
				Some(12),
				Some("invalid-syntax"),
				Severity::Error,
				"Expected a parameter or the end of the parameter list"
			),
			(
				Some("broken.py"),
				Some(1),
				Some(13),
				Some("invalid-syntax"),
				Severity::Error,
				"Expected `)`, found newline"
			),
			(
				Some("outcomes.py"),
				Some(1),
				Some(8),
				Some("F401"),
				Severity::Error,
				"`os` imported but unused"
			),
			(
				Some("outcomes.py"),
				Some(2),
				Some(8),
				Some("F401"),
				Severity::Error,
				"`sys` imported but unused"
			),
			(
				Some("outcomes.py"),
				Some(6),
				Some(12),
				Some("F821"),
				Severity::Error,
				"Undefined name `pi`"
			),
			(
				Some("outcomes.py"),
				Some(10),
				Some(5),
				Some("F841"),
				Severity::Error,
				"Local variable `unused` is assigned to but never used"
			),
		]);
		assert_eq!((diagnostics[1].end_line, diagnostics[1].end_column), (Some(2), Some(1)));
		assert_eq!(diagnostics[2].suggestion.as_deref(), Some("Remove unused import: `os`"));
		assert_eq!(diagnostics[4].suggestion, None);
	}

	#[test]
	fn biome_bare_paths_resolve_and_note_maps_to_info() {
		let root = "/private/tmp/sarif-work/biome";
		assert_eq!(rows(&run(root, root, BIOME).expect("biome log")), [
			(
				Some("biome.json"),
				Some(2),
				Some(13),
				Some("deserialize"),
				Severity::Info,
				"The use of the recommended field has been deprecated, and will removed in the next \
				 major version of Biome. Use preset instead."
			),
			(
				Some("outcomes.js"),
				Some(2),
				Some(9),
				Some("lint/correctness/noUnusedVariables"),
				Severity::Warning,
				"This variable unused is unused."
			),
			(
				Some("broken.js"),
				Some(2),
				Some(10),
				Some("parse"),
				Severity::Error,
				"expected `:` but instead found `1`"
			),
			(
				Some("broken.js"),
				Some(2),
				Some(11),
				Some("parse"),
				Severity::Error,
				"expected `:` but instead found `;`"
			),
			(
				Some("broken.js"),
				Some(3),
				Some(1),
				Some("parse"),
				Severity::Error,
				"Expected a statement but instead found '}'."
			),
			(
				Some("outcomes.js"),
				Some(3),
				Some(9),
				Some("lint/suspicious/noDoubleEquals"),
				Severity::Error,
				"Using == may be unsafe if you are relying on type coercion."
			),
			(
				Some("outcomes.js"),
				Some(4),
				Some(5),
				Some("lint/suspicious/noDebugger"),
				Severity::Error,
				"This is an unexpected use of the debugger statement."
			),
		]);
	}

	/// golangci-lint writes relative URIs with a dangling `index: 0` and no
	/// `artifacts` array; the URI wins and resolves against the checker root.
	#[test]
	fn golangci_relative_uris_resolve_against_the_checker_root() {
		let root = "/private/tmp/sarif-work/golangci";
		let diagnostics = run(root, root, GOLANGCI).expect("golangci-lint log");
		assert_eq!(rows(&diagnostics), [
			(
				Some("outcomes.go"),
				Some(9),
				Some(11),
				Some("errcheck"),
				Severity::Error,
				"Error return value of `os.Remove` is not checked"
			),
			(
				Some("outcomes.go"),
				Some(10),
				Some(14),
				Some("govet"),
				Severity::Error,
				"printf: fmt.Printf format %d has arg path of wrong type string"
			),
			(
				Some("outcomes.go"),
				Some(13),
				Some(6),
				Some("unused"),
				Severity::Error,
				"func helper is unused"
			),
		]);
		assert_eq!(diagnostics[0].end_line, None);
	}

	#[test]
	fn missing_location_levels_keep_the_message_without_a_file() {
		let log = results(concat!(
			r#"{"ruleId":"A","level":"error","message":{"text":"no locations"}},"#,
			r#"{"ruleId":"B","message":{"text":"logical only"},"locations":[{"logicalLocations":[{"name":"main"}]}]},"#,
			r#"{"ruleId":"C","level":"note","message":{"text":"region only"},"locations":[{"physicalLocation":{"region":{"startLine":3}}}]},"#,
			r#"{"ruleId":"D","level":"error","message":{"text":"dangling index"},"locations":[{"physicalLocation":{"artifactLocation":{"index":4}}}]},"#,
			r#"{"ruleId":"E","level":"error","message":{"text":"no region"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/lib.rs"}}}]},"#,
			r#"{"ruleId":"F","level":"error","message":{"text":"zero line"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/lib.rs"},"region":{"startLine":0,"startColumn":2}}}]},"#,
			r#"{"ruleId":"G","level":"error","message":{"text":"remote artifact"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"https://example.com/lib.rs"},"region":{"startLine":1}}}]}"#,
		));
		assert_eq!(rows(&run("/repo", "/repo", &log).expect("log")), [
			(None, None, None, Some("A"), Severity::Error, "no locations"),
			(None, None, None, Some("B"), Severity::Warning, "logical only"),
			(None, Some(3), None, Some("C"), Severity::Info, "region only"),
			(None, None, None, Some("D"), Severity::Error, "dangling index"),
			(Some("src/lib.rs"), None, None, Some("E"), Severity::Error, "no region"),
			(Some("src/lib.rs"), None, Some(2), Some("F"), Severity::Error, "zero line"),
			(None, Some(1), None, Some("G"), Severity::Error, "remote artifact"),
		]);
	}

	#[test]
	fn message_rule_and_level_fall_back_level_by_level() {
		let log = concat!(
			r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint","rules":["#,
			r#"{"id":"R0","shortDescription":{"text":"rule zero summary"},"defaultConfiguration":{"level":"error"}},"#,
			r#"{"id":"R1"}]}},"results":["#,
			r#"{"ruleIndex":0,"message":{}},"#,
			r#"{"rule":{"id":"R1"},"message":{"markdown":"**markdown** only"}},"#,
			r#"{"ruleId":"R0","kind":"open","message":{"text":"needs a decision"}},"#,
			r#"{"message":{"text":"   "}},"#,
			r#"{"ruleId":"X","message":{"id":"default","arguments":["a"]}}"#,
			r#"]}]}"#,
		);
		assert_eq!(rows(&run("/repo", "/repo", log).expect("log")), [
			(None, None, None, Some("R0"), Severity::Error, "rule zero summary"),
			(None, None, None, Some("R1"), Severity::Warning, "**markdown** only"),
			(None, None, None, Some("R0"), Severity::Info, "needs a decision"),
			(None, None, None, None, Severity::Warning, "SARIF result without a message"),
			(None, None, None, Some("X"), Severity::Warning, "X"),
		]);
	}

	#[test]
	fn results_the_log_marks_as_no_problem_are_skipped() {
		let log = results(concat!(
			r#"{"kind":"pass","message":{"text":"pass"}},"#,
			r#"{"kind":"notApplicable","message":{"text":"not applicable"}},"#,
			r#"{"kind":"informational","message":{"text":"informational"}},"#,
			r#"{"message":{"text":"in-source suppression"},"suppressions":[{"kind":"inSource"}]},"#,
			r#"{"message":{"text":"accepted suppression"},"suppressions":[{"kind":"external","status":"accepted"}]},"#,
			r#"{"message":{"text":"fixed since baseline"},"baselineState":"absent"},"#,
			r#"{"message":{"text":"rejected suppression"},"suppressions":[{"kind":"inSource","status":"rejected"}]},"#,
			r#"{"message":{"text":"no suppressions"},"suppressions":[]},"#,
			r#"{"kind":"review","message":{"text":"review"},"baselineState":"new"}"#,
		));
		assert_eq!(rows(&run("/repo", "/repo", &log).expect("log")), [
			(None, None, None, None, Severity::Warning, "rejected suppression"),
			(None, None, None, None, Severity::Warning, "no suppressions"),
			(None, None, None, None, Severity::Info, "review"),
		]);
	}

	#[test]
	fn uri_base_ids_resolve_through_the_declared_chain() {
		let log = concat!(
			r#"{"version":"2.1.0","runs":[{"originalUriBaseIds":{"SRC":{"uri":"file:///repo/src"},"PKG":{"uri":"pkg/","uriBaseId":"SRC"}},"#,
			r#""artifacts":[{"location":{"uri":"indexed.rs","uriBaseId":"SRC"}}],"results":["#,
			r#"{"message":{"text":"declared base"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"lib.rs","uriBaseId":"SRC"}}}]},"#,
			r#"{"message":{"text":"chained base"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"mod.rs","uriBaseId":"PKG"}}}]},"#,
			r#"{"message":{"text":"undeclared base"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"main.rs","uriBaseId":"%SRCROOT%"}}}]},"#,
			r#"{"message":{"text":"percent encoded"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"my%20file.rs"}}}]},"#,
			r#"{"message":{"text":"artifact index"},"locations":[{"physicalLocation":{"artifactLocation":{"index":0}}}]}"#,
			r#"]}]}"#,
		);
		let diagnostics = run("/repo/tool", "/repo", log).expect("log");
		let files = diagnostics
			.iter()
			.map(|diagnostic| (diagnostic.file.as_deref(), diagnostic.message.as_str()))
			.collect::<Vec<_>>();
		assert_eq!(files, [
			(Some("src/lib.rs"), "declared base"),
			(Some("src/pkg/mod.rs"), "chained base"),
			(Some("tool/main.rs"), "undeclared base"),
			(Some("tool/my file.rs"), "percent encoded"),
			(Some("src/indexed.rs"), "artifact index"),
		]);
	}

	/// A tool that failed reports it inside a well-formed log; the run becomes
	/// a file-less failure instead of an empty, clean-looking result.
	#[test]
	fn failed_runs_never_read_as_clean() {
		let failed = |log: &str| {
			run("/repo", "/repo", log)
				.expect("log")
				.into_iter()
				.map(|diagnostic| (diagnostic.code, diagnostic.message))
				.collect::<Vec<_>>()
		};
		let expected = |message: &'static str| (Some(sf!(RUN_FAILED)), sf!(message));
		assert_eq!(failed(r#"{"version":"2.1.0","runs":null}"#), [expected(
			"SARIF log has no runs: its producer failed to populate them (runs is null or absent)"
		)]);
		assert_eq!(
			failed(
				r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint"}},"invocations":[{"executionSuccessful":false,"processStartFailureMessage":"lint: not found"}]}]}"#
			),
			[expected(
				"SARIF run of lint has no results (null or absent): the tool failed to start or to \
				 begin its analysis; lint: not found"
			)]
		);
		assert_eq!(
			failed(concat!(
				r#"{"version":"2.1.0","runs":[{"results":[],"invocations":[{"executionSuccessful":false,"toolExecutionNotifications":["#,
				r#"{"level":"error","message":{"text":"out of memory"}},{"level":"note","message":{"text":"ignored note"}}]}]}]}"#,
			)),
			[expected(
				"SARIF run of an unnamed tool reports executionSuccessful false; out of memory"
			)]
		);
		assert!(
			failed(
				r#"{"version":"2.1.0","runs":[{"results":[],"invocations":[{"executionSuccessful":true}]}]}"#
			)
			.is_empty()
		);
		assert!(failed(r#"{"version":"2.1.0","runs":[]}"#).is_empty());
	}

	#[test]
	fn malformed_logs_are_errors_not_partial_results() {
		let error = |stdout: &str| run("/repo", "/repo", stdout).expect_err("malformed log");
		assert!(matches!(error(" \n"), SarifError::Empty));
		assert!(matches!(error("{"), SarifError::Json(_)));
		assert!(matches!(error(r#"{"runs":[]}"#), SarifError::Json(_)));
		assert!(matches!(
			error(&results(r#"{"level":"critical","message":{"text":"unknown level"}}"#)),
			SarifError::Json(_)
		));
		assert!(matches!(
			error(r#"{"version":"2.0.0","runs":[]}"#),
			SarifError::Version { version } if version.as_str() == "2.0.0"
		));
		assert!(matches!(
			error(r#"{"version":"1.0.0","runs":[{"results":[{"message":"a 1.0 string message"}]}]}"#),
			SarifError::Version { version } if version.as_str() == "1.0.0"
		));
	}
}
