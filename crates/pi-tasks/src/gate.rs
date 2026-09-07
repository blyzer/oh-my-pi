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
	pub ok: bool,
	pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
	pub gate: String,
	pub checks: Vec<Check>,
}

impl GateReport {
	pub fn new(gate: impl Into<String>) -> Self {
		Self { gate: gate.into(), checks: Vec::new() }
	}

	pub fn push(&mut self, item: impl Into<String>, ok: bool, note: impl Into<String>) {
		self.checks.push(Check { item: item.into(), ok, note: note.into() });
	}

	/// True when every check passed. A gate that examined nothing is the gate's
	/// own problem to report: both artifact gates fail on an empty claim rather
	/// than passing vacuously, because a phase must not clear a gate by
	/// declaring nothing.
	pub fn ok(&self) -> bool {
		self.checks.iter().all(|c| c.ok)
	}

	pub fn violations(&self) -> impl Iterator<Item = String> + '_ {
		self.checks
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
				Ok(meta) if meta.len() > 0 => report.push(artifact, true, format!("{} bytes", meta.len())),
				Ok(_) => report.push(artifact, false, "empty file"),
				Err(err) => report.push(artifact, false, format!("unreadable: {err}")),
			}
		}
		report
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::envelope::EnvelopeStatus;
	use crate::test_support::TempDir;

	fn envelope(artifacts: &[&str]) -> Envelope<Value> {
		Envelope {
			status: EnvelopeStatus::Success,
			summary: String::new(),
			artifacts: artifacts.iter().map(|s| (*s).to_string()).collect(),
			notes_for_next_agent: String::new(),
			payload: Value::Object(serde_json::Map::new()),
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
				report.violations().any(|v| v.contains("declared no artifacts")),
				"{}: the violation has to say why, not just fail",
				report.gate
			);
		}
	}
}
