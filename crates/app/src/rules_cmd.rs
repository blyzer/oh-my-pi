//! `omp rules`: present stream-rule inspection composed by
//! [`omp_driver::rules::stream`].
//!
//! Discovery, compilation, and matching all happen in the driver over the
//! stream-rules Director's own matcher; this module only reads inputs and
//! renders text or JSON.

use std::{
	io::{self, Read as _, Write},
	path::Path,
};

use miette::{IntoDiagnostic as _, Result};
use omp_core::Str;
use omp_driver::{
	rules::stream::{Finding, ProbeTarget, ScanOptions, ScanReport, StreamRuleInspector},
	subagent::AgentName,
};
use serde_json::json;

use crate::cli::{RulesArgs, RulesCommand, RulesSourceArg};

/// Runs one `omp rules` operation.
///
/// # Errors
///
/// Returns discovery, unknown-rule, read, walk, and output failures.
pub fn run(args: RulesArgs) -> Result<()> {
	let project = args.project.canonicalize().into_diagnostic()?;
	let con = crate::process_ctx(&project)?;
	let data_dir = omp_core::dirs::data_dir(None).into_diagnostic()?;
	let inspector =
		StreamRuleInspector::discover(&data_dir, &project, args.agent.map(AgentName::from), &con)
			.into_diagnostic()?;
	let mut out = io::stdout().lock();
	match args.command.unwrap_or(RulesCommand::List { json: false }) {
		RulesCommand::List { json } => list(&inspector, json, &mut out),
		RulesCommand::Test { snippet, file, rule, source, tool, path, verbose, json } => {
			let text = match (snippet, file) {
				(Some(snippet), _) => snippet,
				(None, Some(file)) if file == Path::new("-") => {
					let mut text = String::new();
					io::stdin().read_to_string(&mut text).into_diagnostic()?;
					text
				},
				(None, Some(file)) => std::fs::read_to_string(&file).into_diagnostic()?,
				(None, None) => unreachable!("clap requires a snippet or --file"),
			};
			if let Some(rule) = &rule {
				inspector.require(rule).into_diagnostic()?;
			}
			let target = match source {
				RulesSourceArg::Text => ProbeTarget::Text,
				RulesSourceArg::Thinking => ProbeTarget::Thinking,
				RulesSourceArg::Tool => ProbeTarget::Tool { tool, paths: path.into_iter().collect() },
			};
			let findings = inspector.test(&target, &text, rule.as_deref());
			test(&inspector, &findings, verbose, json, &mut out)
		},
		RulesCommand::Scan { directory, rule, tool, no_gitignore, max_bytes, json } => {
			if let Some(rule) = &rule {
				inspector.require(rule).into_diagnostic()?;
			}
			let directory = directory.canonicalize().into_diagnostic()?;
			let report = inspector
				.scan(
					&project,
					&directory,
					&ScanOptions { tool, use_gitignore: !no_gitignore, max_bytes },
					rule.as_deref(),
				)
				.into_diagnostic()?;
			scan(&inspector, &report, json, &mut out)
		},
	}
}

/// Renders an error with its source chain on one line.
fn chain(error: &dyn std::error::Error) -> String {
	let mut text = error.to_string();
	let mut source = error.source();
	while let Some(cause) = source {
		text.push_str(": ");
		text.push_str(&cause.to_string());
		source = cause.source();
	}
	text
}

fn list(inspector: &StreamRuleInspector, json: bool, out: &mut impl Write) -> Result<()> {
	if json {
		let rules = inspector
			.rows()
			.map(|row| {
				json!({
					"name": row.rule.name.as_str(),
					"path": row.rule.path.to_string_lossy(),
					"conditions": row.rule.condition.iter().map(Str::as_str).collect::<Vec<_>>(),
					"compiled": row.compiled,
					"scope": row.rule.scope.iter().map(Str::as_str).collect::<Vec<_>>(),
					"globs": row.rule.globs.iter().map(Str::as_str).collect::<Vec<_>>(),
					"interruptMode": row.rule.interrupt_mode.as_deref(),
					"interrupt": <&'static str>::from(row.interrupt),
					"disabled": row.disabled,
				})
			})
			.collect::<Vec<_>>();
		let warnings = inspector
			.compile_warnings()
			.iter()
			.map(|warning| chain(warning))
			.chain(
				inspector
					.discovery_warnings()
					.iter()
					.map(|warning| format!("{}: {}", warning.path.display(), warning.message)),
			)
			.collect::<Vec<_>>();
		let value = json!({
			"agent": inspector.agent().as_str(),
			"enabled": inspector.enabled(),
			"interrupt": <&'static str>::from(inspector.policy()),
			"rules": rules,
			"warnings": warnings,
		});
		writeln!(out, "{}", serde_json::to_string_pretty(&value).into_diagnostic()?)
			.into_diagnostic()?;
		return Ok(());
	}
	if !inspector.enabled() {
		writeln!(out, "Stream rules are off (ai_stream_rules_enabled 0); live sessions match none.")
			.into_diagnostic()?;
	}
	let mut any = false;
	for row in inspector.rows() {
		any = true;
		let scope = if row.rule.scope.is_empty() {
			"text, tool".to_owned()
		} else {
			row.rule
				.scope
				.iter()
				.map(Str::as_str)
				.collect::<Vec<_>>()
				.join(", ")
		};
		let state = if row.disabled { " (disabled)" } else { "" };
		writeln!(
			out,
			"{}{state}\n  scope: {scope}\n  interrupt: {}\n  conditions: {}/{} compiled",
			row.rule.name,
			<&'static str>::from(row.interrupt),
			row.compiled,
			row.rule.condition.len(),
		)
		.into_diagnostic()?;
		for condition in &row.rule.condition {
			writeln!(out, "    {condition}").into_diagnostic()?;
		}
		if !row.rule.globs.is_empty() {
			let globs = row.rule.globs.iter().map(Str::as_str).collect::<Vec<_>>();
			writeln!(out, "  globs: {}", globs.join(", ")).into_diagnostic()?;
		}
		writeln!(out, "  path: {}", row.rule.path.display()).into_diagnostic()?;
	}
	if !any {
		writeln!(out, "No stream rules for agent `{}`.", inspector.agent()).into_diagnostic()?;
	}
	for warning in inspector.compile_warnings() {
		writeln!(out, "warning: {}", chain(warning)).into_diagnostic()?;
	}
	for warning in inspector.discovery_warnings() {
		writeln!(out, "warning: {}: {}", warning.path.display(), warning.message)
			.into_diagnostic()?;
	}
	Ok(())
}

fn finding_json(finding: &Finding<'_>) -> serde_json::Value {
	json!({
		"rule": finding.rule.name.as_str(),
		"condition": finding.rule.pattern.as_str(),
		"line": finding.line,
		"end": finding.end,
		"excerpt": finding.excerpt.as_str(),
		"action": action(finding),
	})
}

const fn action(finding: &Finding<'_>) -> &'static str {
	if finding.interrupts {
		"interrupt"
	} else {
		"note"
	}
}

fn test(
	inspector: &StreamRuleInspector,
	findings: &[Finding<'_>],
	verbose: bool,
	json: bool,
	out: &mut impl Write,
) -> Result<()> {
	if json {
		let rows = findings.iter().map(finding_json).collect::<Vec<_>>();
		writeln!(out, "{}", serde_json::to_string_pretty(&rows).into_diagnostic()?)
			.into_diagnostic()?;
		return Ok(());
	}
	if findings.is_empty() {
		writeln!(out, "No stream rule matched.").into_diagnostic()?;
	}
	for finding in findings {
		writeln!(
			out,
			"{} ({}) line {}: {}",
			finding.rule.name,
			action(finding),
			finding.line,
			finding.excerpt
		)
		.into_diagnostic()?;
		if verbose {
			for line in finding.rule.body.lines() {
				writeln!(out, "    {line}").into_diagnostic()?;
			}
		}
	}
	if !inspector.enabled() {
		writeln!(out, "Note: ai_stream_rules_enabled is 0, so live sessions would not match.")
			.into_diagnostic()?;
	}
	Ok(())
}

fn scan(
	inspector: &StreamRuleInspector,
	report: &ScanReport<'_>,
	json: bool,
	out: &mut impl Write,
) -> Result<()> {
	if json {
		let files = report
			.files
			.iter()
			.map(|file| {
				json!({
					"path": file.path.as_str(),
					"findings": file.findings.iter().map(finding_json).collect::<Vec<_>>(),
				})
			})
			.collect::<Vec<_>>();
		let value = json!({
			"scanned": report.scanned,
			"skipped": report.skipped,
			"files": files,
		});
		writeln!(out, "{}", serde_json::to_string_pretty(&value).into_diagnostic()?)
			.into_diagnostic()?;
		return Ok(());
	}
	for file in &report.files {
		for finding in &file.findings {
			writeln!(
				out,
				"{}:{}: {} ({}): {}",
				file.path,
				finding.line,
				finding.rule.name,
				action(finding),
				finding.excerpt
			)
			.into_diagnostic()?;
		}
	}
	let hits = report
		.files
		.iter()
		.map(|file| file.findings.len())
		.sum::<usize>();
	writeln!(
		out,
		"{hits} would-trigger event(s) in {} of {} file(s); {} non-UTF-8 file(s) skipped.",
		report.files.len(),
		report.scanned,
		report.skipped
	)
	.into_diagnostic()?;
	if !inspector.enabled() {
		writeln!(out, "Note: ai_stream_rules_enabled is 0, so live sessions would not match.")
			.into_diagnostic()?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use std::{path::PathBuf, sync::Arc};

	use omp_driver::{discovery::rules::ActiveRules, subagent::MAIN_AGENT};
	use omp_ext::claude_plugin::ClaudePlugins;

	use super::*;

	struct Fixture {
		_temp:   tempfile::TempDir,
		project: PathBuf,
		home:    PathBuf,
	}

	fn fixture() -> Fixture {
		let temp = tempfile::tempdir().expect("tempdir");
		let root = temp.path().canonicalize().expect("canonical");
		let project = root.join("project");
		let home = root.join("home");
		std::fs::create_dir_all(project.join(".omp/rules")).expect("rules dir");
		std::fs::create_dir_all(project.join(".git")).expect("repo");
		std::fs::create_dir_all(&home).expect("home");
		std::fs::write(
			project.join(".omp/rules/no-todo.md"),
			"---\ncondition: ['TODO', 'x(?=y)']\nscope: text\ninterruptMode: never\n---\nFinish it \
			 now.\n",
		)
		.expect("rule");
		Fixture { _temp: temp, project, home }
	}

	fn inspector(fixture: &Fixture) -> StreamRuleInspector {
		let rules = ActiveRules::discover(
			&fixture.project,
			&fixture.home,
			&fixture.home.join(".o2"),
			&ClaudePlugins::default(),
		);
		StreamRuleInspector::new(Arc::new(rules), MAIN_AGENT.to_owned(), &omp_con::Ctx::new())
	}

	fn rendered(write: impl FnOnce(&mut Vec<u8>) -> Result<()>) -> String {
		let mut out = Vec::new();
		write(&mut out).expect("render");
		String::from_utf8(out).expect("utf-8")
	}

	#[test]
	fn list_renders_scope_policy_conditions_and_compile_warnings() {
		let fixture = fixture();
		let inspector = inspector(&fixture);
		let text = rendered(|out| list(&inspector, false, out));
		assert!(text.starts_with("no-todo\n  scope: text\n  interrupt: never\n  conditions: 1/2 compiled\n    TODO\n    x(?=y)\n"), "{text}");
		assert!(
			text.contains(
				"warning: stream rule `no-todo` condition 1 `x(?=y)` does not compile for the \
				 streaming matcher: "
			),
			"{text}"
		);
		let json: serde_json::Value =
			serde_json::from_str(&rendered(|out| list(&inspector, true, out))).expect("json");
		assert_eq!(json["rules"][0]["name"], "no-todo");
		assert_eq!(json["rules"][0]["compiled"], 1);
		assert_eq!(json["rules"][0]["interrupt"], "never");
		assert_eq!(json["warnings"].as_array().map(Vec::len), Some(1));
	}

	#[test]
	fn test_and_scan_render_findings_from_the_shared_matcher() {
		let fixture = fixture();
		let inspector = inspector(&fixture);
		let findings = inspector.test(&ProbeTarget::Text, "fine\nleft a TODO here\n", None);
		let text = rendered(|out| test(&inspector, &findings, true, false, out));
		assert_eq!(text, "no-todo (note) line 2: left a TODO here\n    Finish it now.\n");
		let json: serde_json::Value =
			serde_json::from_str(&rendered(|out| test(&inspector, &findings, false, true, out)))
				.expect("json");
		assert_eq!(json[0]["action"], "note");
		assert_eq!(json[0]["line"], 2);

		let none = inspector.test(&ProbeTarget::Thinking, "TODO", None);
		assert_eq!(
			rendered(|out| test(&inspector, &none, false, false, out)),
			"No stream rule matched.\n"
		);

		let report = ScanReport::default();
		assert_eq!(
			rendered(|out| scan(&inspector, &report, false, out)),
			"0 would-trigger event(s) in 0 of 0 file(s); 0 non-UTF-8 file(s) skipped.\n"
		);
	}
}
