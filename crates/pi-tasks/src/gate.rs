//! Acceptance gates.
//!
//! Gates verify claims, never predictions: they run AFTER a phase against what
//! the envelope actually declared. A green gate names what it examined, so
//! "passed" is evidence rather than silence.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::envelope::Envelope;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
	pub item: String,
	pub ok:   bool,
	pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
	pub gate:   String,
	pub checks: Vec<Check>,
}

impl GateReport {
	pub fn new(gate: impl Into<String>) -> Self {
		Self { gate: gate.into(), checks: Vec::new() }
	}

	pub fn push(&mut self, item: impl Into<String>, ok: bool, note: impl Into<String>) {
		self
			.checks
			.push(Check { item: item.into(), ok, note: note.into() });
	}

	/// True when every check passed. A gate that examined nothing is the gate's
	/// own problem to report: both artifact gates fail on an empty claim rather
	/// than passing vacuously, because a phase must not clear a gate by
	/// declaring nothing.
	pub fn ok(&self) -> bool {
		self.checks.iter().all(|c| c.ok)
	}

	pub fn violations(&self) -> impl Iterator<Item = String> + '_ {
		self
			.checks
			.iter()
			.filter(|c| !c.ok)
			.map(|c| format!("{}: {} — {}", self.gate, c.item, c.note))
	}
}

pub struct GateCtx<'a> {
	pub root: &'a Path,
}

pub trait Gate: Send + Sync {
	fn name(&self) -> &'static str;
	fn run(&self, envelope: &Envelope<Value>, ctx: &GateCtx<'_>) -> GateReport;
	/// Called once when the attempt this gate just examined is accepted —
	/// gates green, envelope successful, and any required review decision
	/// present.
	///
	/// Default no-op. A stateful gate stages attempt-scoped evidence during
	/// [`Gate::run`] and commits it here, so a rejected attempt's declarations
	/// never leak into later evaluations.
	fn accept(&self, _envelope: &Envelope<Value>, _ctx: &GateCtx<'_>) {}
}

/// Every path the envelope claims to have produced must exist on disk.
pub struct ArtifactsExist;

impl Gate for ArtifactsExist {
	fn name(&self) -> &'static str {
		"artifacts_exist"
	}

	fn run(&self, envelope: &Envelope<Value>, ctx: &GateCtx<'_>) -> GateReport {
		let mut report = GateReport::new(self.name());
		if envelope.artifacts.is_empty() {
			// A gate with nothing to examine used to pass, which let a phase
			// clear it by claiming nothing at all — measured: an envelope with
			// `artifacts: []` passed both artifact gates. Requesting this gate
			// is an assertion that the phase produces files.
			report.push(".", false, "declared no artifacts, so there is nothing to verify");
			return report;
		}
		for artifact in &envelope.artifacts {
			let path = ctx.root.join(artifact);
			let exists = path.exists();
			report.push(artifact, exists, if exists { "exists" } else { "missing" });
		}
		report
	}
}

/// Claimed artifacts must carry bytes. Catches the `touch`-and-declare failure
/// that `artifacts_exist` alone accepts.
pub struct FilesNonEmpty;

impl Gate for FilesNonEmpty {
	fn name(&self) -> &'static str {
		"files_non_empty"
	}

	fn run(&self, envelope: &Envelope<Value>, ctx: &GateCtx<'_>) -> GateReport {
		let mut report = GateReport::new(self.name());
		if envelope.artifacts.is_empty() {
			report.push(".", false, "declared no artifacts, so there is nothing to verify");
			return report;
		}
		for artifact in &envelope.artifacts {
			let path = ctx.root.join(artifact);
			match std::fs::metadata(&path) {
				Ok(meta) if meta.len() > 0 => {
					report.push(artifact, true, format!("{} bytes", meta.len()));
				},
				Ok(_) => report.push(artifact, false, "empty file"),
				Err(err) => report.push(artifact, false, format!("unreadable: {err}")),
			}
		}
		report
	}
}

/// Every declared artifact that claims to be JSON must actually parse.
///
/// A phase that hands the next one `plan.json` has produced nothing useful if
/// the file is truncated or holds an apology instead of an object, and
/// `files_non_empty` is happy either way — bytes are not structure.
///
/// Scoped by extension rather than by position: which artifacts are JSON is
/// visible in the envelope, so the gate does not need to be told twice.
pub struct JsonParses;

impl Gate for JsonParses {
	fn name(&self) -> &'static str {
		"json_parses"
	}

	fn run(&self, envelope: &Envelope<Value>, ctx: &GateCtx<'_>) -> GateReport {
		let mut report = GateReport::new(self.name());
		let json: Vec<&String> = envelope
			.artifacts
			.iter()
			.filter(|path| path.rsplit('.').next() == Some("json"))
			.collect();
		if json.is_empty() {
			// Requesting this gate asserts the phase produces JSON. Passing here
			// would let a phase clear it by declaring no JSON at all, which is
			// the same vacuous hole the artifact gates had.
			report.push(".", false, "declared no .json artifact, so there is nothing to parse");
			return report;
		}
		for artifact in json {
			let path = ctx.root.join(artifact);
			match std::fs::read_to_string(&path) {
				Ok(text) => match serde_json::from_str::<Value>(&text) {
					Ok(_) => report.push(artifact, true, format!("parses ({} bytes)", text.len())),
					// The parse error carries the line and column, which is what
					// makes the correction actionable instead of "it is invalid".
					Err(err) => report.push(artifact, false, format!("invalid JSON: {err}")),
				},
				Err(err) => report.push(artifact, false, format!("unreadable: {err}")),
			}
		}
		report
	}
}

/// A named file must contain a marker, or must not.
///
/// The artifact gates ask whether a phase produced files; this asks what is
/// IN one, which is the only way a workflow can state a requirement about
/// content rather than existence. It is also what makes a contradictory pair
/// — the same marker required present and absent — expressible, and
/// therefore refusable: `file_contains` and `file_not_contains` on one file
/// can never both pass, whatever the producer writes.
///
/// Scoped to a declared path rather than to the envelope's artifact list: a
/// workflow asserting something about `README.md` must not be satisfied by a
/// phase that simply declines to declare it.
pub struct FileContains {
	pub file:    String,
	pub marker:  String,
	/// `true` inverts the assertion: the marker must be absent.
	pub negated: bool,
}

impl Gate for FileContains {
	fn name(&self) -> &'static str {
		if self.negated {
			"file_not_contains"
		} else {
			"file_contains"
		}
	}

	fn run(&self, _envelope: &Envelope<Value>, ctx: &GateCtx<'_>) -> GateReport {
		let mut report = GateReport::new(self.name());
		let path = ctx.root.join(&self.file);
		match std::fs::read_to_string(&path) {
			Ok(text) => {
				let found = text.contains(&self.marker);
				let ok = found != self.negated;
				let note = match (found, self.negated) {
					(true, false) => "marker present".to_owned(),
					(false, true) => "marker absent".to_owned(),
					(true, true) => format!("marker {:?} present but must not be", self.marker),
					(false, false) => format!("marker {:?} absent", self.marker),
				};
				report.push(&self.file, ok, note);
			},
			// An unreadable file fails either form. A missing file does not
			// "not contain" the marker in any useful sense: the assertion was
			// about a file that was supposed to be there.
			Err(err) => report.push(&self.file, false, format!("unreadable: {err}")),
		}
		report
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{envelope::EnvelopeStatus, test_support::TempDir};

	fn envelope(artifacts: &[&str]) -> Envelope<Value> {
		Envelope {
			status:               EnvelopeStatus::Success,
			summary:              String::new(),
			artifacts:            artifacts.iter().map(|s| (*s).to_string()).collect(),
			notes_for_next_agent: String::new(),
			payload:              Value::Object(serde_json::Map::new()),
		}
	}

	#[test]
	fn missing_artifact_fails_and_names_itself() {
		let dir = TempDir::new("gate-missing");
		let report = ArtifactsExist.run(&envelope(&["specs/plan.md"]), &GateCtx { root: dir.path() });
		assert!(!report.ok());
		let violations: Vec<_> = report.violations().collect();
		assert_eq!(violations.len(), 1);
		assert!(violations[0].contains("specs/plan.md"), "{violations:?}");
	}

	#[test]
	fn existing_artifact_passes() {
		let dir = TempDir::new("gate-present");
		dir.write("plan.md", "content");
		let report = ArtifactsExist.run(&envelope(&["plan.md"]), &GateCtx { root: dir.path() });
		assert!(report.ok());
	}

	#[test]
	fn empty_file_passes_existence_but_fails_non_empty() {
		let dir = TempDir::new("gate-empty");
		dir.write("plan.md", "");
		let ctx = GateCtx { root: dir.path() };
		let env = envelope(&["plan.md"]);
		assert!(ArtifactsExist.run(&env, &ctx).ok());
		assert!(!FilesNonEmpty.run(&env, &ctx).ok());
	}

	#[test]
	fn claiming_nothing_does_not_clear_the_gate() {
		// This test previously asserted the opposite — that an empty claim
		// passes vacuously — and a live run showed what that buys: a fuser
		// returned `artifacts: []` and cleared both artifact gates while writing
		// no file. Requesting the gate is an assertion that files are produced.
		let dir = TempDir::new("gate-none");
		for report in [
			ArtifactsExist.run(&envelope(&[]), &GateCtx { root: dir.path() }),
			FilesNonEmpty.run(&envelope(&[]), &GateCtx { root: dir.path() }),
		] {
			assert!(!report.ok(), "{}: an empty claim must not pass", report.gate);
			assert!(
				report
					.violations()
					.any(|v| v.contains("declared no artifacts")),
				"{}: the violation has to say why, not just fail",
				report.gate
			);
		}
	}

	#[test]
	fn invalid_json_fails_and_carries_the_parse_position() {
		// "it is invalid" is not actionable; the line and column are.
		let dir = TempDir::new("gate-json-bad");
		std::fs::write(dir.path().join("plan.json"), "{\"a\": 1,}").expect("write");
		let report = JsonParses.run(&envelope(&["plan.json"]), &GateCtx { root: dir.path() });
		assert!(!report.ok());
		let violation = report.violations().next().expect("a violation");
		assert!(violation.contains("plan.json"), "{violation}");
		assert!(violation.contains("line") || violation.contains("column"), "{violation}");
	}

	#[test]
	fn valid_json_passes_and_non_json_artifacts_are_left_alone() {
		let dir = TempDir::new("gate-json-ok");
		std::fs::write(dir.path().join("plan.json"), "{\"a\": 1}").expect("write");
		// Deliberately unparseable as JSON: this gate must not look at it.
		std::fs::write(dir.path().join("notes.md"), "# not json").expect("write");
		let report =
			JsonParses.run(&envelope(&["plan.json", "notes.md"]), &GateCtx { root: dir.path() });
		assert!(report.ok(), "{:?}", report.violations().collect::<Vec<_>>());
		assert_eq!(report.checks.len(), 1, "only the .json artifact is examined");
	}

	#[test]
	fn bytes_are_not_structure() {
		// The gap this gate fills: files_non_empty is happy with an apology.
		let dir = TempDir::new("gate-json-prose");
		std::fs::write(dir.path().join("plan.json"), "I could not produce the plan.").expect("write");
		let ctx = GateCtx { root: dir.path() };
		assert!(
			FilesNonEmpty.run(&envelope(&["plan.json"]), &ctx).ok(),
			"files_non_empty accepts prose"
		);
		assert!(!JsonParses.run(&envelope(&["plan.json"]), &ctx).ok(), "json_parses must not");
	}

	#[test]
	fn declaring_no_json_does_not_clear_the_gate() {
		let dir = TempDir::new("gate-json-none");
		std::fs::write(dir.path().join("notes.md"), "# notes").expect("write");
		let report = JsonParses.run(&envelope(&["notes.md"]), &GateCtx { root: dir.path() });
		assert!(!report.ok(), "requesting the gate asserts the phase produces JSON");
		assert!(report.violations().any(|v| v.contains("no .json artifact")));
	}

	#[test]
	fn contradictory_content_assertions_can_never_both_pass() {
		// WI-0024: the canonical fail-closed case. Whatever the producer
		// writes, one of the pair must be red — the property has to hold for
		// every possible content, not just the two obvious ones.
		let dir = TempDir::new("gate-contradiction");
		let marker = "__FACTORY_FAIL_CLOSED_CONTRADICTION_0024__";
		let env = envelope(&["marker.txt"]);
		let ctx = GateCtx { root: dir.path() };
		let present = FileContains {
			file:    "marker.txt".to_owned(),
			marker:  marker.to_owned(),
			negated: false,
		};
		let absent = FileContains {
			file:    "marker.txt".to_owned(),
			marker:  marker.to_owned(),
			negated: true,
		};

		for content in [format!("prefix {marker} suffix"), "nothing here".to_owned(), String::new()] {
			dir.write("marker.txt", &content);
			let both_green = present.run(&env, &ctx).ok() && absent.run(&env, &ctx).ok();
			assert!(!both_green, "both assertions passed for content {content:?}");
		}

		// A missing file fails BOTH: the assertion was about a file that was
		// supposed to exist, so its absence is not a way to satisfy the
		// negative form.
		std::fs::remove_file(dir.path().join("marker.txt")).expect("remove");
		assert!(!present.run(&env, &ctx).ok());
		assert!(!absent.run(&env, &ctx).ok());
	}
}
