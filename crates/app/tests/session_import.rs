//! Foreign transcript import contracts for native journals.

use std::fs;

use omp_app::session_import::{ForeignFormat, import_file};
use omp_journal::{Journal, abandoned};
use omp_session::{ComponentRegistry, Session};

#[test]
fn claude_fixture_imports_to_native_journal() {
	let directory = tempfile::tempdir().expect("tempdir");
	let source = directory.path().join("claude.jsonl");
	let destination = directory.path().join("claude.oms");
	fs::write(
		&source,
		r#"{"type":"user","message":{"role":"user","content":"hello"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"world"}]}}
"#,
	)
	.expect("fixture");
	assert_eq!(import_file(ForeignFormat::Claude, &source, &destination).expect("import"), 2);
	let session = Session::open(&destination, ComponentRegistry::standard()).expect("open");
	assert_eq!(omp_app::print_mode::transcript_text(session.dom()), "world\n");
}

#[test]
fn codex_fixture_imports_to_native_journal() {
	let directory = tempfile::tempdir().expect("tempdir");
	let source = directory.path().join("codex.jsonl");
	let destination = directory.path().join("codex.oms");
	fs::write(
		&source,
		r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ping"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"pong"}]}}
"#,
	)
	.expect("fixture");
	assert_eq!(import_file(ForeignFormat::Codex, &source, &destination).expect("import"), 2);
	let session = Session::open(&destination, ComponentRegistry::standard()).expect("open");
	assert_eq!(omp_app::print_mode::transcript_text(session.dom()), "pong\n");
}

#[test]
fn claude_import_preserves_blocks_tools_media_usage_errors_and_source_bytes() {
	let directory = tempfile::tempdir().expect("tempdir");
	let source = directory.path().join("claude-rich.jsonl");
	let destination = directory.path().join("claude-rich.oms");
	let fixture = concat!(
		"{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"timestamp\":\"2026-01-01T00:00:\
		 00Z\",\"cwd\":\"/project\",\"sessionId\":\"session-1\",\"message\":{\"content\":[{\"type\":\
		 \"text\",\"text\":\"look\"},{\"type\":\"image\",\"source\":{\"media_type\":\"image/png\",\"\
		 data\":\"aW1hZ2U=\"}}]}}\n",
		"{not-json\n",
		"{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"u1\",\"timestamp\":\"\
		 2026-01-01T00:00:01Z\",\"error\":\"provider \
		 warning\",\"apiErrorStatus\":529,\"message\":{\"id\":\"msg-1\",\"model\":\"\
		 claude-sonnet-4-5\",\"stop_reason\":\"tool_use\",\"usage\":{\"input_tokens\":10,\"\
		 output_tokens\":5,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":2},\"\
		 content\":[{\"type\":\"thinking\",\"thinking\":\"inspect\",\"signature\":\"signed\"},{\"\
		 type\":\"text\",\"text\":\"working\"},{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"\
		 read\",\"input\":{\"path\":\"a.rs\"}}]}}\n",
		"{\"type\":\"user\",\"uuid\":\"r1\",\"parentUuid\":\"a1\",\"timestamp\":\"2026-01-01T00:00:\
		 02Z\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"call-1\",\"\
		 content\":[{\"type\":\"text\",\"text\":\"contents\"},{\"type\":\"image\",\"source\":{\"\
		 media_type\":\"image/png\",\"data\":\"cmVzdWx0\"}}],\"is_error\":false}]}}\n",
		"{\"type\":\"custom-title\",\"customTitle\":\"Imported title\"}\n",
	);
	fs::write(&source, fixture).expect("fixture");

	assert_eq!(import_file(ForeignFormat::Claude, &source, &destination).expect("import"), 3);
	let journal_text = fs::read_to_string(&destination).expect("journal");
	assert!(journal_text.contains("tool.call@1"));
	assert!(journal_text.contains("tool.result@1"));
	assert!(journal_text.contains("thinking-signature"));
	assert!(journal_text.contains("signed"));
	assert!(journal_text.contains("source-timestamp-ms"));
	assert!(journal_text.contains("cache_read"));
	assert!(journal_text.contains("image/png"));
	assert!(journal_text.contains("malformed_rows"));
	assert!(journal_text.contains("provider warning"));
	assert!(journal_text.contains("Imported title"));

	let session = Session::open(&destination, ComponentRegistry::standard()).expect("open");
	let rendered = omp_app::print_mode::transcript_text(session.dom());
	assert!(rendered.contains("working"));
}

#[test]
fn claude_parent_links_become_native_abandoned_branches() {
	let directory = tempfile::tempdir().expect("tempdir");
	let source = directory.path().join("claude-branch.jsonl");
	let destination = directory.path().join("claude-branch.oms");
	fs::write(
		&source,
		r#"{"type":"user","uuid":"root","parentUuid":null,"message":{"content":"root"}}
{"type":"assistant","uuid":"old","parentUuid":"root","message":{"content":[{"type":"text","text":"abandoned"}]}}
{"type":"assistant","uuid":"live","parentUuid":"root","message":{"content":[{"type":"text","text":"selected"}]}}
"#,
	)
	.expect("fixture");

	import_file(ForeignFormat::Claude, &source, &destination).expect("import");
	let session = Session::open(&destination, ComponentRegistry::standard()).expect("open");
	assert_eq!(omp_app::print_mode::transcript_text(session.dom()), "selected\n");
	drop(session);
	let (_journal, entries) = Journal::open(&destination).expect("journal");
	assert!(abandoned(&entries).any(|entry| entry.data.contains("abandoned")));
	assert!(
		fs::read_to_string(&destination)
			.expect("journal")
			.contains("prior:")
	);
}

#[test]
fn codex_import_preserves_reasoning_tools_failures_roles_usage_and_rollback() {
	let directory = tempfile::tempdir().expect("tempdir");
	let source = directory.path().join("codex-rich.jsonl");
	let destination = directory.path().join("codex-rich.oms");
	fs::write(
		&source,
		r#"{"type":"session_meta","timestamp":"2026-02-01T00:00:00Z","payload":{"id":"codex-1","cwd":"/project","title":"Codex title"}}
truncated {
{"type":"turn_context","timestamp":"2026-02-01T00:00:01Z","payload":{"model":"gpt-5.3-codex"}}
{"type":"response_item","timestamp":"2026-02-01T00:00:02Z","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"policy"}]}}
{"type":"response_item","timestamp":"2026-02-01T00:00:03Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"inspect"},{"type":"input_image","image_url":"data:image/png;base64,aW1hZ2U="}]}}
{"type":"response_item","timestamp":"2026-02-01T00:00:04Z","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]}}
{"type":"response_item","timestamp":"2026-02-01T00:00:05Z","payload":{"type":"function_call","call_id":"call-1","name":"read","arguments":"{\"path\":\"a.rs\"}"}}
{"type":"response_item","timestamp":"2026-02-01T00:00:06Z","payload":{"type":"function_call_output","call_id":"call-1","output":"failed","status":"failed"}}
{"type":"event_msg","timestamp":"2026-02-01T00:00:07Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":12,"cached_input_tokens":4,"output_tokens":7}}}}
{"type":"response_item","timestamp":"2026-02-01T00:00:08Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old answer"}]}}
{"type":"event_msg","timestamp":"2026-02-01T00:00:09Z","payload":{"type":"thread_rolled_back","num_turns":1}}
{"type":"response_item","timestamp":"2026-02-01T00:00:10Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"new branch"}]}}
{"type":"response_item","timestamp":"2026-02-01T00:00:11Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"new answer"}]}}
"#,
	)
	.expect("fixture");

	import_file(ForeignFormat::Codex, &source, &destination).expect("import");
	let journal_text = fs::read_to_string(&destination).expect("journal");
	assert!(journal_text.contains("developer"));
	assert!(journal_text.contains("tool.call@1"));
	assert!(journal_text.contains("tool.result@1"));
	assert!(journal_text.contains("\"fault\""));
	assert!(journal_text.contains("tokens_in"));
	assert!(journal_text.contains("source-timestamp"));
	assert!(journal_text.contains("malformed_rows"));
	assert!(journal_text.contains("foreign-rollback"));
	assert!(journal_text.contains("prior:"));

	let session = Session::open(&destination, ComponentRegistry::standard()).expect("open");
	assert_eq!(omp_app::print_mode::transcript_text(session.dom()), "new answer\n");
	drop(session);
	let (_journal, entries) = Journal::open(&destination).expect("journal");
	assert!(abandoned(&entries).any(|entry| entry.data.contains("old answer")));
}

// omp v1 transcripts (`ForeignFormat::Omp1`), converted through the driver's
// sessions import exactly as the picker and `omp config import-v1 --sessions`
// do.

mod omp1 {
	use std::path::{Path, PathBuf};

	use omp_app::session_import::V1Converter;
	use omp_core::{Hash32, Str};
	use omp_dom::{Dom, Handle, KnownTag, PropId, PropKey, Tag, Value as DomValue};
	use omp_driver::{
		session_imports::PriorImport,
		v1_import::{
			Attention, CredentialAccess, ImportMode, ImportOutcome, ImportPair, ImportReport,
			ImportStep, ProfileSelection, SessionImport, SkipReason, V1Inputs, V1Source, V2Roots,
			plan, run_with,
			sessions::{import_session, list},
		},
	};
	use omp_proto::thread::v1::{item, part};
	use serde_json::{Value, json};

	use super::*;

	const PNG: &[u8] = b"\x89PNG v1 image bytes";

	struct Tree {
		root:    tempfile::TempDir,
		home:    PathBuf,
		project: PathBuf,
	}

	impl Tree {
		fn new() -> Self {
			let root = tempfile::tempdir().expect("scratch");
			let home = root.path().join("home");
			let project = root.path().join("project");
			fs::create_dir_all(&project).expect("project");
			Self { root, home, project }
		}

		fn agent(&self) -> PathBuf {
			self.home.join(".omp/agent")
		}

		/// Writes `<agent>/sessions/-project/<ts>_<id>.jsonl`.
		fn transcript(&self, id: &str, lines: &[Value]) -> PathBuf {
			let path = self
				.agent()
				.join("sessions/-project")
				.join(format!("2026-01-02T03-04-05-000Z_{id}.jsonl"));
			write_lines(&path, lines);
			path
		}

		fn header(&self, id: &str) -> Value {
			json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-01-02T03:04:05.000Z", "cwd": self.project, "providerPromptCacheKey": "cache-key-v1"})
		}

		fn pair(&self) -> ImportPair {
			let v2 = V2Roots {
				config_dir:     self.root.path().join("o2"),
				data_dir:       self.root.path().join("share/omp"),
				state_dir:      self.root.path().join("state/omp"),
				cache_dir:      self.root.path().join("cache/omp"),
				active_profile: None,
			};
			let inputs = V1Inputs { home: self.home.clone(), ..V1Inputs::default() };
			plan(&V1Source::new(inputs), &v2, &ProfileSelection::Named(None))
				.expect("plan")
				.swap_remove(0)
		}

		/// Converts `source` as the picker would, and reopens the journal.
		fn import(&self, source: &Path) -> (PathBuf, Session) {
			let pair = self.pair();
			let imported =
				import_session(&pair.target, &pair.source, source, &V1Converter).expect("import");
			assert!(imported.converted);
			let session =
				Session::open(&imported.journal, ComponentRegistry::standard()).expect("resume");
			(imported.journal, session)
		}
	}

	fn write_lines(path: &Path, lines: &[Value]) {
		fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
		let mut text = String::new();
		for line in lines {
			text.push_str(&line.to_string());
			text.push('\n');
		}
		fs::write(path, text).expect("transcript");
	}

	fn message(id: &str, parent: Option<&str>, message: Value) -> Value {
		json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2026-01-02T03:04:06.000Z", "message": message})
	}

	fn user(id: &str, parent: Option<&str>, text: &str) -> Value {
		message(id, parent, json!({"role": "user", "content": text, "timestamp": 1}))
	}

	fn assistant(id: &str, parent: Option<&str>, text: &str) -> Value {
		message(
			id,
			parent,
			json!({"role": "assistant", "content": [{"type": "text", "text": text}], "api": "anthropic-messages", "provider": "anthropic", "model": "claude-opus-4-5", "stopReason": "stop", "timestamp": 2}),
		)
	}

	/// Every text part the provider projection would send, in order.
	fn thread_texts(dom: &Dom) -> Vec<String> {
		omp_session::project_thread(dom)
			.iter()
			.filter_map(|item| match item.kind.as_ref()? {
				item::Kind::Message(message) => Some(message),
				_ => None,
			})
			.flat_map(|message| &message.parts)
			.filter_map(|part| match part.kind.as_ref()? {
				part::Kind::Text(text) => Some(text.to_string()),
				_ => None,
			})
			.collect()
	}

	fn custom<'d>(dom: &'d Dom, handle: Handle, key: &'static str) -> Option<&'d DomValue> {
		dom.get(handle)?
			.prop(&PropKey::Custom(Str::new_static(key)))
	}

	fn tagged(dom: &Dom, tag: &Tag) -> Vec<Handle> {
		dom.handles()
			.filter(|handle| dom.get(*handle).is_some_and(|node| &node.tag == tag))
			.collect()
	}

	#[test]
	fn v1_messages_tools_thinking_images_and_usage_round_trip_and_resume() {
		let tree = Tree::new();
		let hash = Hash32::sum(PNG).to_hex();
		let hash = hash.as_str();
		let blobs = tree.agent().join("blobs");
		fs::create_dir_all(&blobs).expect("blobs");
		fs::write(blobs.join(hash), PNG).expect("blob");
		let source = tree.transcript("rich", &[
			json!({"type": "title", "v": 1, "title": "Rich v1 session", "updatedAt": "2026-01-02T03:05:00.000Z", "pad": "    "}),
			tree.header("rich"),
			json!({"type": "model_change", "id": "e0", "parentId": null, "timestamp": "2026-01-02T03:04:05.500Z", "model": "anthropic/claude-opus-4-5"}),
			message("e1", Some("e0"), json!({"role": "user", "content": [{"type": "text", "text": "look at this"}, {"type": "image", "data": format!("blob:sha256:{hash}"), "mimeType": "image/png"}], "timestamp": 1})),
			message("e2", Some("e1"), json!({"role": "assistant", "content": [{"type": "thinking", "thinking": "inspect first", "thinkingSignature": "sig-from-v1"}, {"type": "text", "text": "reading"}, {"type": "toolCall", "id": "call-1", "name": "read", "arguments": {"path": "a.rs"}}], "api": "anthropic-messages", "provider": "anthropic", "model": "claude-opus-4-5", "responseId": "msg-1", "usage": {"input": 10, "output": 5, "cacheRead": 3, "cacheWrite": 2, "totalTokens": 20, "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}, "stopReason": "toolUse", "timestamp": 2})),
			message("e3", Some("e2"), json!({"role": "toolResult", "toolCallId": "call-1", "toolName": "read", "content": [{"type": "text", "text": "fn main() {}"}], "isError": false, "timestamp": 3})),
			assistant("e4", Some("e3"), "done reading"),
			json!({"type": "credential_pin", "id": "e5", "parentId": "e4", "timestamp": "2026-01-02T03:04:09.000Z", "provider": "anthropic", "hash": "pin-hash"}),
			message("e6", Some("e5"), json!({"role": "bashExecution", "command": "ls", "output": "a.rs", "exitCode": 0, "cancelled": false, "truncated": false, "timestamp": 4})),
		]);

		let (journal, session) = tree.import(&source);

		let text = fs::read_to_string(&journal).expect("journal");
		assert!(text.contains("tool.call@1") && text.contains("tool.result@1"));
		assert!(text.contains("import-format") && text.contains("omp1"));
		assert!(text.contains("credential_pin") && text.contains("pin-hash"));
		assert!(text.contains("cacheRead") && text.contains("tokens_in"));
		let dom = session.dom();
		let meta = dom.get(dom.meta()).expect("meta");
		assert_eq!(
			meta.prop(&PropId::Name.into()).and_then(DomValue::as_str),
			Some("Rich v1 session")
		);
		assert_eq!(
			meta
				.prop(&PropKey::Custom(Str::new_static("import-source-id")))
				.and_then(DomValue::as_str),
			Some("rich")
		);
		assert_eq!(
			omp_app::print_mode::transcript_text(dom),
			"reading\n[tool: read]\ndone reading\n",
			"thinking is not transcript text"
		);
		// Thinking is kept, its signature only as foreign metadata.
		let thinking = tagged(dom, &Tag::Custom(Str::new_static(omp_session::ASSISTANT_CONTENT_TAG)))
			.into_iter()
			.find(|handle| {
				dom.get(*handle)
					.and_then(|node| node.prop(&PropId::Kind.into()))
					.and_then(DomValue::as_str)
					== Some("thinking")
			})
			.expect("thinking block");
		assert!(custom(dom, thinking, "thinking-signature").is_none());
		let Some(DomValue::Json(block)) = custom(dom, thinking, "foreign-block") else {
			panic!("the raw thinking block is foreign metadata");
		};
		assert!(block.get().contains("sig-from-v1"));
		// The tool call settled, so the resumed session has nothing pending.
		assert!(session.unsettled_calls().is_empty());
		let texts = thread_texts(dom);
		assert!(texts.iter().any(|text| text.contains("look at this")), "{texts:?}");
		assert!(texts.iter().any(|text| text.contains("done reading")), "{texts:?}");
		// The `!ls` execution is a developer note in its turn.
		let notes = tagged(dom, &Tag::Known(KnownTag::Developer));
		assert!(notes.iter().any(|handle| {
			custom(dom, *handle, "role").and_then(DomValue::as_str) == Some("bashExecution")
		}));
	}

	#[test]
	fn v1_blob_images_resolve_from_the_blob_store() {
		let tree = Tree::new();
		let hash = Hash32::sum(PNG).to_hex();
		let hash = hash.as_str();
		let blobs = tree.agent().join("blobs");
		fs::create_dir_all(&blobs).expect("blobs");
		fs::write(blobs.join(hash), PNG).expect("blob");
		let missing = "0".repeat(64);
		let source = tree.transcript("images", &[
			tree.header("images"),
			message("u1", None, json!({"role": "user", "content": [{"type": "text", "text": "see"}, {"type": "image", "data": format!("blob:sha256:{hash}"), "mimeType": "image/png"}, {"type": "image", "data": format!("blob:sha256:{missing}"), "mimeType": "image/png"}], "timestamp": 1})),
		]);

		let (journal, session) = tree.import(&source);

		// The stored blob is the same bytes, so the same SHA-256 address, and
		// the user message names it as its first attachment.
		let blob = session
			.blobs()
			.get(&omp_journal::blob::BlobRef { hash: Hash32::sum(PNG), size: PNG.len() as u64 })
			.expect("the image is in the journal store");
		assert_eq!(blob.as_ref(), PNG);
		let text = fs::read_to_string(&journal).expect("journal");
		assert!(text.contains("[Image #1]"), "{text}");
		assert!(text.contains(&format!("[v1 image blob:sha256:{missing} is missing]")), "{text}");
	}

	#[test]
	fn v1_compactions_branches_and_clears_become_native() {
		let tree = Tree::new();
		let source = tree.transcript("tree", &[
			tree.header("tree"),
			user("u1", None, "first question"),
			assistant("a1", Some("u1"), "first answer"),
			user("u2", Some("a1"), "abandoned question"),
			assistant("a2", Some("u2"), "abandoned answer"),
			json!({"type": "branch_summary", "id": "b1", "parentId": "a1", "timestamp": "2026-01-02T03:04:10.000Z", "fromId": "a2", "summary": "tried the abandoned path"}),
			user("u3", Some("b1"), "alternate question"),
			assistant("a3", Some("u3"), "alternate answer"),
			json!({"type": "compaction", "id": "c1", "parentId": "a3", "timestamp": "2026-01-02T03:04:11.000Z", "summary": "summary of the first exchange", "firstKeptEntryId": "b1", "tokensBefore": 1000, "method": "auto"}),
			user("u4", Some("c1"), "after compaction"),
			assistant("a4", Some("u4"), "final answer"),
		]);

		let (journal, session) = tree.import(&source);

		let texts = thread_texts(session.dom());
		assert_eq!(texts.first().map(String::as_str), Some("summary of the first exchange"));
		for kept in ["alternate question", "alternate answer", "after compaction", "final answer"] {
			assert!(texts.iter().any(|text| text == kept), "{kept} is live: {texts:?}");
		}
		for hidden in ["first question", "first answer", "abandoned question", "abandoned answer"] {
			assert!(!texts.iter().any(|text| text == hidden), "{hidden} is hidden: {texts:?}");
		}
		let notes = tagged(session.dom(), &Tag::Known(KnownTag::Developer));
		assert!(notes.iter().any(|handle| {
			custom(session.dom(), *handle, "role").and_then(DomValue::as_str) == Some("branchSummary")
		}));
		drop(session);
		let (_journal, entries) = Journal::open(&journal).expect("journal");
		assert!(abandoned(&entries).any(|entry| entry.data.contains("abandoned answer")));

		// A `/clear` boundary hides everything before it.
		let cleared = tree.transcript("cleared", &[
			tree.header("cleared"),
			user("u1", None, "before clear"),
			assistant("a1", Some("u1"), "old reply"),
			json!({"type": "reset_boundary", "id": "r1", "parentId": "a1", "timestamp": "2026-01-02T03:04:12.000Z"}),
			user("u2", Some("r1"), "after clear"),
		]);
		let (_, session) = tree.import(&cleared);
		let texts = thread_texts(session.dom());
		assert!(texts.iter().any(|text| text == "after clear"), "{texts:?}");
		assert!(!texts.iter().any(|text| text == "before clear"), "{texts:?}");
	}

	#[test]
	fn v1_and_v2_headers_migrate_before_import() {
		let tree = Tree::new();
		// Version 1: no ids, and a compaction indexing its first kept entry.
		let legacy = tree.transcript("legacy", &[
			json!({"type": "session", "id": "legacy", "timestamp": "2025-12-09T00:53:29.825Z", "cwd": tree.project}),
			json!({"type": "message", "timestamp": "2025-12-09T00:53:30.000Z", "message": {"role": "user", "content": "oldest question", "timestamp": 1}}),
			json!({"type": "message", "timestamp": "2025-12-09T00:53:31.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "oldest answer"}], "timestamp": 2}}),
			json!({"type": "message", "timestamp": "2025-12-09T00:53:32.000Z", "message": {"role": "user", "content": "kept question", "timestamp": 3}}),
			json!({"type": "compaction", "timestamp": "2025-12-09T00:53:33.000Z", "summary": "legacy summary", "firstKeptEntryIndex": 3, "tokensBefore": 10}),
			json!({"type": "message", "timestamp": "2025-12-09T00:53:34.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "latest answer"}], "timestamp": 4}}),
		]);
		let (_, session) = tree.import(&legacy);
		let texts = thread_texts(session.dom());
		assert_eq!(texts.first().map(String::as_str), Some("legacy summary"), "{texts:?}");
		assert!(texts.iter().any(|text| text == "kept question"), "{texts:?}");
		assert!(texts.iter().any(|text| text == "latest answer"), "{texts:?}");
		assert!(!texts.iter().any(|text| text == "oldest question"), "{texts:?}");

		// Version 2: `hookMessage` became `custom` in version 3.
		let hooked = tree.transcript("hooked", &[
			json!({"type": "session", "version": 2, "id": "hooked", "timestamp": "2026-01-01T00:00:00.000Z", "cwd": tree.project}),
			user("u1", None, "question"),
			message("h1", Some("u1"), json!({"role": "hookMessage", "customType": "note", "content": "hook text", "display": true, "timestamp": 2})),
		]);
		let (_, session) = tree.import(&hooked);
		let notes = tagged(session.dom(), &Tag::Known(KnownTag::Developer));
		assert!(notes.iter().any(|handle| {
			custom(session.dom(), *handle, "role").and_then(DomValue::as_str) == Some("custom")
		}));
	}

	#[test]
	fn v1_subagents_become_linked_child_journals_with_artifacts() {
		let tree = Tree::new();
		let parent = tree.transcript("parent", &[
			tree.header("parent"),
			user("u1", None, "delegate"),
			assistant("a1", Some("u1"), "delegated"),
		]);
		let artifacts = parent.with_extension("");
		write_lines(&artifacts.join("0-Scout.jsonl"), &[
			json!({"type": "session", "version": 3, "id": "child", "timestamp": "2026-01-02T03:05:00.000Z", "cwd": tree.project}),
			json!({"type": "session_init", "id": "i1", "parentId": null, "timestamp": "2026-01-02T03:05:00.000Z", "systemPrompt": "scout", "task": "look", "tools": ["read"], "agent": "scout"}),
			user("c1", Some("i1"), "child task"),
			assistant("c2", Some("c1"), "child result"),
		]);
		fs::write(artifacts.join("0.bash.log"), "spilled tool output").expect("artifact");

		let (journal, session) = tree.import(&parent);

		let dom = session.dom();
		let jobs = tagged(dom, &Tag::Known(KnownTag::Subagent));
		assert_eq!(jobs.len(), 1);
		let job = dom.get(jobs[0]).expect("job");
		assert_eq!(job.prop(&PropId::Status.into()).and_then(DomValue::as_str), Some("completed"));
		assert_eq!(custom(dom, jobs[0], "agent").and_then(DomValue::as_str), Some("scout"));
		assert_eq!(custom(dom, jobs[0], "delivered"), Some(&DomValue::Bool(true)));
		let id = job
			.prop(&PropId::Id.into())
			.and_then(DomValue::as_str)
			.expect("id");
		let child_path = journal.with_file_name(format!("{id}.oms"));
		let child = Session::open(&child_path, ComponentRegistry::standard()).expect("child");
		assert_eq!(omp_app::print_mode::transcript_text(child.dom()), "child result\n");
		// The spilled output is copied into the project blob store and named,
		// with the id v1 addressed it by.
		let artifact = tagged(dom, &Tag::Custom(Str::new_static("foreign-artifact")));
		assert_eq!(artifact.len(), 1);
		assert_eq!(
			dom.get(artifact[0])
				.and_then(|node| node.prop(&PropId::Name.into()))
				.and_then(DomValue::as_str),
			Some("0.bash.log")
		);
		assert_eq!(custom(dom, artifact[0], "v1-artifact"), Some(&DomValue::Int(0)));
	}

	/// The project blob store of the bucket `journal` landed in.
	fn project_store(journal: &Path) -> omp_journal::blob::BlobStore {
		let state = journal
			.parent()
			.and_then(Path::parent)
			.expect("<state>/sessions/<id>.oms");
		omp_journal::blob::BlobStore::open(omp_env::project_state::blob_store(state))
			.expect("project blob store")
	}

	#[test]
	fn v1_artifact_uris_stay_verbatim_and_map_to_the_copied_bytes() {
		let tree = Tree::new();
		let spilled = b"full output line 1\nfull output line 2\n";
		let parent = tree.transcript("spill", &[
			tree.header("spill"),
			user("u1", None, "run the build"),
			message("a1", Some("u1"), json!({"role": "assistant", "content": [{"type": "toolCall", "id": "call-1", "name": "bash", "arguments": {"command": "make"}}], "api": "anthropic-messages", "provider": "anthropic", "model": "claude-opus-4-5", "stopReason": "toolUse", "timestamp": 2})),
			message("t1", Some("a1"), json!({"role": "toolResult", "toolCallId": "call-1", "toolName": "bash", "content": [{"type": "text", "text": "full output line 2\n[raw output: artifact://0]"}], "isError": false, "timestamp": 3})),
			assistant("a2", Some("t1"), "see artifact://0 and the lost artifact://4"),
		]);
		let artifacts = parent.with_extension("");
		fs::create_dir_all(&artifacts).expect("artifact dir");
		fs::write(artifacts.join("0.bash.log"), spilled).expect("artifact");
		write_lines(&artifacts.join("0-Task.jsonl"), &[
			json!({"type": "session", "version": 3, "id": "task", "timestamp": "2026-01-02T03:05:00.000Z", "cwd": tree.project}),
			assistant("c1", None, "the parent's artifact://0"),
		]);
		let pair = tree.pair();

		let imported =
			import_session(&pair.target, &pair.source, &parent, &V1Converter).expect("import");

		// A missing artifact is reported, not fatal.
		assert_eq!(imported.missing_artifacts, [omp_driver::v1_import::MissingArtifact {
			transcript: parent,
			id:         4,
		}]);
		assert_eq!(imported.artifacts, 1);
		// Journaled text keeps v1's URI exactly.
		let text = fs::read_to_string(&imported.journal).expect("journal");
		assert!(text.contains("[raw output: artifact://0]"), "{text}");
		// The journal maps v1's id to the copied bytes in the project store,
		// where the environment's `artifact://` resolver reads.
		let entries = Journal::scan(&imported.journal).expect("scan");
		let mapped = omp_session::import::v1_artifacts(&entries).collect::<Vec<_>>();
		assert_eq!(mapped.len(), 1);
		assert_eq!(mapped[0].0, 0);
		let store = project_store(&imported.journal);
		assert_eq!(store.get(&mapped[0].1).expect("copied").as_ref(), spilled);
		// The subagent shares its parent's artifacts, as v1 subagents did.
		let session = Session::open(&imported.journal, ComponentRegistry::standard()).expect("open");
		let job = tagged(session.dom(), &Tag::Known(KnownTag::Subagent))[0];
		let child_id = session
			.dom()
			.get(job)
			.and_then(|node| node.prop(&PropId::Id.into()))
			.and_then(DomValue::as_str)
			.expect("child id")
			.to_owned();
		let child = imported.journal.with_file_name(format!("{child_id}.oms"));
		let child_entries = Journal::scan(&child).expect("child");
		assert_eq!(omp_session::import::v1_artifacts(&child_entries).collect::<Vec<_>>(), mapped);
		// The provenance the driver indexes is in the journal's first bytes.
		let origin = omp_session::import::import_origin(
			&Journal::scan_prefix(&imported.journal, omp_session::import::PROVENANCE_PREFIX_BYTES)
				.expect("prefix"),
		)
		.expect("origin");
		assert_eq!(origin.format, omp_session::import::OMP1_FORMAT);
		assert_eq!(origin.source_id.as_deref(), Some("spill"));
	}

	#[test]
	fn v1_bulk_import_places_by_directory_remaps_pins_and_is_idempotent() {
		let tree = Tree::new();
		tree.transcript("here", &[tree.header("here"), user("u1", None, "in the project")]);
		tree.transcript("gone", &[
			json!({"type": "session", "version": 3, "id": "gone", "timestamp": "2026-01-02T03:04:05.000Z", "cwd": tree.root.path().join("deleted-project")}),
			user("u1", None, "in a deleted project"),
		]);
		fs::write(tree.agent().join("session-pins.json"), "[\"gone\"]").expect("pins");
		let pair = tree.pair();
		let offline = omp_con::Ctx::new();
		let pairs = [pair.clone()];

		let report = run_with(
			&pairs,
			ImportMode::Apply,
			CredentialAccess::Offline(&offline),
			SessionImport::Bulk(&V1Converter),
		);

		let imported = report
			.entries()
			.filter(|entry| entry.step == ImportStep::Sessions)
			.filter(|entry| matches!(entry.outcome, ImportOutcome::Imported))
			.count();
		assert_eq!(imported, 2);
		let orphans = omp_env::project_state::no_directory(&pair.target.data_dir);
		let pinned: Vec<String> =
			serde_json::from_slice(&fs::read(orphans.join("session-pins.json")).expect("pins"))
				.expect("pin list");
		assert_eq!(pinned.len(), 1);
		let orphan = orphans.join("sessions").join(format!("{}.oms", pinned[0]));
		let session = Session::open(&orphan, ComponentRegistry::standard()).expect("resume");
		assert!(
			thread_texts(session.dom())
				.iter()
				.any(|text| text == "in a deleted project")
		);
		let project = omp_env::project_state::directory(&pair.target.data_dir, &tree.project)
			.expect("project state")
			.join("sessions");
		assert_eq!(
			fs::read_dir(&project)
				.expect("sessions")
				.filter(|entry| {
					entry
						.as_ref()
						.expect("entry")
						.path()
						.extension()
						.and_then(|value| value.to_str())
						== Some("oms")
				})
				.count(),
			1
		);

		// The marker set, a rerun still scans: the imported journals
		// themselves keep every session from converting again, and the picker
		// marks them.
		assert!(
			ImportStep::Sessions
				.marker(&pair.target.config_dir)
				.is_set()
		);
		let rerun = run_with(
			&pairs,
			ImportMode::Apply,
			CredentialAccess::Offline(&offline),
			SessionImport::Bulk(&V1Converter),
		);
		let skipped = rerun
			.entries()
			.filter(|entry| entry.step == ImportStep::Sessions)
			.filter(|entry| {
				matches!(entry.outcome, ImportOutcome::Skipped(SkipReason::SessionImported))
			})
			.count();
		assert_eq!(skipped, 2);
		let rows = list(&pair).expect("list");
		assert_eq!(rows.len(), 2);
		assert!(rows.iter().all(|row| {
			matches!(&row.imported, Some(PriorImport::Current(journal)) if journal.is_file())
		}));
	}

	fn sessions_outcomes(report: &ImportReport) -> Vec<(Option<String>, &ImportOutcome)> {
		report
			.entries()
			.filter(|entry| entry.step == ImportStep::Sessions)
			.map(|entry| (entry.subject.as_ref().map(ToString::to_string), &entry.outcome))
			.collect()
	}

	/// A v1 session that went on after its import is imported again, into a
	/// fresh journal holding the whole transcript; the earlier journal stays
	/// exactly as it was.
	#[test]
	fn a_v1_session_that_grew_imports_again_beside_its_earlier_journal() {
		let tree = Tree::new();
		let lines = [tree.header("grew"), user("u1", None, "first question")];
		let source = tree.transcript("grew", &lines);
		let pair = tree.pair();
		let pairs = [pair.clone()];
		let offline = omp_con::Ctx::new();
		let bulk = || {
			run_with(
				&pairs,
				ImportMode::Apply,
				CredentialAccess::Offline(&offline),
				SessionImport::Bulk(&V1Converter),
			)
		};
		let first = import_session(&pair.target, &pair.source, &source, &V1Converter)
			.expect("import")
			.journal;
		let first_bytes = fs::read(&first).expect("journal");

		tree.transcript("grew", &[
			lines[0].clone(),
			lines[1].clone(),
			assistant("a1", Some("u1"), "first answer"),
			user("u2", Some("a1"), "follow-up after the import"),
		]);

		let rows = list(&pair).expect("list");
		assert_eq!(rows[0].imported, Some(PriorImport::Changed(first.clone())));
		let dry = run_with(
			&pairs,
			ImportMode::DryRun,
			CredentialAccess::Offline(&offline),
			SessionImport::Bulk(&V1Converter),
		);
		assert!(matches!(sessions_outcomes(&dry)[..], [(_, ImportOutcome::WouldReimport)]));
		assert_eq!(fs::read(&first).expect("journal"), first_bytes);

		let report = bulk();
		assert!(matches!(sessions_outcomes(&report)[..], [(_, ImportOutcome::Reimported)]));
		assert_eq!(fs::read(&first).expect("earlier journal"), first_bytes, "never rewritten");
		let Some(PriorImport::Current(second)) = list(&pair).expect("list").remove(0).imported else {
			panic!("the fresh journal is the current import");
		};
		assert_ne!(second, first);
		let session = Session::open(&second, ComponentRegistry::standard()).expect("resume");
		let texts = thread_texts(session.dom());
		assert!(texts.iter().any(|text| text == "first question"), "{texts:?}");
		assert!(
			texts
				.iter()
				.any(|text| text == "follow-up after the import"),
			"{texts:?}"
		);
		let earlier = Session::open(&first, ComponentRegistry::standard()).expect("earlier");
		assert!(
			!thread_texts(earlier.dom())
				.iter()
				.any(|text| text == "follow-up after the import")
		);
		drop((session, earlier));
		// The fresh journal records the transcript's new digest: settled.
		assert!(matches!(sessions_outcomes(&bulk())[..], [(
			_,
			ImportOutcome::Skipped(SkipReason::SessionImported)
		)]));
	}

	/// The converter records the transcript's size and modification time
	/// beside its digest once the file settled, so a listing finds it
	/// current without reading it. v1 rewrites its padded title slot in
	/// place, keeping the size but moving the modification time: the listing
	/// then digests it and finds the change. A transcript modified moments
	/// before its import records no stamp.
	#[test]
	fn a_v1_import_records_the_transcript_stamp_and_a_same_size_retitle_is_caught() {
		use std::time::{Duration, SystemTime};

		use omp_session::import::{SourceStamp, import_origin};

		let tree = Tree::new();
		let title = |text: &str| json!({"type": "title", "v": 1, "title": text, "updatedAt": "2026-01-02T03:04:07.000Z", "pad": "    "});
		let source = tree.transcript("stamped", &[
			title("First title"),
			tree.header("stamped"),
			user("u1", None, "question"),
		]);
		fs::File::options()
			.write(true)
			.open(&source)
			.and_then(|file| file.set_modified(SystemTime::now() - Duration::from_secs(60)))
			.expect("settle");
		let (journal, session) = tree.import(&source);
		drop(session);
		let entries = Journal::scan(&journal).expect("journal");
		let origin = import_origin(&entries).expect("origin");
		let stat = fs::metadata(&source).expect("stat");
		assert!(origin.source_stamp.is_some());
		assert_eq!(origin.source_stamp, SourceStamp::of(&stat));
		assert_eq!(origin.source_stamp.map(|stamp| stamp.size), Some(stat.len()));
		let pair = tree.pair();
		assert_eq!(
			list(&pair).expect("list")[0].imported,
			Some(PriorImport::Current(journal.clone()))
		);

		let bytes = fs::read(&source).expect("transcript");
		let text = String::from_utf8(bytes).expect("utf-8");
		let retitled = text.replacen("First title", "Other title", 1);
		assert_eq!(retitled.len(), text.len());
		fs::write(&source, retitled).expect("retitle in place");
		assert_eq!(list(&pair).expect("list")[0].imported, Some(PriorImport::Changed(journal)));

		let fresh = tree.transcript("fresh", &[tree.header("fresh"), user("u1", None, "now")]);
		let (journal, session) = tree.import(&fresh);
		drop(session);
		let entries = Journal::scan(&journal).expect("journal");
		let origin = import_origin(&entries).expect("origin");
		assert_eq!(origin.source_stamp, None, "modified moments ago: nothing to trust");
		assert!(origin.source_digest.is_some());
	}

	/// `--dry-run --sessions` reports a referenced v1 artifact that is gone,
	/// as a real run does, and writes nothing.
	#[test]
	fn a_v1_bulk_dry_run_reports_missing_artifacts() {
		let tree = Tree::new();
		let source = tree.transcript("lost", &[
			tree.header("lost"),
			user("u1", None, "run it"),
			assistant("a1", Some("u1"), "kept artifact://0, lost artifact://3"),
		]);
		fs::create_dir_all(source.with_extension("")).expect("artifact dir");
		fs::write(source.with_extension("").join("0.bash.log"), "kept").expect("artifact");
		let pair = tree.pair();
		let pairs = [pair.clone()];
		let offline = omp_con::Ctx::new();
		let run = |mode| {
			run_with(
				&pairs,
				mode,
				CredentialAccess::Offline(&offline),
				SessionImport::Bulk(&V1Converter),
			)
		};

		let dry = run(ImportMode::DryRun);
		assert!(!pair.target.data_dir.exists(), "a dry run writes nothing");
		let outcomes = sessions_outcomes(&dry);
		assert!(
			matches!(outcomes[..], [
				(_, ImportOutcome::WouldImport),
				(Some(ref subject), ImportOutcome::NeedsAttention(Attention::ArtifactMissing)),
			] if subject == "lost artifact://3"),
			"{outcomes:?}"
		);

		let real = run(ImportMode::Apply);
		let outcomes = sessions_outcomes(&real);
		assert!(
			matches!(outcomes[..], [
				(_, ImportOutcome::Imported),
				(Some(ref subject), ImportOutcome::NeedsAttention(Attention::ArtifactMissing)),
			] if subject == "lost artifact://3"),
			"{outcomes:?}"
		);
	}
}
