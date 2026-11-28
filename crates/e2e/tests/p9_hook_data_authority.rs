//! P9: a hook holds no DATA authority, so the HTTP request `remote-approve`
//! makes from its hooks is refused before it leaves the extension host
//! (ADR 0076, item 4).
//!
//! ADR 0076 says, from reading the code, that every site that builds the
//! authority of a hook or lifecycle callback sets `data: None`, and that
//! `AuthorityTable::open` is called in production only for a routed tool call.
//! This proof runs it. It registers one extension behind the environment
//! authority's CONTROL host and lets four callers make the identical request,
//! `omp.env.http_get(url, timeout=10s)`, which is the call `remote-approve`'s
//! `_request` makes:
//!
//! - a **tool** that declares a fetch effect, which holds DATA authority;
//! - an `extension_activate` **hook**, the event `remote-approve` uses to
//!   reconcile its tickets, which the host dispatches from the lifecycle with
//!   no DATA authority; and
//! - an `agent_settled` **hook**, which the host's hook gate dispatches with no
//!   DATA authority either; and
//! - a `turn_start` **hook** in the `observe` phase, the other event
//!   `remote-approve` subscribes to, which the same gate dispatches once per
//!   turn with no DATA authority either.
//!
//! The endpoint is a loopback server that counts the requests it receives. The
//! tool's request arrives; no hook's does, and each hook records the error
//! it got, so the refusal is observed and not inferred from a missing request.
//!
//! The extension is a trusted workspace fixture (`PythonExtension::trusting`),
//! as in `p9_agent_settled`, not an operator-installed user extension in the
//! sandboxed tier. No hook authority depends on the tier.
#![cfg(unix)]

use std::{
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::Duration,
};

use bytes::Bytes;
use omp_agent::{AgentSettled, LifecycleHooks};
use omp_e2e::{
	Context as _, Result, error,
	support::{InProcessEnvHarness, PythonExtension, Scratch, within},
};
use omp_env::{EnvClient, InvocationEvent};
use omp_proto::{env::v1::InvokeTool, toolhost::v1::HookEventId};
use omp_tool::CallOutcome;
use serde_json::{Value, json};
use tokio::{
	io::{AsyncReadExt as _, AsyncWriteExt as _},
	net::TcpListener,
};

const MODULE: &str = "hookproof";
const STEP_LIMIT: Duration = Duration::from_secs(30);

/// The extension: one request, made by a tool and by a hook.
///
/// `attempt` is what `remote-approve`'s `_request` does, with the same
/// arguments. Each caller records what the call did, and the hook stores its
/// record where `report` can read it, because an observe hook has no return
/// value. The endpoint URL is read when the call is made, from a file the test
/// writes next to the module, because the port is only known at run time.
const SOURCE: &str = r#"
import os

import omp
from omp.devices import Effects, FetchEffects

HOOK = {}


def endpoint():
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "endpoint.txt")) as handle:
        return handle.read().strip()


async def attempt():
    try:
        response = await omp.env.http_get(endpoint(), timeout=omp.Duration("10s"))
    except Exception as error:
        return {
            "outcome": "refused",
            "error": type(error).__name__,
            "message": str(error),
        }
    return {
        "outcome": "sent",
        "status": response.status,
        "body": response.body.decode(),
    }


@omp.hook("extension_activate", phase=omp.HookPhase.OBSERVE)
async def probe_on_activate(event, ctx):
    del event
    HOOK["activate"] = await attempt()


@omp.hook("turn_start", phase=omp.HookPhase.OBSERVE)
async def probe_on_turn_start(event, ctx):
    del event
    HOOK["turn_start"] = await attempt()


@omp.hook("agent_settled")
async def probe_on_settled(event, ctx):
    del event
    HOOK["settled"] = await attempt()
    return omp.agents.Settle()


@omp.tool("from_tool", rev=1, effects=Effects(fetch=FetchEffects()))
async def from_tool():
    return {"parts": [], "details": {"proof": "from_tool", "attempt": await attempt()}}


@omp.tool("report", rev=1)
async def report():
    return {"parts": [], "details": {"proof": "report", "hook": dict(HOOK)}}
"#;

/// A loopback HTTP endpoint that answers every request with the pending state
/// `remote-approve` understands, and counts what it receives.
async fn endpoint() -> Result<(u16, Arc<AtomicUsize>)> {
	let listener = TcpListener::bind(("127.0.0.1", 0))
		.await
		.context("bind endpoint")?;
	let port = listener.local_addr().context("endpoint address")?.port();
	let accepted = Arc::new(AtomicUsize::new(0));
	let count = Arc::clone(&accepted);
	tokio::spawn(async move {
		while let Ok((mut stream, _)) = listener.accept().await {
			count.fetch_add(1, Ordering::SeqCst);
			tokio::spawn(async move {
				let mut chunk = [0_u8; 2048];
				let _ = stream.read(&mut chunk).await;
				let body = r#"{"state":"pending"}"#;
				let response = format!(
					"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
					 {}\r\nConnection: close\r\n\r\n{body}",
					body.len()
				);
				let _ = stream.write_all(response.as_bytes()).await;
				let _ = stream.shutdown().await;
			});
		}
	});
	Ok((port, accepted))
}

/// A `turn_start` payload shaped like the one the agent loop publishes, with
/// `thinking` as `reasoning_effort` reports it for a request that names none.
fn turn_start_payload() -> Value {
	json!({
		"turn_id": "t1",
		"turn_index": 0,
		"prompt_hash": "p",
		"toolset_hash": "t",
		"enabled_tools": [],
		"input_mode": "full",
		"model": {"provider": "acme", "api": "", "model": "m"},
		"route": {"provider": "acme", "route": "acme/m"},
		"thinking": "off",
		"deadline": null,
		"attempt": 0,
		"prompt_changed": true,
		"toolset_changed": false,
	})
}

fn settled_payload() -> Value {
	json!({
		"submission_id": "s1",
		"reason": "stop",
		"committed_turns": 1,
		"last_stop": "stop",
		"pending_jobs": [],
		"continuations_used": 0,
		"incomplete_todos": [],
	})
}

fn find_proof(value: &Value, proof: &str) -> Option<Value> {
	match value {
		Value::Object(map) if map.get("proof").and_then(Value::as_str) == Some(proof) => {
			Some(value.clone())
		},
		Value::Object(map) => map.values().find_map(|inner| find_proof(inner, proof)),
		Value::Array(items) => items.iter().find_map(|inner| find_proof(inner, proof)),
		Value::String(text) => serde_json::from_str::<Value>(text)
			.ok()
			.and_then(|parsed| find_proof(&parsed, proof)),
		_ => None,
	}
}

async fn call_tool(
	client: &EnvClient,
	extension: &PythonExtension,
	name: &str,
	id: &str,
) -> Result<Value> {
	let mut invocation = within(
		"open extension invocation",
		STEP_LIMIT,
		client.invoke(InvokeTool {
			invocation_id: id.to_owned(),
			name: name.to_owned(),
			rev: extension.revision(),
			..InvokeTool::default()
		}),
	)
	.await??;
	match within("extension acceptance", STEP_LIMIT, invocation.next_event()).await?? {
		Some(InvocationEvent::Accepted(_)) => {},
		other => return Err(error(format!("expected extension acceptance, got {other:?}"))),
	}
	within(
		"commit extension arguments",
		STEP_LIMIT,
		invocation.commit_args(
			Bytes::from(serde_json::to_vec(&json!({}))?),
			Bytes::from_static(b"p9-hook-token"),
			1000,
			None,
		),
	)
	.await??;
	let payload = loop {
		match within("extension verdict", STEP_LIMIT, invocation.next_event()).await?? {
			Some(InvocationEvent::Verdict(verdict)) => {
				let outcome: CallOutcome<Value, Value> = serde_json::from_slice(&verdict.json)?;
				match outcome {
					CallOutcome::Ok(value) => break value,
					other => return Err(error(format!("extension tool {name} returned {other:?}"))),
				}
			},
			Some(InvocationEvent::Update(_)) => {},
			Some(other) => return Err(error(format!("unexpected extension event {other:?}"))),
			None => return Err(error("extension invocation closed before its verdict")),
		}
	};
	find_proof(&payload, name)
		.with_context(|| format!("the extension tool {name} answered {payload} without its proof"))
}

#[tokio::test]
async fn a_tool_reaches_the_endpoint_and_the_same_request_from_a_hook_does_not() -> Result<()> {
	let scratch = Scratch::new()?;
	let (port, accepted) = endpoint().await?;
	std::fs::write(scratch.project().join("endpoint.txt"), format!("http://127.0.0.1:{port}/omp"))
		.context("writing the endpoint file")?;
	let extension = PythonExtension::new(MODULE, SOURCE, &["from_tool", "report"])
		.trusting()
		.with_data_grants(&["env.net"]);
	let harness = InProcessEnvHarness::spawn_with_extension(scratch, &extension).await?;

	// The host dispatches `extension_activate` when it activates the extension
	// and `agent_settled` when the agent loop asks whether to settle; both run
	// the extension's hook with the hook authority.
	let hooks = LifecycleHooks::new(harness.server().admission_gate());
	let settled = hooks.agent_settled(settled_payload()).await;
	assert_eq!(
		settled.decision,
		AgentSettled::Settle,
		"the agent_settled hook did not answer Settle"
	);
	// The agent loop publishes `turn_start` through the same gate once per turn;
	// the extension subscribed it when it registered, so it is delivered at once.
	assert!(
		hooks.subscribed(HookEventId::HookEventTurnStart),
		"the gate holds no turn_start subscription for the extension"
	);
	hooks
		.notify(HookEventId::HookEventTurnStart, turn_start_payload())
		.context("publishing turn_start")?;
	let mut report = call_tool(harness.client(), &extension, "report", "p9-hook-report-0").await?;
	for round in 1..40 {
		if ["activate", "settled", "turn_start"]
			.iter()
			.all(|hook| report["hook"][hook].is_object())
		{
			break;
		}
		tokio::time::sleep(Duration::from_millis(250)).await;
		report =
			call_tool(harness.client(), &extension, "report", &format!("p9-hook-report-{round}"))
				.await?;
	}
	eprintln!("hook record: {report}");
	assert!(
		report["hook"]["activate"].is_object(),
		"the extension_activate hook never ran: {report}"
	);
	assert!(report["hook"]["settled"].is_object(), "the agent_settled hook never ran: {report}");
	assert!(report["hook"]["turn_start"].is_object(), "the turn_start hook never ran: {report}");
	let before_tool = accepted.load(Ordering::SeqCst);

	let tool = call_tool(harness.client(), &extension, "from_tool", "p9-hook-from-tool").await?;
	eprintln!("tool record: {tool}");
	let after_tool = accepted.load(Ordering::SeqCst);

	// The control: the identical request from a tool that declares a fetch
	// effect passes the DATA gate and reaches the endpoint.
	assert_eq!(tool["attempt"]["outcome"], "sent", "the tool's request was refused: {tool}");
	assert_eq!(tool["attempt"]["status"], 200);
	assert_eq!(after_tool - before_tool, 1, "the tool's request must arrive exactly once");

	// Neither hook's request was sent: each was refused, and none reached the
	// endpoint.
	for hook in ["activate", "settled", "turn_start"] {
		assert_eq!(
			report["hook"][hook]["outcome"], "refused",
			"the {hook} hook's request was not refused: {report}"
		);
	}
	assert_eq!(before_tool, 0, "a hook's request reached the endpoint: {report}");
	harness.shutdown().await;
	Ok(())
}
