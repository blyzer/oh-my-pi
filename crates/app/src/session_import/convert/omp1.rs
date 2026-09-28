//! omp v1 (the TypeScript `omp`) transcripts.
//!
//! A v1 transcript is JSONL: an optional fixed-width `title` slot, the
//! `session` header (version 3; older headers migrate first, as v1's
//! `session-migrations.ts` did), then entries forming an `id`/`parentId`
//! tree. `message` entries carry `@oh-my-pi/pi-ai` messages (`user`,
//! `developer`, `assistant`, `toolResult`, plus the app roles `custom`,
//! `bashExecution`, `pythonExecution`, …). Images may be externalized to v1's
//! blob store as `blob:sha256:<hex>`.
//!
//! Conversion reuses the Claude Code and Codex machinery: every entry keeps
//! its raw record as `foreign-record`, the tree replays through rewinds,
//! compactions and `/clear` boundaries become native compactions, and
//! provider-native state v2 cannot replay (thinking signatures, prompt-cache
//! keys, credential pins) stays foreign metadata only.

use std::{borrow::Cow, fmt::Write as _, fs, path::Path};

use miette::{IntoDiagnostic as _, miette};
use omp_core::{FastHashMap, Str, base64, sf};
use omp_dom::{KnownTag, Op, PropId, PropKey, Txn, Value as DomValue};
use omp_journal::{EntryId, data::Compaction};
use omp_session::{Session, components::jobs};
use serde_json::{Map, Value, json};

use super::{
	ImportState, Omp1Context, SourceMetadata, SourceRecord, SourceTimestamp, append_assistant_error,
	append_foreign_meta, append_foreign_role, append_receipt, ensure_turn, head, import_tool_call,
	import_tool_result, insert_assistant_content, last_child_with_tag, last_turn,
	materialize_content, object, patch_record, patch_string, resolve_parent, source_timestamp,
	string,
};

/// Prefix of an image externalized to v1's blob store.
const BLOB_PREFIX: &str = "blob:sha256:";

fn kind(value: &Value) -> Option<&str> {
	value.get("type").and_then(Value::as_str)
}

/// Brings pre-v3 entries to v3 in place, as v1's `migrateToCurrentVersion`
/// does: version 1 gains the `id`/`parentId` chain (and compactions their
/// `firstKeptEntryId`), and version 2's `hookMessage` role becomes `custom`.
pub(super) fn migrate(records: &mut [SourceRecord]) {
	let version = records
		.iter()
		.find(|record| kind(&record.value) == Some("session"))
		.and_then(|record| record.value.get("version"))
		.and_then(Value::as_u64)
		.unwrap_or(1);
	if version < 2 {
		// v1 indexes `firstKeptEntryIndex` into its entries after the title
		// slot is stripped, header included.
		let mut ids = Vec::<Option<Str>>::with_capacity(records.len());
		let mut previous = None::<Str>;
		for record in records.iter_mut() {
			let Some(entry) = record.value.as_object_mut() else {
				continue;
			};
			match entry.get("type").and_then(Value::as_str) {
				Some("title") => continue,
				Some("session") => {
					ids.push(None);
					continue;
				},
				_ => {},
			}
			let id = sf!("v1-{}", ids.len());
			entry.insert("id".to_owned(), Value::String(id.to_string()));
			entry.insert(
				"parentId".to_owned(),
				previous
					.as_ref()
					.map_or(Value::Null, |parent| Value::String(parent.to_string())),
			);
			if entry.get("type").and_then(Value::as_str) == Some("compaction")
				&& let Some(index) = entry.get("firstKeptEntryIndex").and_then(Value::as_u64)
			{
				entry.remove("firstKeptEntryIndex");
				if let Some(Some(kept)) = usize::try_from(index).ok().and_then(|index| ids.get(index)) {
					entry.insert("firstKeptEntryId".to_owned(), Value::String(kept.to_string()));
				}
			}
			previous = Some(id.clone());
			ids.push(Some(id));
		}
	}
	if version < 3 {
		for record in records.iter_mut() {
			if kind(&record.value) != Some("message") {
				continue;
			}
			if let Some(role) = record.value.pointer_mut("/message/role")
				&& role.as_str() == Some("hookMessage")
			{
				*role = Value::String("custom".to_owned());
			}
		}
	}
}

/// v1's session id and current title: the title slot, else the latest
/// `title_change`, else the header's.
pub(super) fn metadata(records: &[SourceRecord], metadata: &mut SourceMetadata) {
	let mut slot = None;
	let mut changed = None;
	for record in records {
		let value = object(&record.value);
		match kind(&record.value) {
			Some("session") => {
				if let Some(id) = string(value, "id") {
					metadata.id = Some(id);
				}
			},
			Some("title") => slot = string(value, "title"),
			Some("title_change") => changed = string(value, "title").or(changed),
			_ => {},
		}
	}
	if let Some(title) = slot.or(changed) {
		metadata.title = Some(title);
	}
}

/// Replays the entry tree onto `session`, then links its converted
/// subagents.
pub(super) fn import(
	session: &mut Session,
	records: &[SourceRecord],
	base: EntryId,
	state: &mut ImportState,
	v1: &Omp1Context<'_>,
) -> miette::Result<()> {
	let entries = || {
		records
			.iter()
			.filter(|record| !matches!(kind(&record.value), Some("session" | "title")))
	};
	let mut parents = FastHashMap::<Str, Option<Str>>::default();
	for record in entries() {
		let value = object(&record.value);
		if let Some(id) = string(value, "id") {
			parents.insert(id, string(value, "parentId"));
		}
	}
	// `tails`: the journal head after each entry; `starts`: before it, which
	// is what a later compaction hides up to.
	let mut tails = FastHashMap::<Str, EntryId>::default();
	let mut starts = FastHashMap::<Str, EntryId>::default();
	for record in entries() {
		let value = object(&record.value);
		let id = string(value, "id").unwrap_or_else(|| sf!("line-{}", record.line));
		let parent = string(value, "parentId");
		let target = if value.contains_key("parentId") {
			resolve_parent(parent.as_ref(), &parents, &tails, base)
		} else {
			head(session)?
		};
		if head(session)? != target {
			session.rewind(target).into_diagnostic()?;
		}
		starts.insert(id.clone(), target);
		let stamp = source_timestamp(&record.value, state.fallback_ms);
		match kind(&record.value).unwrap_or_default() {
			"message" => message(session, record, &id, parent.as_ref(), &stamp, state, v1)?,
			"compaction" => compaction(session, record, &starts, base, &stamp)?,
			"reset_boundary" => reset(session, record, &stamp)?,
			"branch_summary" => append_foreign_role(
				session,
				record,
				"branchSummary",
				value.get("summary").unwrap_or(&Value::Null),
				&stamp,
			)?,
			"custom_message" => append_foreign_role(
				session,
				record,
				"custom",
				&resolve_images(value.get("content").unwrap_or(&Value::Null), v1.blobs),
				&stamp,
			)?,
			// Settings changes, labels, titles, usage of side calls, TTSR
			// injections, subagent init, modes, and credential pins: foreign
			// metadata only.
			_ => append_foreign_meta(session, record, "foreign-entry", &stamp)?,
		}
		tails.insert(id, head(session)?);
	}
	children(session, v1)
}

fn message(
	session: &mut Session,
	record: &SourceRecord,
	id: &Str,
	parent: Option<&Str>,
	stamp: &SourceTimestamp,
	state: &mut ImportState,
	v1: &Omp1Context<'_>,
) -> miette::Result<()> {
	let Some(message) = object(&record.value)
		.get("message")
		.and_then(Value::as_object)
	else {
		return append_foreign_meta(session, record, "foreign-entry", stamp);
	};
	let role = message
		.get("role")
		.and_then(Value::as_str)
		.unwrap_or_default();
	let content = message.get("content").unwrap_or(&Value::Null);
	match role {
		"user" => user(session, record, &resolve_images(content, v1.blobs), id, parent, stamp, state),
		"assistant" => assistant(session, record, message, id, parent, stamp, state),
		"toolResult" => import_tool_result(
			session,
			record,
			message.get("toolCallId").and_then(Value::as_str),
			message.get("toolName").and_then(Value::as_str),
			&resolve_images(content, v1.blobs),
			message.get("isError").and_then(Value::as_bool) == Some(true),
			stamp,
			state,
		),
		"bashExecution" | "pythonExecution" => append_foreign_role(
			session,
			record,
			role,
			&Value::String(execution_text(role, message)),
			stamp,
		),
		"branchSummary" | "compactionSummary" => append_foreign_role(
			session,
			record,
			role,
			message.get("summary").unwrap_or(&Value::Null),
			stamp,
		),
		"developer" | "custom" => {
			append_foreign_role(session, record, role, &resolve_images(content, v1.blobs), stamp)
		},
		_ => append_foreign_meta(session, record, "foreign-entry", stamp),
	}
}

fn user(
	session: &mut Session,
	record: &SourceRecord,
	content: &Value,
	id: &Str,
	parent: Option<&Str>,
	stamp: &SourceTimestamp,
	state: &mut ImportState,
) -> miette::Result<()> {
	let (text, attachments) = materialize_content(session, content)?;
	if text.is_empty() && attachments.is_empty() {
		return append_foreign_meta(session, record, "foreign-entry", stamp);
	}
	session.begin_turn().into_diagnostic()?;
	session.user(text, attachments).into_diagnostic()?;
	let user = last_child_with_tag(session, last_turn(session)?, KnownTag::User)?;
	patch_record(session, user, record, Some(id), parent, stamp, "foreign.message")?;
	state.messages += 1;
	Ok(())
}

fn assistant(
	session: &mut Session,
	record: &SourceRecord,
	message: &Map<String, Value>,
	id: &Str,
	parent: Option<&Str>,
	stamp: &SourceTimestamp,
	state: &mut ImportState,
) -> miette::Result<()> {
	ensure_turn(session)?;
	if let Some(model) = string(message, "model") {
		state.model = model;
	}
	if let Some(provider) = string(message, "provider") {
		state.provider = provider;
	}
	if let Some(api) = string(message, "api") {
		state.route = api;
	}
	session
		.assistant_start(state.model.clone(), state.provider.clone(), state.route.clone())
		.into_diagnostic()?;
	let assistant = last_child_with_tag(session, last_turn(session)?, KnownTag::Assistant)?;
	let mut index = 0_i64;
	// Redacted thinking, provider fallback markers, server-tool blocks, and
	// assistant images have no native block; the raw record keeps them.
	for block in message
		.get("content")
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
	{
		match kind(block) {
			Some("text") => {
				if let Some(text) = block.get("text").and_then(Value::as_str) {
					insert_assistant_content(session, assistant, "text", text, index, block)?;
					index += 1;
				}
			},
			Some("thinking") => {
				// The block, with its provider signature, stays `foreign-block`
				// metadata: v2 never replays a v1 signature.
				if let Some(text) = block.get("thinking").and_then(Value::as_str) {
					insert_assistant_content(session, assistant, "thinking", text, index, block)?;
					index += 1;
				}
			},
			Some("toolCall") => {
				import_tool_call(session, record, block, stamp, Some(index))?;
				index += 1;
			},
			_ => {},
		}
	}
	session
		.assistant_end(string(message, "stopReason").unwrap_or_else(|| Str::new_static("stop")))
		.into_diagnostic()?;
	patch_record(session, assistant, record, Some(id), parent, stamp, "foreign.message")?;
	if let Some(response) = message.get("responseId").and_then(Value::as_str) {
		patch_string(session, assistant, "foreign-response-id", response, "foreign.response")?;
	}
	if let Some(error) = message.get("errorMessage").and_then(Value::as_str) {
		let status = message.get("errorStatus").and_then(Value::as_i64);
		append_assistant_error(session, assistant, error, status, record)?;
	}
	if let Some(usage) = message.get("usage").and_then(Value::as_object) {
		append_receipt(session, usage, stamp, state.provider.as_str(), state.model.as_str())?;
	}
	state.messages += 1;
	Ok(())
}

/// A v1 compaction becomes a native one hiding everything before its first
/// kept entry; the entry itself stays foreign metadata. A compaction whose
/// kept entry is unknown (or hides nothing) is metadata only.
fn compaction(
	session: &mut Session,
	record: &SourceRecord,
	starts: &FastHashMap<Str, EntryId>,
	base: EntryId,
	stamp: &SourceTimestamp,
) -> miette::Result<()> {
	let value = object(&record.value);
	let boundary = string(value, "firstKeptEntryId")
		.and_then(|kept| starts.get(&kept).copied())
		.filter(|boundary| *boundary != base);
	if let (Some(boundary), Some(summary)) = (boundary, value.get("summary").and_then(Value::as_str))
	{
		ensure_turn(session)?;
		let summary = session
			.store_attachment("text/plain", summary.as_bytes())
			.into_diagnostic()?
			.blob;
		let mut native = Compaction::new(summary, boundary);
		native.method = string(value, "method");
		native.tokens_before = value.get("tokensBefore").and_then(Value::as_u64);
		native.tokens_after = value.get("tokensAfter").and_then(Value::as_u64);
		native.warning = string(value, "warning");
		session.compaction(native).into_diagnostic()?;
	}
	append_foreign_meta(session, record, "foreign-compaction", stamp)
}

/// v1's `/clear` boundary: a native compaction with an empty summary at the
/// head, as v2's own `/clear` records.
fn reset(
	session: &mut Session,
	record: &SourceRecord,
	stamp: &SourceTimestamp,
) -> miette::Result<()> {
	if !session.dom().children(session.dom().body()).is_empty() {
		let summary = session
			.store_attachment("text/plain", b"")
			.into_diagnostic()?
			.blob;
		let mut native = Compaction::new(summary, head(session)?);
		native.method = Some(Str::new_static("clear"));
		session.compaction(native).into_diagnostic()?;
	}
	append_foreign_meta(session, record, "foreign-entry", stamp)
}

/// The blob hash of an externalized v1 image block.
fn blob_hash(block: &Value) -> Option<&str> {
	if kind(block) != Some("image") {
		return None;
	}
	block
		.get("data")
		.and_then(Value::as_str)
		.and_then(|data| data.strip_prefix(BLOB_PREFIX))
}

/// Inlines `blob:sha256:<hex>` image data from v1's blob store so the shared
/// content helpers see ordinary base64 images. A missing blob (or a malformed
/// reference, which v1 itself refused to resolve) becomes a text note.
fn resolve_images<'v>(content: &'v Value, blobs: Option<&Path>) -> Cow<'v, Value> {
	let Some(blocks) = content.as_array() else {
		return Cow::Borrowed(content);
	};
	if !blocks.iter().any(|block| blob_hash(block).is_some()) {
		return Cow::Borrowed(content);
	}
	Cow::Owned(Value::Array(
		blocks
			.iter()
			.map(|block| {
				let Some(hash) = blob_hash(block) else {
					return block.clone();
				};
				let canonical = hash.len() == 64
					&& hash
						.bytes()
						.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
				let bytes = blobs
					.filter(|_| canonical)
					.and_then(|blobs| fs::read(blobs.join(hash)).ok());
				match bytes {
					Some(bytes) => {
						let mut block = block.clone();
						block["data"] = Value::String(base64::encode(&bytes).into_string());
						block
					},
					None => json!({
						"type": "text",
						"text": format!("[v1 image {BLOB_PREFIX}{hash} is missing]"),
					}),
				}
			})
			.collect(),
	))
}

/// v1's `bashExecutionToText` / `pythonExecutionToText`.
fn execution_text(role: &str, message: &Map<String, Value>) -> String {
	let field = |key| message.get(key).and_then(Value::as_str).unwrap_or_default();
	let output = field("output");
	let mut text = String::with_capacity(output.len() + 64);
	if role == "bashExecution" {
		let _ = writeln!(text, "Ran `{}`", field("command"));
	} else {
		let _ = write!(text, "Ran Python:\n```python\n{}\n```\n", field("code"));
	}
	if output.is_empty() {
		text.push_str("(no output)");
	} else {
		if role != "bashExecution" {
			text.push_str("Output:\n");
		}
		let _ = write!(text, "```\n{output}\n```");
	}
	if message.get("cancelled").and_then(Value::as_bool) == Some(true) {
		text.push_str(if role == "bashExecution" {
			"\n\n(command cancelled)"
		} else {
			"\n\n(execution cancelled)"
		});
	} else if let Some(code) = message
		.get("exitCode")
		.and_then(Value::as_i64)
		.filter(|code| *code != 0)
	{
		let _ = write!(text, "\n\nCommand exited with code {code}");
	}
	text
}

/// Names every v1 artifact the driver copied into the project blob store
/// (spilled tool output, agent outputs) as a `<meta><foreign-artifact>`, with
/// the id v1's `artifact://<id>` resolved to
/// ([`omp_session::import::foreign_artifact`]). Journaled text keeps v1's
/// URIs; the environment's `artifact://` resolver maps them through these
/// nodes.
pub(super) fn artifacts(
	session: &mut Session,
	artifacts: &[omp_driver::v1_import::V1Artifact],
) -> miette::Result<()> {
	for artifact in artifacts {
		let mime = match Path::new(artifact.name.as_str())
			.extension()
			.and_then(|value| value.to_str())
		{
			Some("log" | "md" | "txt") => "text/plain",
			Some("json") => "application/json",
			_ => "application/octet-stream",
		};
		let node = omp_session::import::foreign_artifact(
			artifact.name.clone(),
			artifact.blob,
			Str::new_static(mime),
			artifact.id,
		);
		let meta = session.dom().meta();
		let cause = head(session)?;
		session
			.patch(Txn {
				cause,
				label: Some(Str::new_static("foreign.artifact")),
				ops: vec![Op::Ins {
					parent: meta,
					after: session.dom().children(meta).last().copied(),
					node,
				}],
			})
			.into_diagnostic()?;
	}
	Ok(())
}

/// Links each converted subagent journal as a settled, delivered subagent
/// job: its journal is `<job id>.oms` beside this one, where native children
/// live, and delivery is not re-armed on resume.
fn children(session: &mut Session, v1: &Omp1Context<'_>) -> miette::Result<()> {
	for child in v1.children {
		let cause = head(session)?;
		let insert = jobs::insert(session.dom(), cause, jobs::JobSpec {
			id:      child.id.clone(),
			kind:    Str::new_static("subagent"),
			owner:   Str::new(v1.id),
			started: sf!("{}", child.started_ms),
			agent:   Some(child.agent.clone()),
		})
		.ok_or_else(|| miette!("imported session has no jobs component"))?;
		session.patch(insert).into_diagnostic()?;
		let handle = jobs::jobs_handle(session.dom())
			.and_then(|jobs| session.dom().children(jobs).last().copied())
			.ok_or_else(|| miette!("imported subagent job is absent"))?;
		let cause = head(session)?;
		session
			.patch(Txn {
				cause,
				label: Some(Str::new_static("foreign.subagent")),
				ops: vec![
					Op::Set {
						h:     handle,
						prop:  PropId::Status.into(),
						value: DomValue::Str(Str::new_static("completed")),
					},
					Op::Set {
						h:     handle,
						prop:  PropKey::Custom(Str::new_static(omp_agent::jobs::DELIVERED)),
						value: DomValue::Bool(true),
					},
					Op::Set {
						h:     handle,
						prop:  PropId::Label.into(),
						value: DomValue::Str(child.source_id.clone()),
					},
					Op::Set {
						h:     handle,
						prop:  PropKey::Custom(Str::new_static(omp_session::import::IMPORT_SOURCE_ID)),
						value: DomValue::Str(child.source_id.clone()),
					},
				],
			})
			.into_diagnostic()?;
	}
	Ok(())
}
