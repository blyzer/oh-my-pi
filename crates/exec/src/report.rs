//! [`BuildReport`] and [`BuildStatus`] (ADR 0047, Decision items 4 and 5).

use omp_core::Str;

// ── BuildReport ──────────────────────────────────────────────────────────────

/// Structured summary of one build/test run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
	/// Process exit code.
	pub exit_code:    i32,
	/// Total test cases executed.
	pub test_count:   u32,
	/// Passing test cases.
	pub passed:       u32,
	/// Failing test cases.
	pub failed:       u32,
	/// Wall-clock duration in milliseconds.
	pub duration_ms:  u64,
	/// Names of each failing test.
	pub failed_tests: Vec<Str>,
	/// Full captured stdout.
	pub raw_output:   Str,
}

impl BuildReport {
	/// Parse a `BuildReport` from cargo-nextest captured output.
	///
	/// Nextest prints a summary line of the form:
	/// ```text
	///     Summary [  1.234s] N tests run: P passed, F failed, S skipped
	/// ```
	/// This constructor finds that line and extracts the counters.  If the
	/// summary line is absent (e.g. the build failed before any test ran) all
	/// counts stay zero.
	///
	/// Individual failing tests are detected from lines that contain `FAIL`:
	/// ```text
	///     FAIL [  0.001s] <crate>::<module>::<test_name>
	/// ```
	pub fn from_nextest_output(exit_code: i32, stdout: &str, duration_ms: u64) -> Self {
		let mut test_count = 0u32;
		let mut passed = 0u32;
		let mut failed = 0u32;
		let mut failed_tests: Vec<Str> = Vec::new();

		for line in stdout.lines() {
			let trimmed = line.trim();

			// Summary: "N tests run: P passed, F failed"
			// Full nextest form: "Summary [...]  N tests run: P passed, F failed, S
			// skipped"
			if let Some(rest) = trimmed
				.strip_prefix("Summary")
				.and_then(|s| s.trim().strip_prefix('['))
				.and_then(|s| s.find(']').map(|i| &s[i + 1..]))
				.map(str::trim)
			{
				// rest = "N tests run: P passed, F failed, S skipped"
				if let Some(counts) = parse_summary(rest) {
					test_count = counts.0;
					passed = counts.1;
					failed = counts.2;
				}
			}

			// Failing test lines: "FAIL [...]  crate::path::test_name"
			if trimmed.starts_with("FAIL") {
				if let Some(name) = extract_test_name(trimmed) {
					failed_tests.push(Str::new(name));
				}
			}
		}

		BuildReport {
			exit_code,
			test_count,
			passed,
			failed,
			duration_ms,
			failed_tests,
			raw_output: Str::new(stdout),
		}
	}
}

/// Parse `"N tests run: P passed, F failed[, S skipped]"` → `(N, P, F)`.
fn parse_summary(s: &str) -> Option<(u32, u32, u32)> {
	// "N tests run: …"
	let (n_str, rest) = s.split_once(" tests run:")?;
	let n: u32 = n_str.trim().parse().ok()?;
	let passed = extract_counter(rest, "passed")?;
	let failed = extract_counter(rest, "failed")?;
	Some((n, passed, failed))
}

/// Extract `K` from a string containing `"K <label>"`.
fn extract_counter(s: &str, label: &str) -> Option<u32> {
	for part in s.split(',') {
		let part = part.trim();
		if let Some(rest) = part.strip_suffix(label) {
			return rest.trim().parse().ok();
		}
	}
	None
}

/// Extract a test name from a nextest FAIL line.
///
/// Lines look like:
/// ```text
/// FAIL [  0.001s] crate::mod::test_name
/// FAIL [  0.001s] crate::mod::test_name (property-based test)
/// ```
fn extract_test_name(line: &str) -> Option<&str> {
	// skip "FAIL", skip the "[…]" timing block, take the rest
	let rest = line.strip_prefix("FAIL")?.trim();
	let rest = if rest.starts_with('[') {
		rest.find(']').map(|i| rest[i + 1..].trim())?
	} else {
		rest
	};
	// strip any trailing parenthetical annotation
	let name = rest.split('(').next()?.trim();
	if name.is_empty() { None } else { Some(name) }
}

// ── BuildStatus ──────────────────────────────────────────────────────────────

/// Lifecycle state of one build job (ADR 0047 §5).
///
/// The state machine is linear until a terminal state:
/// ```text
/// Created → Queued → Preparing → Running → Succeeded(report)
///                                        → Failed(report)
///                                        → Cancelled
///                                        → TimedOut
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildStatus {
	/// The job record was created; not yet enqueued.
	Created,
	/// Waiting for a backend slot.
	Queued,
	/// Backend accepted; inputs being staged.
	Preparing,
	/// Build process is running.
	Running,
	/// Run completed with exit code 0.
	Succeeded(BuildReport),
	/// Run completed with a non-zero exit code or test failures.
	Failed(BuildReport),
	/// Cancelled before or during execution.
	Cancelled,
	/// Killed because it exceeded its timeout.
	TimedOut,
}

impl BuildStatus {
	/// Returns `true` for every terminal state.
	pub fn is_terminal(&self) -> bool {
		matches!(self, Self::Succeeded(_) | Self::Failed(_) | Self::Cancelled | Self::TimedOut)
	}
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	const NEXTEST_PASS: &str = r#"
       Compiling omp-envd v0.1.0
        Finished test [unoptimized + debuginfo]
        Running 12 tests
     Summary [  1.234s] 12 tests run: 12 passed, 0 failed, 0 skipped
"#;

	const NEXTEST_FAIL: &str = r#"
        Running 5 tests
     FAIL [  0.002s] omp_envd::workspace::tests::bad_path
     FAIL [  0.001s] omp_envd::workspace::tests::missing_file
     Summary [  0.456s] 5 tests run: 3 passed, 2 failed, 0 skipped
"#;

	#[test]
	fn parse_all_passing() {
		let r = BuildReport::from_nextest_output(0, NEXTEST_PASS, 1234);
		assert_eq!(r.exit_code, 0);
		assert_eq!(r.test_count, 12);
		assert_eq!(r.passed, 12);
		assert_eq!(r.failed, 0);
		assert!(r.failed_tests.is_empty());
	}

	#[test]
	fn parse_some_failing() {
		let r = BuildReport::from_nextest_output(1, NEXTEST_FAIL, 456);
		assert_eq!(r.test_count, 5);
		assert_eq!(r.passed, 3);
		assert_eq!(r.failed, 2);
		assert_eq!(r.failed_tests.len(), 2);
		assert_eq!(r.failed_tests[0].as_str(), "omp_envd::workspace::tests::bad_path");
		assert_eq!(r.failed_tests[1].as_str(), "omp_envd::workspace::tests::missing_file");
	}

	#[test]
	fn parse_no_summary() {
		let r = BuildReport::from_nextest_output(101, "error: could not compile", 0);
		assert_eq!(r.test_count, 0);
		assert_eq!(r.failed, 0);
		assert_eq!(r.exit_code, 101);
	}

	#[test]
	fn build_status_terminal() {
		assert!(!BuildStatus::Created.is_terminal());
		assert!(!BuildStatus::Queued.is_terminal());
		assert!(!BuildStatus::Preparing.is_terminal());
		assert!(!BuildStatus::Running.is_terminal());
		let report = BuildReport::from_nextest_output(0, "", 0);
		assert!(BuildStatus::Succeeded(report.clone()).is_terminal());
		assert!(BuildStatus::Failed(report).is_terminal());
		assert!(BuildStatus::Cancelled.is_terminal());
		assert!(BuildStatus::TimedOut.is_terminal());
	}
}
