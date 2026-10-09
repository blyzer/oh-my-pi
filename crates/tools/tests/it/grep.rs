//! Model-facing behavioral contracts for `grep@1`.

use std::{future, future::Future, sync::Arc};

use bytes::Bytes;
use futures::{StreamExt, executor::block_on};
use omp_core::{Str, sf};
use omp_tool::{
	CallOutcome, CapsBase, Diag, DiagKind, Ev, IncomingParams, ModelClass, Omitted, Part,
	PromptCaps, Severity, Tool, ToolTerminal, Unit, VisibilityReceipt, VisibleSourceLine,
};
use omp_tools::{glob, grep};
use parking_lot::Mutex;
use serde_json::json;

#[derive(Clone)]
struct FakeWorkspace {
	result:   Result<grep::SearchResult, grep::Fault>,
	recorded: Arc<Mutex<Vec<grep::SnapshotRecord>>>,
	requests: Arc<Mutex<Vec<grep::SearchRequest>>>,
}

impl grep::WorkspaceSearch for FakeWorkspace {
	fn search(
		&self,
		request: grep::SearchRequest,
	) -> impl Future<Output = Result<grep::SearchResult, grep::Fault>> + Send + '_ {
		let mut result = self.result.clone();
		if let Ok(result) = &mut result {
			let context_before = usize::try_from(request.context_before).unwrap_or(usize::MAX);
			let context_after = usize::try_from(request.context_after).unwrap_or(usize::MAX);
			for matched in &mut result.matches {
				let first_before = matched.context_before.len().saturating_sub(context_before);
				matched.context_before.drain(..first_before);
				matched.context_after.truncate(context_after);
			}
		}
		self.requests.lock().push(request);
		async move { result }
	}

	fn stage_snapshots(&self, _snapshots: Vec<grep::SearchSnapshot>) -> Result<(), grep::Fault> {
		Ok(())
	}

	fn record_snapshots(&self, records: Vec<grep::SnapshotRecord>) -> Result<(), grep::Fault> {
		self.recorded.lock().extend(records);
		Ok(())
	}

	fn glob(
		&self,
		_request: glob::WalkRequest,
		_cancellation: tokio_util::sync::CancellationToken,
	) -> impl Future<Output = Result<glob::WalkResult, glob::Fault>> + Send + '_ {
		future::ready(Err(glob::Fault::Workspace { message: sf!("unused fake glob boundary") }))
	}

	/// Reads `ssh://`, `issue://`, `pr://` and `mcp://` roots with stored
	/// credentials, as the production resolvers do; every other resolver
	/// reads local or environment-owned state.
	fn internal_root_fetch(&self, root: &grep::SearchRoot) -> Option<omp_tool::FetchEffects> {
		["ssh://", "issue://", "pr://", "mcp://"]
			.iter()
			.any(|scheme| root.path.starts_with(scheme))
			.then_some(omp_tool::FetchEffects { credentials: true })
	}

	fn walk_fetches(&self, _path: &str) -> Vec<(Str, omp_tool::FetchEffects)> {
		Vec::new()
	}
}

struct Invocation {
	outcome: CallOutcome<grep::Payload, grep::Fault>,
	useless: bool,
	diags:   Vec<Diag>,
}

fn fake(result: grep::SearchResult) -> FakeWorkspace {
	FakeWorkspace { result: Ok(result), recorded: Arc::default(), requests: Arc::default() }
}

fn failed(fault: grep::Fault) -> FakeWorkspace {
	FakeWorkspace { result: Err(fault), recorded: Arc::default(), requests: Arc::default() }
}

fn matched(path: &str, line_number: u32, line: &str, tag: Option<&str>) -> grep::SearchMatch {
	grep::SearchMatch {
		source_key: Str::new(path),
		path: Str::new(path),
		root_index: 0,
		line_number,
		line: Str::new(line),
		truncated: false,
		context_before: Vec::new(),
		context_after: Vec::new(),
		snapshot_tag: tag.map(Str::new),
	}
}

fn context(line_number: u32, line: &str) -> grep::ContextLine {
	grep::ContextLine { line_number, line: Str::new(line) }
}

fn invoke_with_context(
	workspace: &FakeWorkspace,
	raw: &str,
	context_before: u32,
	context_after: u32,
) -> Invocation {
	let tool =
		grep::tool(workspace.clone(), context_before, context_after, grep::SearchPolicy::default());
	let (feed, params) = IncomingParams::channel();
	feed
		.args_committed(Str::new(raw))
		.expect("invocation consumer remains live");
	let events = block_on(tool.call(params).collect::<Vec<_>>());
	let mut diags = Vec::new();
	let mut terminal = None;
	for event in events {
		match event {
			Ev::Diag(diag) => diags.push(diag),
			Ev::Done(ToolTerminal::Done { result, useless }) => {
				let outcome = match result {
					Ok(payload) => CallOutcome::Ok(payload),
					Err(fault) => CallOutcome::Faulted(fault),
				};
				terminal = Some((outcome, useless));
			},
			other => panic!("unexpected grep event: {other:?}"),
		}
	}
	let (outcome, useless) = terminal.expect("grep emits one terminal outcome");
	Invocation { outcome, useless, diags }
}

fn invoke(workspace: &FakeWorkspace, raw: &str) -> Invocation {
	invoke_with_context(workspace, raw, 2, 2)
}

fn prompt(workspace: &FakeWorkspace, outcome: &CallOutcome<grep::Payload, grep::Fault>) -> String {
	let tool = grep::tool(workspace.clone(), 2, 2, grep::SearchPolicy::default());
	let caps = PromptCaps::for_tool(
		CapsBase {
			maximum_parts:      1,
			maximum_text_bytes: u32::MAX,
			media:              false,
			model_class:        ModelClass::Standard,
		},
		&tool.spec().rev,
	);
	let parts = match outcome {
		CallOutcome::Ok(payload) => tool.prompt(Ok(payload), &caps),
		CallOutcome::Faulted(fault) => tool.prompt(Err(fault), &caps),
		other => panic!("expected a projectable grep outcome, got {other:?}"),
	};
	match parts.as_slice() {
		[] => String::new(),
		[Part::Text { text }] => text.to_string(),
		_ => panic!("grep must project at most one text part: {parts:?}"),
	}
}

fn invoke_prompt(workspace: &FakeWorkspace, raw: &str) -> (String, bool) {
	let invocation = invoke(workspace, raw);
	(prompt(workspace, &invocation.outcome), invocation.useless)
}

#[test]
fn schema_is_exactly_the_native_grep_schema() {
	let tool = grep::tool(fake(grep::SearchResult::default()), 2, 2, grep::SearchPolicy::default());
	let actual: serde_json::Value =
		serde_json::from_slice(&tool.spec().schema).expect("grep schema is JSON");
	assert_eq!(
		tool.spec().schema.as_ref(),
		omp_tool::schema::<grep::Params>().as_ref(),
		"tool schema must be generated directly from Params",
	);
	assert_eq!(
		actual,
		json!({
			"type": "object",
			"additionalProperties": false,
			"required": ["i", "pattern"],
			"properties": {
				"pattern": {"type": "string", "description": "regex pattern"},
				"path": {
					"type": "string",
					"description": "file, directory, glob, internal URL, or \"<file>:<lines>\" selector to search; pass several as a semicolon-delimited list (\"src; tests\"). Omitted -> searches the workspace root (\".\")"
				},
				"case": {"type": "boolean", "description": "case-sensitive search"},
				"gitignore": {"type": "boolean", "description": "respect gitignore"},
				"skip": {
					"type": ["number", "null"],
					"description": "files to skip before collecting results — use to paginate when the prior call hit the file limit"
				},
				"i": {
					"type": "string",
					"description": "Short present-participle intent for this call."
				},
				"notrunc": {
					"type": "boolean",
					"description": "Prefer complete output inline up to the host security ceiling; overflow or transport backpressure remains available through its artifact."
				}
			}
		})
	);
	for legacy in [
		json!({"pattern": "needle", "patterns": ["needle"]}),
		json!({"pattern": "needle", "include": "*.rs"}),
		json!({"pattern": "needle", "exclude": "target/**"}),
		json!({"pattern": "needle", "mode": "files"}),
		json!({"pattern": "needle", "limit": 20}),
	] {
		assert!(
			serde_json::from_value::<grep::Params>(legacy).is_err(),
			"grep params must reject legacy fields"
		);
	}
}

#[test]
fn grouped_matches_have_folded_headers_tags_and_hashline_match_rows() {
	let workspace = fake(grep::SearchResult {
		matches: vec![
			matched("dir/alpha.rs", 2, "let needle = 1;", Some("A1B2")),
			matched("dir/beta.rs", 7, "// needle", Some("C3D4")),
		],
		multi_scope: true,
		..grep::SearchResult::default()
	});
	let (text, useless) = invoke_prompt(&workspace, r#"{"pattern":"needle","path":"dir"}"#);
	assert_eq!(text, "# dir/\n## alpha.rs#A1B2\n*2:let needle = 1;\n## beta.rs#C3D4\n*7:// needle");
	assert!(!useless);
}

#[test]
fn single_file_match_has_hashline_header_and_no_group_heading() {
	let workspace = fake(grep::SearchResult {
		matches: vec![matched("src/one.rs", 4, "needle();", Some("BEEF"))],
		multi_scope: false,
		..grep::SearchResult::default()
	});
	let (text, useless) = invoke_prompt(&workspace, r#"{"pattern":"needle","path":"src/one.rs"}"#);
	assert_eq!(text, "[src/one.rs#BEEF]\n*4:needle();");
	assert!(!useless);
}

#[test]
fn asymmetric_and_zero_context_are_request_scoped_and_preserve_source_order() {
	let mut found = matched("src/context.rs", 4, "needle", Some("C0DE"));
	found.context_before =
		vec![context(1, "before 1"), context(2, "before 2"), context(3, "before 3")];
	found.context_after = vec![
		context(5, "after 1"),
		context(6, "after 2"),
		context(7, "after 3"),
		context(8, "after 4"),
	];
	let workspace = fake(grep::SearchResult {
		matches: vec![found],
		multi_scope: false,
		..grep::SearchResult::default()
	});

	let asymmetric =
		invoke_with_context(&workspace, r#"{"pattern":"needle","path":"src/context.rs"}"#, 1, 3);
	assert_eq!(
		prompt(&workspace, &asymmetric.outcome),
		"[src/context.rs#C0DE]\n 3:before 3\n*4:needle\n 5:after 1\n 6:after 2\n 7:after 3"
	);

	let zero =
		invoke_with_context(&workspace, r#"{"pattern":"needle","path":"src/context.rs"}"#, 0, 0);
	assert_eq!(prompt(&workspace, &zero.outcome), "[src/context.rs#C0DE]\n*4:needle");

	let requests = workspace.requests.lock();
	assert_eq!(
		requests
			.iter()
			.map(|request| (request.context_before, request.context_after))
			.collect::<Vec<_>>(),
		vec![(1, 3), (0, 0)]
	);
}

#[test]
fn twenty_file_window_emits_pagination_diag_and_skip_twenty_returns_next_page() {
	let matches = (1..=21)
		.map(|index| {
			let path = format!("page/file-{index:02}.rs");
			matched(&path, 1, "needle", Some("CAFE"))
		})
		.collect();
	let workspace =
		fake(grep::SearchResult { matches, multi_scope: true, ..grep::SearchResult::default() });

	let first = invoke(&workspace, r#"{"pattern":"needle","path":"page"}"#);
	let first_text = prompt(&workspace, &first.outcome);
	let expected_files = (1..=20)
		.map(|index| format!("## file-{index:02}.rs#CAFE\n*1:needle"))
		.collect::<Vec<_>>()
		.join("\n");
	assert_eq!(first_text, format!("# page/\n{expected_files}"));
	assert!(!first.useless);
	assert_eq!(first.diags.len(), 1);
	let diag = &first.diags[0];
	assert_eq!(diag.native_kind(), Some(DiagKind::Pagination));
	assert_eq!(diag.severity, Severity::Info);
	assert_eq!(diag.continuation.as_deref(), Some("skip=20"));
	assert_eq!(diag.omitted, Some(Omitted { count: 1, unit: Unit::Files }));

	let (second, second_useless) =
		invoke_prompt(&workspace, r#"{"pattern":"needle","path":"page","skip":20}"#);
	assert_eq!(second, "# page/\n## file-21.rs#CAFE\n*1:needle");
	assert!(!second_useless);
}

#[test]
fn skip_past_the_last_matching_file_has_empty_data_and_range_diag() {
	let workspace = fake(grep::SearchResult {
		matches: vec![
			matched("page/one.rs", 1, "needle", None),
			matched("page/two.rs", 1, "needle", None),
		],
		multi_scope: true,
		..grep::SearchResult::default()
	});
	let invocation = invoke(&workspace, r#"{"pattern":"needle","path":"page","skip":2}"#);
	assert_eq!(prompt(&workspace, &invocation.outcome), "");
	assert_eq!(invocation.diags.len(), 1);
	assert_eq!(invocation.diags[0].native_kind(), Some(DiagKind::RangeOutOfBounds));
	assert_eq!(invocation.diags[0].severity, Severity::Warn);
}

#[test]
fn no_matches_projects_the_pi_message_and_is_useless() {
	let workspace = fake(grep::SearchResult { multi_scope: true, ..grep::SearchResult::default() });
	let (text, useless) = invoke_prompt(&workspace, r#"{"pattern":"absent","path":"src"}"#);
	assert_eq!(text, "No matches found");
	assert!(useless);
}

#[test]
fn invalid_regex_is_mapped_to_the_pi_fault_text() {
	let workspace = failed(grep::Fault::InvalidRegex { message: sf!("unclosed group") });
	let (text, useless) = invoke_prompt(&workspace, r#"{"pattern":"(","path":"src"}"#);
	assert_eq!(text, "Invalid regex: unclosed group");
	assert!(!useless);
}

#[test]
fn line_selector_filters_matches_before_projection() {
	let workspace = fake(grep::SearchResult {
		matches: vec![
			matched("src/range.rs", 2, "needle before", Some("F00D")),
			matched("src/range.rs", 3, "needle in range", Some("F00D")),
			matched("src/range.rs", 5, "needle after", Some("F00D")),
		],
		multi_scope: false,
		..grep::SearchResult::default()
	});
	let (text, useless) =
		invoke_prompt(&workspace, r#"{"pattern":"needle","path":"src/range.rs:3-4"}"#);
	assert_eq!(text, "[src/range.rs#F00D]\n*3:needle in range");
	assert!(!useless);
}

#[test]
fn central_visibility_receipt_authorizes_only_dispatcher_retained_rows() {
	let mut matches = (1..500)
		.map(|line_number| matched("src/range.rs", line_number, "needle before range", Some("F00D")))
		.collect::<Vec<_>>();
	matches.extend((500..=600).map(|line_number| {
		matched("src/range.rs", line_number, &format!("needle {}", "x".repeat(505)), Some("F00D"))
	}));
	let workspace = fake(grep::SearchResult {
		matches,
		snapshots: vec![grep::SearchSnapshot {
			source_key: sf!("src/range.rs"),
			revision:   Bytes::from_static(b"range-revision"),
			bytes:      Bytes::from(vec![b'x'; 80 * 1024]),
		}],
		multi_scope: false,
		..grep::SearchResult::default()
	});
	let invocation = invoke(&workspace, r#"{"pattern":"needle","path":"src/range.rs:500-600"}"#);
	let CallOutcome::Ok(payload) = &invocation.outcome else {
		panic!("grep must succeed: {:?}", invocation.outcome);
	};
	assert!(
		workspace.recorded.lock().is_empty(),
		"the tool must not authorize source lines before central bounding"
	);
	let tool = grep::tool(workspace.clone(), 2, 2, grep::SearchPolicy::default());
	let caps = PromptCaps::for_tool(
		CapsBase {
			maximum_parts:      1,
			maximum_text_bytes: u32::MAX,
			media:              false,
			model_class:        ModelClass::Standard,
		},
		&tool.spec().rev,
	);
	let projection = tool.projection(Ok(payload), &caps);
	let [Part::Text { text }] = projection.parts.as_slice() else {
		panic!("grep must project exactly one text part: {:?}", projection.parts);
	};
	let centrally_retained = 32 * 1024;
	let receipt = VisibilityReceipt {
		lines: projection
			.visibility
			.iter()
			.filter(|span| span.end_byte <= centrally_retained)
			.map(|span| VisibleSourceLine {
				source_key: span.source_key.clone(),
				line:       span.line,
			})
			.collect(),
	};
	tool
		.authorize_visibility(Ok(payload), &receipt)
		.expect("document authority accepts central receipt");
	let recorded = workspace.recorded.lock();
	let [record] = recorded.as_slice() else {
		panic!("one visible file snapshot must be recorded: {recorded:?}");
	};

	assert!(text.starts_with("[src/range.rs#F00D]\n*500:needle "));
	assert!(!text.contains("[truncated:"), "tool-local truncation prose is prohibited");
	assert!(text.contains("*600:needle "), "complete projection reaches the dispatcher");
	assert!(!record.seen_lines.is_empty());
	assert!(
		record
			.seen_lines
			.iter()
			.all(|line| (500..=600).contains(line))
	);
	assert!(!record.seen_lines.contains(&600), "central receipt must omit tail matches");
	assert_eq!(
		record.seen_lines,
		receipt
			.lines
			.iter()
			.map(|line| line.line)
			.collect::<Vec<_>>()
	);
}

#[test]
fn explicit_oversized_file_emits_partial_scan_diag() {
	let workspace = fake(grep::SearchResult {
		matches: vec![matched("large.log", 1, "needle", None)],
		multi_scope: false,
		oversized_files: vec![sf!("large.log")],
		..grep::SearchResult::default()
	});
	let invocation = invoke(&workspace, r#"{"pattern":"needle","path":"large.log"}"#);
	assert_eq!(prompt(&workspace, &invocation.outcome), "*1:needle");
	assert!(!invocation.useless);
	assert_eq!(invocation.diags.len(), 1);
	assert_eq!(invocation.diags[0].native_kind(), Some(DiagKind::PartialScan));
	assert_eq!(invocation.diags[0].severity, Severity::Warn);
}

#[test]
fn skipped_inputs_emit_structured_diags_without_changing_result_data() {
	let workspace = fake(grep::SearchResult {
		matches: vec![matched("src/lib.rs", 1, "needle", None)],
		multi_scope: true,
		missing_paths: vec![sf!("missing")],
		archive_unreadable: vec![sf!("archive.zip:binary.bin")],
		oversized_files: vec![sf!("large.log")],
		..grep::SearchResult::default()
	});
	let invocation = invoke(&workspace, r#"{"pattern":"needle","path":"src; missing"}"#);
	assert_eq!(prompt(&workspace, &invocation.outcome), "# src/\n## lib.rs\n*1:needle");
	assert_eq!(
		invocation
			.diags
			.iter()
			.map(Diag::native_kind)
			.collect::<Vec<_>>(),
		[Some(DiagKind::MissingPaths), Some(DiagKind::Skipped), Some(DiagKind::PartialScan),]
	);
	assert!(
		invocation
			.diags
			.iter()
			.all(|diag| diag.severity == Severity::Warn)
	);
}

#[test]
fn unnamed_unreadable_large_files_emit_skipped_diag() {
	let workspace = fake(grep::SearchResult {
		matches: vec![matched("src/lib.rs", 1, "needle", None)],
		skipped_oversized: 2,
		..grep::SearchResult::default()
	});
	let invocation = invoke(&workspace, r#"{"pattern":"needle","path":"src/lib.rs"}"#);
	assert_eq!(prompt(&workspace, &invocation.outcome), "*1:needle");
	assert_eq!(invocation.diags.len(), 1);
	assert_eq!(invocation.diags[0].native_kind(), Some(DiagKind::Skipped));
	assert_eq!(invocation.diags[0].severity, Severity::Warn);
}

#[test]
fn injected_timeout_projects_the_fixed_thirty_second_mapping() {
	let workspace = failed(grep::Fault::TimedOut);
	let (text, useless) = invoke_prompt(&workspace, r#"{"pattern":"needle","path":"src"}"#);
	assert_eq!(
		text,
		"Grep timed out after 30s; narrow paths or pattern, or scope with `glob` first"
	);
	assert!(!useless);
}

#[test]
fn oversized_projection_remains_complete_for_central_dispatch() {
	let matches = (1..=200)
		.map(|line_number| {
			matched("large.rs", line_number, &format!("needle {}", "x".repeat(400)), Some("B10B"))
		})
		.collect();
	let workspace =
		fake(grep::SearchResult { matches, multi_scope: false, ..grep::SearchResult::default() });
	let invocation = invoke(&workspace, r#"{"pattern":"needle","path":"large.rs"}"#);
	let text = prompt(&workspace, &invocation.outcome);
	let CallOutcome::Ok(payload) = &invocation.outcome else {
		panic!("large grep output must succeed");
	};
	assert!(text.starts_with("[large.rs#B10B]\n*1:needle "));
	let expected_tail = format!("*200:needle {}", "x".repeat(400));
	assert!(text.ends_with(expected_tail.as_str()));
	assert!(!text.contains("[truncated"));
	assert_eq!(payload.files[0].matches.len(), 200);

	let zero_tool = grep::tool(workspace, 2, 2, grep::SearchPolicy::default());
	let zero = zero_tool.prompt(
		Ok(payload),
		&PromptCaps::for_tool(
			CapsBase {
				maximum_parts:      0,
				maximum_text_bytes: 0,
				media:              false,
				model_class:        ModelClass::Standard,
			},
			&zero_tool.spec().rev,
		),
	);
	assert!(zero.is_empty());
}

/// Document reads plus `fetch`, the envelope of one search call.
fn search_effects(fetch: Option<omp_tool::FetchEffects>) -> omp_tool::Effects {
	omp_tool::Effects {
		documents: Some(omp_tool::DocEffects { read: true, write_globs: Arc::default() }),
		fetch,
		..omp_tool::Effects::empty()
	}
}

/// A registry holding `grep@1` over the fake workspace under `policy`, which
/// judges each call as the environment does.
fn judging_registry(policy: grep::SearchPolicy) -> omp_tool::Registry {
	let mut registry = omp_tool::Registry::new();
	registry
		.register(
			grep::tool(fake(grep::SearchResult::default()), 2, 2, policy),
			omp_tool::Presentation::Slot,
			omp_tool::Claims {
				precedence: omp_tool::Precedence::CORE,
				claimant:   sf!("omp/core"),
				replaces:   None,
			},
		)
		.expect("grep registers");
	registry
}

/// The production policy shape: URL roots enabled and credentialed resolvers
/// registered.
const PRODUCTION_POLICY: grep::SearchPolicy =
	grep::SearchPolicy { fetch_enabled: true, credentialed_fetch: true };

/// Each call is judged by what its roots fetch, with the root kinds the
/// workspace routes them by: a local path, a line selector, an archive
/// member, a `file://` URL and every resolver-backed root whose resolver
/// reads local state only read documents; an http(s) root is an anonymous
/// fetch; an `ssh://`, `issue://`, `pr://` or `mcp://` root fetches with
/// credentials. Every `;` root counts, each fetching root is named once in
/// its selector-peeled spelling, and a path the executor refuses before it
/// searches anything fetches nothing.
#[test]
fn grep_judges_each_call_by_what_its_roots_fetch() {
	let registry = judging_registry(PRODUCTION_POLICY);
	let anonymous = Some(omp_tool::FetchEffects { credentials: false });
	let credentialed = Some(omp_tool::FetchEffects { credentials: true });
	for (path, fetch, locators) in [
		(None, None, &[][..]),
		(Some("src"), None, &[]),
		(Some("src/**/*.rs"), None, &[]),
		(Some("src/lib.rs:10-20"), None, &[]),
		(Some("fixture.zip:docs"), None, &[]),
		(Some("file:///tmp/notes.txt"), None, &[]),
		(Some("local://scratch.md"), None, &[]),
		(Some("omp://"), None, &[]),
		(Some("vault://notes?op=search&q=todo"), None, &[]),
		(Some("custom://thing"), None, &[]),
		(Some("www.example.com/page"), None, &[]),
		(Some("https://docs.rs/serde"), anonymous, &["https://docs.rs/serde"]),
		(Some("https://docs.rs/tokio:10-20"), anonymous, &["https://docs.rs/tokio"]),
		(Some("ssh://prod/etc/hosts"), credentialed, &["ssh://prod/etc/hosts"]),
		(Some("issue://5"), credentialed, &["issue://5"]),
		(Some("pr://owner/repo/7"), credentialed, &["pr://owner/repo/7"]),
		(Some("mcp://linear/issue/1"), credentialed, &["mcp://linear/issue/1"]),
		(Some("src; https://docs.rs/x"), anonymous, &["https://docs.rs/x"]),
		(Some("https://b.example/x; issue://9; src"), credentialed, &[
			"https://b.example/x",
			"issue://9",
		]),
		(Some("https://a.example/x;https://a.example/x"), anonymous, &["https://a.example/x"]),
		(Some("https://docs.rs/x; src:@Widget"), None, &[]),
	] {
		let arguments = match path {
			Some(path) => json!({ "pattern": "needle", "path": path }),
			None => json!({ "pattern": "needle" }),
		}
		.to_string();
		assert_eq!(
			registry
				.invocation_effects("grep", &arguments)
				.expect("judged"),
			search_effects(fetch),
			"{path:?}"
		);
		assert_eq!(
			registry.fetch_locators("grep", &arguments),
			locators.iter().copied().map(Str::new).collect::<Vec<_>>(),
			"{path:?}"
		);
	}
}

/// The declared maximum is what the policy lets a call fetch: the anonymous
/// URL fetch while URL roots are enabled, a credentialed one while the
/// workspace's resolvers fetch with stored credentials, whatever URL roots
/// say. A local search is a document read under every policy; a URL root
/// fetches only while URL roots are enabled; a credentialed root beyond the
/// maximum is refused, never judged by it.
#[test]
fn grep_maximum_holds_the_fetches_its_policy_permits() {
	let anonymous = Some(omp_tool::FetchEffects { credentials: false });
	let credentialed = Some(omp_tool::FetchEffects { credentials: true });
	assert_eq!(grep::spec(grep::SearchPolicy::default()).effects, search_effects(anonymous));
	for (fetch_enabled, credentialed_fetch, ceiling) in [
		(true, false, anonymous),
		(true, true, credentialed),
		(false, true, credentialed),
		(false, false, None),
	] {
		let policy = grep::SearchPolicy { fetch_enabled, credentialed_fetch };
		assert_eq!(grep::spec(policy).effects, search_effects(ceiling), "{policy:?}");
		let registry = judging_registry(policy);
		let judge = |path: &str| {
			registry
				.invocation_effects("grep", &json!({ "pattern": "needle", "path": path }).to_string())
		};
		assert_eq!(judge("src").expect("a local search"), search_effects(None), "{policy:?}");
		assert_eq!(
			judge("https://docs.rs/serde").expect("a URL search"),
			search_effects(anonymous.filter(|_| fetch_enabled)),
			"{policy:?}"
		);
		let ssh = judge("ssh://prod/etc/hosts");
		if credentialed_fetch {
			assert_eq!(
				ssh.expect("a credentialed search"),
				search_effects(credentialed),
				"{policy:?}"
			);
		} else {
			assert!(
				matches!(ssh, Err(omp_tool::RegistryError::InvocationEffectsExceedMaximum { .. })),
				"{policy:?}: {ssh:?}"
			);
		}
	}
}

/// With URL fetches disabled a URL root is refused before the workspace
/// searches anything, and the search of local roots alone still runs; with
/// them enabled the URL root reaches the workspace.
#[test]
fn url_roots_are_refused_before_the_search_while_fetches_are_disabled() {
	let run = |policy: grep::SearchPolicy, raw: &str| {
		let workspace = fake(grep::SearchResult::default());
		let tool = grep::tool(workspace.clone(), 2, 2, policy);
		let (feed, params) = IncomingParams::channel();
		feed
			.args_committed(Str::new(raw))
			.expect("invocation consumer remains live");
		let events = block_on(tool.call(params).collect::<Vec<_>>());
		let terminal = events
			.into_iter()
			.find_map(|event| match event {
				Ev::Done(ToolTerminal::Done { result, .. }) => Some(result),
				_ => None,
			})
			.expect("grep emits one terminal outcome");
		let searched = workspace
			.requests
			.lock()
			.iter()
			.map(|request| {
				request
					.roots
					.iter()
					.map(|root| root.kind)
					.collect::<Vec<_>>()
			})
			.collect::<Vec<_>>();
		(terminal, searched)
	};
	let disabled = grep::SearchPolicy { fetch_enabled: false, credentialed_fetch: true };
	let mixed = r#"{"pattern":"needle","path":"src; https://docs.rs/x:1-5"}"#;
	let (terminal, searched) = run(disabled, mixed);
	assert_eq!(terminal, Err(grep::Fault::UrlRootDisabled { root: sf!("https://docs.rs/x:1-5") }));
	assert!(searched.is_empty(), "nothing reached the workspace: {searched:?}");
	assert!(
		grep::Fault::UrlRootDisabled { root: sf!("https://docs.rs/x") }
			.to_string()
			.contains("tools.fetch.enabled")
	);

	let (terminal, searched) = run(disabled, r#"{"pattern":"needle","path":"src; issue://5"}"#);
	assert!(terminal.is_ok(), "{terminal:?}");
	assert_eq!(searched, [vec![grep::SearchRootKind::Filesystem, grep::SearchRootKind::Internal]]);

	let (terminal, searched) = run(PRODUCTION_POLICY, mixed);
	assert!(terminal.is_ok(), "{terminal:?}");
	assert_eq!(searched, [vec![grep::SearchRootKind::Filesystem, grep::SearchRootKind::Url]]);
}
