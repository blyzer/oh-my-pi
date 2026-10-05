//! End-to-end coverage for built-in source tool contracts, including `read` and
//! `lsp` resolving one declaration by symbol through the production project
//! environment.

#![cfg(unix)]

use bytes::Bytes;
use omp_e2e::{
	Result, error,
	support::{DEFAULT_TIMEOUT, EnvHarness, Scratch, within},
};
use omp_env::{EnvClient, InvocationEvent};
use omp_proto::env::v1::InvokeTool;
use omp_tool::{CallOutcome, Registry};
use omp_tools::{
	lsp::{self, symbol::SymbolFault},
	read::{self, PayloadPart},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const FIXTURE_ROOT: &str =
	concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/tests/fixtures/special-sources");

/// The live `read` revision, taken from the tool's own spec so a revision bump
/// cannot strand these invocations.
fn read_rev() -> String {
	omp_tools::read::spec(omp_tools::read::ReadPolicy::default())
		.rev
		.to_string()
}

/// Runs one built-in invocation to its verdict and decodes the settled outcome,
/// whether it succeeded or faulted with `F`.
async fn invoke_outcome<F: DeserializeOwned>(
	client: &EnvClient,
	invocation_id: &str,
	name: &str,
	rev: &str,
	args: Value,
) -> Result<CallOutcome<Value, F>> {
	let mut invocation = within(
		"opening built-in invocation",
		DEFAULT_TIMEOUT,
		client.invoke(InvokeTool {
			invocation_id: invocation_id.to_owned(),
			name: name.to_owned(),
			rev: rev.to_owned(),
			..InvokeTool::default()
		}),
	)
	.await??;
	match within("built-in acceptance", DEFAULT_TIMEOUT, invocation.next_event()).await?? {
		Some(InvocationEvent::Accepted(_)) => {},
		Some(event) => return Err(error(format!("expected accepted event, got {event:?}"))),
		None => return Err(error(format!("built-in invocation closed before acceptance"))),
	}
	within(
		"committing built-in arguments",
		DEFAULT_TIMEOUT,
		invocation.commit_args(
			Bytes::from(serde_json::to_vec(&args)?),
			Bytes::from_static(b"tool-sources-test-token"),
			1000,
			None,
		),
	)
	.await??;
	loop {
		match within("built-in verdict", DEFAULT_TIMEOUT, invocation.next_event()).await?? {
			Some(InvocationEvent::Verdict(verdict)) => {
				return Ok(serde_json::from_slice::<CallOutcome<Value, F>>(&verdict.json)?);
			},
			Some(InvocationEvent::Update(_)) => {},
			Some(InvocationEvent::Accepted(_)) => {
				return Err(error(format!("built-in invocation was accepted twice")));
			},
			Some(InvocationEvent::Admission(_)) => {
				return Err(error(format!("unexpected admission in built-in invocation")));
			},
			None => return Err(error(format!("built-in invocation closed before its verdict"))),
		}
	}
}

async fn invoke_ok(
	client: &EnvClient,
	invocation_id: &str,
	name: &str,
	rev: &str,
	args: Value,
) -> Result<Value> {
	match invoke_outcome::<Value>(client, invocation_id, name, rev, args).await? {
		CallOutcome::Ok(payload) => Ok(payload),
		other => Err(error(format!("{name} returned a non-success outcome: {other:?}"))),
	}
}

fn checked_fixture(relative: &str) -> &'static [u8] {
	match relative {
		"archives/bundle.zip" => {
			include_bytes!("../../tools/tests/fixtures/special-sources/archives/bundle.zip")
		},
		"database/catalog.sqlite" => {
			include_bytes!("../../tools/tests/fixtures/special-sources/database/catalog.sqlite")
		},
		"images/pixel.png" => {
			include_bytes!("../../tools/tests/fixtures/special-sources/images/pixel.png")
		},
		"profiles/run.cpuprofile" => {
			include_bytes!("../../tools/tests/fixtures/special-sources/profiles/run.cpuprofile")
		},
		_ => panic!("unknown checked fixture {relative}"),
	}
}

#[tokio::test]
async fn production_env_reads_special_sources_and_shares_write_edit_snapshots() -> Result<()> {
	assert!(std::path::Path::new(FIXTURE_ROOT).is_dir());
	let scratch = Scratch::new()?;
	for relative in [
		"archives/bundle.zip",
		"database/catalog.sqlite",
		"images/pixel.png",
		"profiles/run.cpuprofile",
	] {
		scratch.write(relative, checked_fixture(relative))?;
	}
	let env = EnvHarness::spawn(&scratch, Registry::new()).await?;

	for (id, path) in [
		("read-archive", "archives/bundle.zip:dir/member.txt:raw"),
		("read-database", "database/catalog.sqlite:people:2"),
		("read-image", "images/pixel.png"),
		("read-profile", "profiles/run.cpuprofile"),
	] {
		let payload = invoke_ok(env.client(), id, "read", &read_rev(), json!({"path": path})).await?;
		assert!(payload.is_object(), "read payload for {path}: {payload}");
	}

	let initial = "alpha\nbeta\n";
	let write = invoke_ok(
		env.client(),
		"write-create",
		"write",
		"2",
		json!({"path":"roundtrip.txt", "content":initial}),
	)
	.await?;
	assert_eq!(write["byte_len"].as_u64(), Some(initial.len() as u64));
	assert_eq!(scratch.read("roundtrip.txt")?, initial.as_bytes());

	let tag = omp_edit::store::file_hash(&initial);
	let edit_input = format!("[roundtrip.txt#{tag}]\nPUT 2.=2:\n+gamma\n");
	invoke_ok(env.client(), "edit-after-write", "edit", "hl.1", json!({"input":edit_input})).await?;
	assert_eq!(scratch.read("roundtrip.txt")?, b"alpha\ngamma\n");

	let final_content = "final café 東京\n";
	let overwrite = invoke_ok(
		env.client(),
		"write-overwrite",
		"write",
		"2",
		json!({"path":"roundtrip.txt", "content":final_content}),
	)
	.await?;
	assert_eq!(overwrite["byte_len"].as_u64(), Some(final_content.len() as u64));
	assert_eq!(scratch.read("roundtrip.txt")?, final_content.as_bytes());

	env.shutdown().await?;
	Ok(())
}

/// Language server that echoes the position of every request it receives, so
/// the proof observes exactly where each symbol-addressed call landed, and that
/// reports document symbols with the same ranges tree-sitter finds.
const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys

def send(identifier, result):
    payload = json.dumps({"jsonrpc": "2.0", "id": identifier, "result": result}).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(payload) + payload)
    sys.stdout.buffer.flush()

def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        if name.lower() == "content-length":
            length = int(value.strip())
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

while True:
    message = read()
    if message is None:
        break
    method = message.get("method")
    params = message.get("params") or {}
    identifier = message.get("id")
    if identifier is None:
        continue
    position = params.get("position")
    uri = (params.get("textDocument") or {}).get("uri")
    if method == "initialize":
        send(identifier, {"capabilities": {"textDocumentSync": {"openClose": True, "change": 1}, "definitionProvider": True, "referencesProvider": True, "hoverProvider": True, "documentSymbolProvider": True}})
    elif method in ("textDocument/references", "textDocument/definition"):
        end = {"line": position["line"], "character": position["character"] + 1}
        send(identifier, [{"uri": uri, "range": {"start": position, "end": end}}])
    elif method == "textDocument/hover":
        send(identifier, {"contents": "position %d:%d" % (position["line"], position["character"])})
    elif method == "textDocument/documentSymbol":
        def at(start, end):
            return {"uri": uri, "range": {"start": {"line": start[0], "character": start[1]}, "end": {"line": end[0], "character": end[1]}}}
        send(identifier, [
            {"name": "Widget", "kind": 23, "location": at((0, 0), (0, 14))},
            {"name": "new", "kind": 6, "containerName": "Widget", "location": at((3, 1), (6, 2))},
        ])
    else:
        send(identifier, None)
"#;

/// `Widget` is declared twice (`struct Widget` on line 1, `impl Widget` on
/// lines 3-8); `Widget.new` is the one method, whose doc comment starts the
/// declaration on line 4 and whose name is on line 5.
const LIB: &str = "struct Widget;\n\nimpl Widget {\n\t/// Make one.\n\tpub fn new() -> Self \
                   {\n\t\tWidget\n\t}\n}\n";

/// The declaration a `read path:@symbol` returned, taken from its numbered
/// projection.
struct ReadDeclaration {
	/// First numbered line, doc comments included.
	first:     u32,
	/// Last numbered line.
	last:      u32,
	/// One-based line of the declaration's own name.
	name_line: u32,
}

/// Parses the `N:content` lines below the `[path#TAG]` header of a read payload
/// and locates the line holding `name_marker`.
fn read_declaration(payload: Value, name_marker: &str) -> Result<ReadDeclaration> {
	let payload = serde_json::from_value::<read::Payload>(payload)?;
	let [PayloadPart::Text { text }] = payload.parts.as_slice() else {
		return Err(error("a source read is exactly one text part"));
	};
	let mut numbered = Vec::new();
	let mut name_line = None;
	for line in text.as_str().lines().skip(1) {
		let Some((number, content)) = line.split_once(':') else {
			continue;
		};
		let Ok(number) = number.parse::<u32>() else {
			continue;
		};
		if content.contains(name_marker) {
			name_line = Some(number);
		}
		numbered.push(number);
	}
	Ok(ReadDeclaration {
		first:     *numbered.first().context("read returned no lines")?,
		last:      *numbered.last().context("read returned no lines")?,
		name_line: name_line.context("the read declaration holds its name line")?,
	})
}

async fn invoke_lsp(env: &EnvHarness, id: &str, args: Value) -> Result<lsp::Payload> {
	let rev = lsp::spec().rev.to_string();
	let payload = invoke_ok(env.client(), id, "lsp", &rev, args).await?;
	Ok(serde_json::from_value(payload)?)
}

async fn lsp_fault(env: &EnvHarness, id: &str, args: Value) -> Result<lsp::Fault> {
	let rev = lsp::spec().rev.to_string();
	match invoke_outcome::<lsp::Fault>(env.client(), id, "lsp", &rev, args).await? {
		CallOutcome::Faulted(fault) => Ok(fault),
		_ => Err(error("lsp was expected to fault")),
	}
}

async fn read_fault(env: &EnvHarness, id: &str, path: &str) -> Result<read::Fault> {
	match invoke_outcome::<read::Fault>(env.client(), id, "read", &read_rev(), json!({"path": path}))
		.await?
	{
		CallOutcome::Faulted(fault) => Ok(fault),
		_ => Err(error("read was expected to fault")),
	}
}

/// Writes the source file and a project-local `.lsp.json` binding `.rs` files
/// to the echoing fake server.
fn write_project(scratch: &Scratch) -> Result<()> {
	use std::os::unix::fs::PermissionsExt as _;

	let server = scratch.write("fake-lsp.py", FAKE_SERVER)?;
	std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700))?;
	scratch.write("foo.marker", b"")?;
	scratch.write("src/lib.rs", LIB)?;
	scratch.write(
		".lsp.json",
		serde_json::to_vec(&json!({
			"servers": {
				"fake": {
					"command": server,
					"args": [],
					"fileTypes": [".rs"],
					"rootMarkers": ["foo.marker"],
				}
			}
		}))?,
	)?;
	Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lsp_symbol_and_read_symbol_resolve_the_same_declaration() -> Result<()> {
	let scratch = Scratch::new()?;
	write_project(&scratch)?;
	let env = EnvHarness::spawn_project(&scratch).await?;

	// `read path:@Widget.new` reads the declaration, doc comment first.
	let by_symbol = invoke_ok(
		env.client(),
		"read-symbol",
		"read",
		&read_rev(),
		json!({"path": "src/lib.rs:@Widget.new"}),
	)
	.await?;
	let declared = read_declaration(by_symbol.clone(), "pub fn new")?;
	assert_eq!((declared.first, declared.last, declared.name_line), (4, 7, 5));
	let by_range = invoke_ok(
		env.client(),
		"read-range",
		"read",
		&read_rev(),
		json!({"path": format!("src/lib.rs:{}-{}", declared.first, declared.last)}),
	)
	.await?;
	assert_eq!(by_symbol, by_range, "a symbol read is the range read of its declaration");

	// A symbol-only `lsp` call (no `line`) lands on that declaration's name.
	let name_position = json!({"line": declared.name_line - 1, "character": 8});
	for (id, action) in [("lsp-references", "references"), ("lsp-definition", "definition")] {
		let payload = invoke_lsp(
			&env,
			id,
			json!({"action": action, "file": "src/lib.rs", "symbol": "Widget.new"}),
		)
		.await?;
		let servers = payload.servers.iter().map(|name| name.as_str());
		assert!(servers.eq(["fake"]), "{action} reaches the configured server");
		assert_eq!(payload.data[0]["range"]["start"], name_position, "{action} by symbol");
	}
	let hover = invoke_lsp(
		&env,
		"lsp-hover",
		json!({"action": "hover", "file": "src/lib.rs", "symbol": "Widget.new"}),
	)
	.await?;
	assert_eq!(hover.output.as_str(), format!("position {}:8", declared.name_line - 1));

	// The symbols projection advertises the range `read` resolves to.
	let symbols =
		invoke_lsp(&env, "lsp-symbols", json!({"action": "symbols", "file": "src/lib.rs"})).await?;
	assert!(
		symbols
			.output
			.contains(&format!("method new in Widget @ lines {}-{}", declared.first, declared.last)),
		"{}",
		symbols.output
	);

	env.shutdown().await?;
	Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lsp_symbol_and_read_symbol_report_the_same_candidates_and_misses() -> Result<()> {
	let scratch = Scratch::new()?;
	write_project(&scratch)?;
	let env = EnvHarness::spawn_project(&scratch).await?;

	// `Widget` names both the struct (line 1) and the impl (lines 3-8): neither
	// tool guesses, and both list the same ranges.
	let ambiguous = lsp_fault(
		&env,
		"lsp-ambiguous",
		json!({"action": "references", "file": "src/lib.rs", "symbol": "Widget"}),
	)
	.await?;
	let lsp_text = ambiguous.to_string();
	let lsp::Fault::Symbol(SymbolFault::Ambiguous { candidates, .. }) = ambiguous else {
		return Err(error("an ambiguous lsp symbol is a typed ambiguity fault"));
	};
	let ranges = candidates
		.iter()
		.map(|candidate| (candidate.start_line, candidate.end_line, candidate.name_line))
		.collect::<Vec<_>>();
	assert_eq!(ranges, [(1, 1, 1), (3, 8, 3)]);
	let read_text = read_fault(&env, "read-ambiguous", "src/lib.rs:@Widget")
		.await?
		.message()
		.to_string();
	for (start, end, name_line) in ranges {
		let range = format!("src/lib.rs:{start}-{end}");
		assert!(lsp_text.contains(&format!("{range} (line={name_line})")), "{lsp_text}");
		assert!(read_text.contains(&range), "{read_text}");
	}

	// An unknown member is a miss in both, never a guess.
	let missing_lsp = lsp_fault(
		&env,
		"lsp-missing",
		json!({"action": "definition", "file": "src/lib.rs", "symbol": "Widget.missing"}),
	)
	.await?;
	assert!(
		matches!(
			&missing_lsp,
			lsp::Fault::Symbol(SymbolFault::NotFound { query, .. }) if query.as_str() == "Widget.missing"
		),
		"{missing_lsp:?}"
	);
	let missing_read = read_fault(&env, "read-missing", "src/lib.rs:@Widget.missing").await?;
	assert!(
		missing_read
			.message()
			.starts_with("No declaration matches symbol ':@Widget.missing'"),
		"{missing_read:?}"
	);

	env.shutdown().await?;
	Ok(())
}
