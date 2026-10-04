//! Offline stream-rule inspection behind `omp rules`.
//!
//! [`StreamRuleInspector`] composes exactly what a kernel composes for the
//! stream-rules Director: [`ActiveRules::discover`] for the project, the
//! agent-class filter, [`ActiveRules::stream_patterns`], and
//! [`StreamRuleSet::compile`]. Matching then runs through
//! [`StreamRuleSet::probe`], which drives the Director's own automaton, scope
//! and path gates, and interrupt-policy resolution. Nothing here is a second
//! matcher; a difference between `omp rules test` and a live session is a bug
//! in one shared path, not drift between two.
//!
//! `scan` gives a file tree the smallest honest live meaning: each UTF-8 file
//! is probed as the authored text of one tool call (by default `write`) whose
//! target is the file's path relative to the project root. It answers "would
//! the model writing this file have tripped a rule", not "does this file
//! violate a rule" (rules scoped only to `text` or `thinking` never match a
//! scan).

use std::{
	convert::Infallible,
	io::{self, Read as _},
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_agent::{
	StreamSource,
	directors::stream_rules::{
		CompiledStreamRules, ProbeHit, RulePattern, StreamRuleSet, StreamRuleWarning,
	},
	vars::{
		AI_STREAM_RULES_DISABLED, AI_STREAM_RULES_ENABLED, AI_STREAM_RULES_INTERRUPT,
		StreamRuleInterrupt,
	},
};
use omp_core::Str;
use omp_ext::claude_plugin::{ClaudeCodeHome, ClaudePlugins};

use crate::{
	discovery::rules::{ActiveRules, Rule, Warning},
	subagent::{AgentName, MAIN_AGENT},
};

/// Bytes of matched line shown around a hit.
const EXCERPT_BYTES: usize = 160;

/// Why an inspection could not run.
#[derive(Debug, thiserror::Error)]
pub enum InspectError {
	/// Home or configuration directories are unavailable.
	#[error("rule discovery needs the home and configuration directories")]
	Dirs(#[from] omp_core::dirs::DataDirError),
	/// `--rule` names no admitted stream rule.
	#[error("no stream rule named `{name}` is admitted for agent `{agent}`")]
	UnknownRule {
		/// Requested rule.
		name:  Str,
		/// Agent class the rules were filtered for.
		agent: AgentName,
	},
	/// `--rule` names a rule `ai_stream_rules_disabled` turns off.
	#[error("stream rule `{name}` is disabled by ai_stream_rules_disabled")]
	Disabled {
		/// Requested rule.
		name: Str,
	},
	/// The scan root could not be walked.
	#[error("could not walk `{}`", root.display())]
	Walk {
		/// Scan root.
		root:   PathBuf,
		/// Walker failure.
		#[source]
		source: omp_walker::WalkError<Infallible>,
	},
	/// A scanned or tested file could not be read.
	#[error("could not read `{}`", path.display())]
	Read {
		/// File path.
		path:   PathBuf,
		/// I/O failure.
		#[source]
		source: io::Error,
	},
}

/// One discovered stream rule as `omp rules list` presents it.
#[derive(Clone, Copy, Debug)]
pub struct StreamRuleRow<'a> {
	/// The discovered rule document.
	pub rule:      &'a Rule,
	/// Conditions the streaming matcher accepted.
	pub compiled:  usize,
	/// Whether `ai_stream_rules_disabled` names it.
	pub disabled:  bool,
	/// Effective interrupt policy for text/thinking hits and tool hits:
	/// the rule's valid `interruptMode` override, else the session policy.
	pub interrupt: StreamRuleInterrupt,
}

/// What to probe: one stream source and the target paths of a tool source.
#[derive(Clone, Debug)]
pub enum ProbeTarget {
	/// Assistant-visible text.
	Text,
	/// Assistant reasoning.
	Thinking,
	/// Authored text of one call to `tool` writing to `paths`.
	Tool {
		/// Stable tool name (`edit`, `write`, ...).
		tool:  Str,
		/// Candidate target paths for path-gated rules.
		paths: Vec<Str>,
	},
}

impl ProbeTarget {
	fn source(&self) -> StreamSource<'_> {
		match self {
			Self::Text => StreamSource::Text,
			Self::Thinking => StreamSource::Thinking,
			Self::Tool { tool, .. } => StreamSource::ToolArgs { call_id: "", tool: tool.as_str() },
		}
	}

	fn paths(&self) -> &[Str] {
		match self {
			Self::Tool { paths, .. } => paths,
			Self::Text | Self::Thinking => &[],
		}
	}
}

/// One would-trigger event in probed text.
#[derive(Clone, Debug)]
pub struct Finding<'a> {
	/// Rule and condition that matched.
	pub rule:       &'a RulePattern,
	/// Byte offset where the match ended.
	pub end:        usize,
	/// One-based line of the match end.
	pub line:       usize,
	/// The matched line, bounded to a short excerpt.
	pub excerpt:    Str,
	/// Whether the live Director would redirect the response (else it
	/// records a note).
	pub interrupts: bool,
}

/// Findings for one scanned file.
#[derive(Clone, Debug)]
pub struct ScannedFile<'a> {
	/// Path relative to the project root (absolute outside it).
	pub path:     Str,
	/// Rules that file's authored text would trigger.
	pub findings: Vec<Finding<'a>>,
}

/// Options for [`StreamRuleInspector::scan`].
#[derive(Clone, Debug)]
pub struct ScanOptions {
	/// Tool whose authored text each file stands for.
	pub tool:          Str,
	/// Honor `.gitignore`/`.ignore` files.
	pub use_gitignore: bool,
	/// Maximum bytes probed per file.
	pub max_bytes:     u64,
}

/// The outcome of [`StreamRuleInspector::scan`].
#[derive(Clone, Debug, Default)]
pub struct ScanReport<'a> {
	/// Files with at least one finding, in path order.
	pub files:   Vec<ScannedFile<'a>>,
	/// Files probed.
	pub scanned: usize,
	/// Files skipped because they are not UTF-8 text.
	pub skipped: usize,
}

/// Discovered and compiled stream rules for one project and agent class.
pub struct StreamRuleInspector {
	rules:    Arc<ActiveRules>,
	agent:    AgentName,
	compiled: CompiledStreamRules,
	enabled:  bool,
	policy:   StreamRuleInterrupt,
	disabled: Vec<Str>,
}

impl StreamRuleInspector {
	/// Discovers rules for `project_root` the way kernel composition does
	/// (installed plugins resolved under `data_dir`) and compiles the stream
	/// conditions `agent` admits under the policy convars of `con`.
	///
	/// # Errors
	///
	/// Returns [`InspectError::Dirs`] when the home or configuration root is
	/// unavailable.
	pub fn discover(
		data_dir: &Path,
		project_root: &Path,
		agent: Option<AgentName>,
		con: &omp_con::Ctx,
	) -> Result<Self, InspectError> {
		let home = omp_core::dirs::home_dir().ok_or(omp_core::dirs::DataDirError::HomeUnset)?;
		let config_root = omp_core::dirs::user_config_root()?;
		let plugins =
			ClaudePlugins::resolve(data_dir, project_root, ClaudeCodeHome::detect().as_ref());
		let rules = ActiveRules::discover(project_root, &home, &config_root, &plugins);
		Ok(Self::new(Arc::new(rules), agent.unwrap_or_else(|| MAIN_AGENT.to_owned()), con))
	}

	/// Compiles the stream conditions of already discovered `rules` that
	/// `agent` admits.
	#[must_use]
	pub fn new(rules: Arc<ActiveRules>, agent: AgentName, con: &omp_con::Ctx) -> Self {
		let compiled = StreamRuleSet::compile(rules.stream_patterns(&agent));
		Self {
			rules,
			agent,
			compiled,
			enabled: AI_STREAM_RULES_ENABLED.get(con),
			policy: AI_STREAM_RULES_INTERRUPT.get(con),
			disabled: AI_STREAM_RULES_DISABLED.get(con),
		}
	}

	/// Agent class the rules were filtered for.
	#[must_use]
	pub const fn agent(&self) -> &AgentName {
		&self.agent
	}

	/// `ai_stream_rules_enabled`: whether live sessions watch at all.
	#[must_use]
	pub const fn enabled(&self) -> bool {
		self.enabled
	}

	/// `ai_stream_rules_interrupt`: the session interrupt policy.
	#[must_use]
	pub const fn policy(&self) -> StreamRuleInterrupt {
		self.policy
	}

	/// Malformed or colliding rule documents found during discovery.
	#[must_use]
	pub fn discovery_warnings(&self) -> &[Warning] {
		&self.rules.warnings
	}

	/// Conditions, globs, scopes, and overrides compilation left out.
	#[must_use]
	pub fn compile_warnings(&self) -> &[StreamRuleWarning] {
		&self.compiled.warnings
	}

	/// Admitted rules that carry a `condition`, in discovery order.
	pub fn rows(&self) -> impl Iterator<Item = StreamRuleRow<'_>> + '_ {
		self
			.rules
			.for_agent(&self.agent)
			.filter(|rule| !rule.condition.is_empty())
			.map(|rule| {
				let compiled = self.compiled.set.as_ref().map_or(0, |set| {
					set.patterns()
						.filter(|pattern| pattern.name == rule.name)
						.count()
				});
				let interrupt = rule
					.interrupt_mode
					.as_deref()
					.and_then(|mode| mode.parse().ok())
					.unwrap_or(self.policy);
				StreamRuleRow {
					rule,
					compiled,
					disabled: self.disabled.contains(&rule.name),
					interrupt,
				}
			})
	}

	/// Checks that `name` is an active stream rule.
	///
	/// # Errors
	///
	/// [`InspectError::UnknownRule`] when no admitted rule of that name has a
	/// compiled condition, [`InspectError::Disabled`] when the convar turns
	/// it off.
	pub fn require(&self, name: &str) -> Result<(), InspectError> {
		let known = self
			.compiled
			.set
			.as_ref()
			.is_some_and(|set| set.patterns().any(|pattern| pattern.name.as_str() == name));
		if !known {
			return Err(InspectError::UnknownRule {
				name:  Str::new(name),
				agent: self.agent.clone(),
			});
		}
		if self.disabled.iter().any(|entry| entry.as_str() == name) {
			return Err(InspectError::Disabled { name: Str::new(name) });
		}
		Ok(())
	}

	/// Runs `text` through the Director's matcher as one complete block from
	/// `target`, keeping hits for `rule` when given.
	#[must_use]
	pub fn test(&self, target: &ProbeTarget, text: &str, rule: Option<&str>) -> Vec<Finding<'_>> {
		let Some(set) = &self.compiled.set else {
			return Vec::new();
		};
		set.probe(target.source(), target.paths(), text.as_bytes(), self.policy, &self.disabled)
			.into_iter()
			.filter(|hit| rule.is_none_or(|rule| hit.rule.name.as_str() == rule))
			.map(|hit| finding(text, hit))
			.collect()
	}

	/// Probes every UTF-8 file under `root` as the authored text of one
	/// `options.tool` call targeting that file (see the module docs),
	/// keeping hits for `rule` when given.
	///
	/// # Errors
	///
	/// Returns [`InspectError::Walk`] when `root` cannot be traversed and
	/// [`InspectError::Read`] when a listed file cannot be read.
	pub fn scan(
		&self,
		project_root: &Path,
		root: &Path,
		options: &ScanOptions,
		rule: Option<&str>,
	) -> Result<ScanReport<'_>, InspectError> {
		let mut files = Files::default();
		omp_walker::WalkRequest::new(root)
			.hidden(true)
			.gitignore(options.use_gitignore)
			.skip_git(true)
			.skip_node_modules(true)
			.stream(&mut files)
			.map_err(|source| InspectError::Walk { root: root.to_path_buf(), source })?;
		files.0.sort();
		let mut report = ScanReport::default();
		let mut buffer = Vec::new();
		for path in files.0 {
			buffer.clear();
			std::fs::File::open(&path)
				.and_then(|file| file.take(options.max_bytes).read_to_end(&mut buffer))
				.map_err(|source| InspectError::Read { path: path.clone(), source })?;
			let Ok(text) = std::str::from_utf8(&buffer) else {
				report.skipped += 1;
				continue;
			};
			report.scanned += 1;
			let shown = path
				.strip_prefix(project_root)
				.unwrap_or(&path)
				.to_string_lossy();
			let shown = Str::new(shown.replace(std::path::MAIN_SEPARATOR, "/"));
			let target = ProbeTarget::Tool { tool: options.tool.clone(), paths: vec![shown.clone()] };
			let findings = self.test(&target, text, rule);
			if !findings.is_empty() {
				report.files.push(ScannedFile { path: shown, findings });
			}
		}
		Ok(report)
	}
}

/// Regular files a walk yields, absolute.
#[derive(Default)]
struct Files(Vec<PathBuf>);

impl omp_walker::EntryVisitor for Files {
	type Error = Infallible;

	fn visit(
		&mut self,
		entry: omp_walker::Entry<'_>,
	) -> Result<omp_walker::WalkControl, Infallible> {
		if entry.file_type == omp_walker::FileType::File {
			self.0.push(entry.path.to_path_buf());
		}
		Ok(omp_walker::WalkControl::Continue)
	}
}

/// Locates a hit in `text`: its line and a bounded excerpt of that line.
fn finding<'a>(text: &str, hit: ProbeHit<'a>) -> Finding<'a> {
	let mut last = hit.end.min(text.len()).saturating_sub(1);
	while !text.is_char_boundary(last) {
		last -= 1;
	}
	let start = text[..last].rfind('\n').map_or(0, |at| at + 1);
	let stop = text[last..].find('\n').map_or(text.len(), |at| last + at);
	let line = text[..start].bytes().filter(|byte| *byte == b'\n').count() + 1;
	let mut excerpt = text[start..stop].trim();
	if excerpt.len() > EXCERPT_BYTES {
		let mut cut = EXCERPT_BYTES;
		while !excerpt.is_char_boundary(cut) {
			cut -= 1;
		}
		excerpt = &excerpt[..cut];
	}
	Finding {
		rule: hit.rule,
		end: hit.end,
		line,
		excerpt: Str::new(excerpt),
		interrupts: hit.interrupts,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rule_file(root: &Path, name: &str, frontmatter: &str) {
		let dir = root.join(".omp/rules");
		std::fs::create_dir_all(&dir).expect("rules dir");
		std::fs::write(
			dir.join(format!("{name}.md")),
			format!("---\n{frontmatter}---\nDo not do {name}.\n"),
		)
		.expect("rule file");
	}

	fn inspector(project: &Path, home: &Path, con: &omp_con::Ctx) -> StreamRuleInspector {
		let rules =
			ActiveRules::discover(project, home, &home.join(".o2"), &ClaudePlugins::default());
		StreamRuleInspector::new(Arc::new(rules), MAIN_AGENT.to_owned(), con)
	}

	fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
		let temp = tempfile::tempdir().expect("tempdir");
		let root = temp.path().canonicalize().expect("canonical");
		let project = root.join("project");
		let home = root.join("home");
		std::fs::create_dir_all(project.join(".git")).expect("project");
		std::fs::create_dir_all(&home).expect("home");
		rule_file(
			&project,
			"no-unwrap",
			"condition: '\\.unwrap\\(\\)'\nscope: \"tool:write(*.rs)\"\n",
		);
		rule_file(
			&project,
			"no-secret",
			"condition: ['(?i)password', 'a(?=b)']\nscope: text\ninterruptMode: never\n",
		);
		rule_file(&project, "described", "description: plain rulebook rule\n");
		(temp, project, home)
	}

	#[test]
	fn list_rows_carry_compiled_conditions_policy_and_warnings() {
		let (_temp, project, home) = fixture();
		let con = omp_con::Ctx::new();
		let inspector = inspector(&project, &home, &con);
		let rows = inspector
			.rows()
			.map(|row| (row.rule.name.as_str(), row.compiled, row.interrupt, row.disabled))
			.collect::<Vec<_>>();
		assert_eq!(rows, [
			("no-secret", 1, StreamRuleInterrupt::Never, false),
			("no-unwrap", 1, StreamRuleInterrupt::Always, false),
		]);
		let warnings = inspector.compile_warnings();
		assert_eq!(warnings.len(), 1, "{warnings:?}");
		assert!(matches!(
			&warnings[0],
			StreamRuleWarning::Condition { rule, index: 1, .. } if rule.as_str() == "no-secret"
		));
	}

	#[test]
	fn test_runs_the_director_matcher_with_scope_path_and_rule_filters() {
		let (_temp, project, home) = fixture();
		let con = omp_con::Ctx::new();
		let inspector = inspector(&project, &home, &con);
		let text = inspector.test(&ProbeTarget::Text, "line one\nmy PASSWORD here\n", None);
		assert_eq!(text.len(), 1);
		assert_eq!(text[0].rule.name.as_str(), "no-secret");
		assert_eq!(text[0].line, 2);
		assert_eq!(text[0].excerpt.as_str(), "my PASSWORD here");
		assert!(!text[0].interrupts, "the rule's interruptMode never wins");

		let tool = |path: &str| ProbeTarget::Tool {
			tool:  Str::new_static("write"),
			paths: vec![Str::new(path)],
		};
		let hits = inspector.test(&tool("src/main.rs"), "let x = y.unwrap();", None);
		assert_eq!(hits.len(), 1);
		assert!(hits[0].interrupts);
		assert!(
			inspector
				.test(&tool("README.md"), "y.unwrap()", None)
				.is_empty()
		);
		assert!(
			inspector
				.test(&tool("src/main.rs"), "y.unwrap()", Some("no-secret"))
				.is_empty()
		);
		assert!(matches!(inspector.require("described"), Err(InspectError::UnknownRule { .. })));
		inspector.require("no-unwrap").expect("active rule");
	}

	#[test]
	fn disabled_rules_are_listed_but_never_reported() {
		let (_temp, project, home) = fixture();
		let con = omp_con::Ctx::new();
		AI_STREAM_RULES_DISABLED
			.set(&con, vec![Str::new_static("no-secret")])
			.expect("set disabled");
		let inspector = inspector(&project, &home, &con);
		assert!(inspector.rows().any(|row| row.disabled));
		assert!(
			inspector
				.test(&ProbeTarget::Text, "password", None)
				.is_empty()
		);
		assert!(matches!(inspector.require("no-secret"), Err(InspectError::Disabled { .. })));
	}

	#[test]
	fn scan_probes_files_as_authored_tool_text_relative_to_the_project() {
		let (_temp, project, home) = fixture();
		std::fs::create_dir_all(project.join("src")).expect("src");
		std::fs::write(project.join("src/lib.rs"), "fn a() {\n\tb.unwrap();\n}\n").expect("lib");
		std::fs::write(project.join("notes.md"), "b.unwrap()\n").expect("notes");
		std::fs::write(project.join("blob.bin"), [0xff_u8, 0xfe, 0x00]).expect("blob");
		let con = omp_con::Ctx::new();
		let inspector = inspector(&project, &home, &con);
		let report = inspector
			.scan(
				&project,
				&project,
				&ScanOptions {
					tool:          Str::new_static("write"),
					use_gitignore: true,
					max_bytes:     1 << 20,
				},
				None,
			)
			.expect("scan");
		assert_eq!(report.skipped, 1);
		assert_eq!(report.files.len(), 1);
		assert_eq!(report.files[0].path.as_str(), "src/lib.rs");
		assert_eq!(report.files[0].findings[0].line, 2);
	}
}
