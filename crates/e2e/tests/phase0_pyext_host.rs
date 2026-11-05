//! Phase 0 feasibility instrument: can the embedded-Python extension host
//! carry an out-of-core capability? The extension is admitted through the
//! production local-extension discovery (`omp.toml`, sandboxed tier) and served
//! by the production environment host. Observations print with a `P0|` prefix:
//! `cargo nextest run -p omp-e2e --test phase0_pyext_host --no-capture`.

#![cfg(unix)]

use std::{
	fs, future,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, Instant},
};

use bytes::Bytes;
use omp_core::{EnvPath, Principal, Provenance, Str, sf};
use omp_driver::discovery::native::{
	NativeAdmissionOptions, NativeLoadMode, admit_native_extensions,
};
use omp_e2e::{
	Context as _, Result, error,
	support::{DEFAULT_TIMEOUT, omp_binary, within},
};
use omp_env::{Admitter, EnvClient, Invocation, InvocationEvent, ProcessAttachmentEvent};
use omp_envd::{
	EnvServer, ExtensionDataBinding, RegistryBridges,
	worker::{ExtHostConfig, HostKey},
};
use omp_proto::{
	SCHEMA_REV,
	env::v1::{
		Admission, AdmitInvocation, AttachOutput, ClientHello, InvokeTool, ProcessSpec, Script,
		StartProcess, StopProcess,
	},
};
use omp_tool::{CallOutcome, Registry};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

const EXTENSION_ID: &str = "p0_result_normalize";
const SESSION: &str = "p0-session";
const TOOLS: &[&str] = &[
	"p0_normalize",
	"p0_wait",
	"p0_wait_sync",
	"p0_net",
	"p0_proc_start",
	"p0_proc_collect",
	"p0_proc_stop",
];

const EXTENSION: &str = r#"
"""Phase 0 probe: register, payload, structured JSON, cancellation, reattach."""

import asyncio
import json
import os
import socket
import time

import omp

_DIAG = "__P0_DIAG__"
try:
    _diag = os.open(_DIAG, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    os.write(_diag, f"module imported pid={os.getpid()}\n".encode())
    os.dup2(_diag, 2)
except OSError:
    pass

_EXEC = omp.Effects(exec=omp.ExecEffects(commands=("/bin/sh",), network=False))


def _mark(path, event, **fields):
    record = {"event": event, "pid": os.getpid(), "monotonic": round(time.monotonic(), 3)}
    record.update(fields)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")


@omp.tool("p0_normalize", rev=1)
def p0_normalize(stdout: str, stderr: str, exit: int, format: str = "") -> dict:
    """Return one run's output as structured JSON."""
    text = stdout + stderr
    return {
        "exit": exit,
        "format": "text",
        "reason": "no format requested" if not format else f"format {format!r} is not registered",
        "failures": [],
        "summary": {
            "lines": len(text.splitlines()),
            "stdout_bytes": len(stdout.encode("utf-8")),
            "stderr_bytes": len(stderr.encode("utf-8")),
        },
        "artifact": None,
        "received": {"stdout": stdout, "stderr": stderr, "exit": exit, "format": format},
        "pid": os.getpid(),
    }


@omp.tool("p0_wait", rev=1)
async def p0_wait(marker: str, seconds: float, ctx: omp.Context) -> dict:
    """Wait cooperatively and record how the wait ended."""
    started = time.monotonic()
    remaining = None if ctx is None else ctx.deadline_in()
    _mark(marker, "started", context=ctx is not None, deadline_in=None if remaining is None else str(remaining))
    if ctx is not None:
        try:
            ctx.on_cancel(lambda: _mark(marker, "on_cancel", cancelled=ctx.cancelled()))
        except BaseException as error:
            _mark(marker, "on_cancel_unavailable", error=repr(error))
        try:
            ctx.update({"phase": "started", "pid": os.getpid()})
        except BaseException as error:
            _mark(marker, "update_unavailable", error=repr(error))
    try:
        await asyncio.sleep(seconds)
    except asyncio.CancelledError:
        _mark(
            marker,
            "cancelled_error",
            elapsed=round(time.monotonic() - started, 3),
            cancelled=None if ctx is None else ctx.cancelled(),
        )
        raise
    except BaseException as error:
        _mark(marker, "base_exception", kind=type(error).__name__, elapsed=round(time.monotonic() - started, 3))
        raise
    _mark(marker, "completed", elapsed=round(time.monotonic() - started, 3))
    return {"slept": seconds, "pid": os.getpid()}


@omp.tool("p0_wait_sync", rev=1)
def p0_wait_sync(marker: str, seconds: float) -> dict:
    """Block a worker thread in Python code and record how it ended."""
    started = time.monotonic()
    _mark(marker, "started")
    try:
        while time.monotonic() - started < seconds:
            time.sleep(0.02)
    except BaseException as error:
        _mark(marker, "base_exception", kind=type(error).__name__, elapsed=round(time.monotonic() - started, 3))
        raise
    _mark(marker, "completed", elapsed=round(time.monotonic() - started, 3))
    return {"slept": seconds, "pid": os.getpid()}


@omp.tool("p0_net", rev=1)
def p0_net(host: str, port: int) -> dict:
    """Report whether this host process can open an outbound TCP connection."""
    try:
        with socket.create_connection((host, port), timeout=3):
            return {"connected": True, "error": None}
    except OSError as error:
        return {"connected": False, "error": f"{type(error).__name__}: {error}"}


@omp.tool("p0_proc_start", rev=1, effects=_EXEC)
async def p0_proc_start(name: str, script: str) -> dict:
    """Start an Environment-supervised named process and return its handle."""
    process = await omp.env.proc.start(name, script)
    return {"name": process.name, "generation": process.generation}


@omp.tool("p0_proc_collect", rev=1, effects=_EXEC)
async def p0_proc_collect(name: str, seconds: float) -> dict:
    """Adopt a named process by name, wait for it to end, and return its output."""
    process = await omp.env.proc.adopt(name)
    if process is None:
        return {"adopted": False}
    deadline = time.monotonic() + seconds
    info = await process.info()
    while str(info.state).lower() not in ("exited", "stopped", "failed") and time.monotonic() < deadline:
        await asyncio.sleep(0.1)
        info = await process.info()
    chunks = []
    stream = process.output(after=0).__aiter__()
    while True:
        try:
            item = await asyncio.wait_for(stream.__anext__(), timeout=0.5)
        except (asyncio.TimeoutError, StopAsyncIteration):
            break
        chunks.append(bytes(item.data).decode("utf-8", "replace"))
    return {
        "adopted": True,
        "generation": process.generation,
        "state": str(info.state),
        "status": repr(info.status),
        "output": "".join(chunks),
    }


@omp.tool("p0_proc_stop", rev=1, effects=_EXEC)
async def p0_proc_stop(name: str) -> dict:
    """Stop a named process adopted by name."""
    process = await omp.env.proc.adopt(name)
    if process is None:
        return {"adopted": False}
    info = await process.stop()
    return {"adopted": True, "state": str(info.state), "status": repr(info.status)}
"#;

fn manifest() -> String {
	let mut text = format!(
		"id = \"{EXTENSION_ID}\"\nentry = \"{EXTENSION_ID}\"\ncapabilities = [\"invocation\", \
		 \"env.process\"]\n"
	);
	for tool in TOOLS {
		text.push_str(&format!(
			"\n[[declarations]]\nid = \"{tool}@{EXTENSION_ID}.1\"\nkind = \"soft\"\nmodule = \
			 \"{EXTENSION_ID}\"\ntrigger = \"lazy\"\nkey = \"{tool}{}\"\napi = 1\nfailure = \
			 \"fault\"\n{}",
			if std::env::var("P0_KEY_FORM").as_deref() == Ok("name") {
				String::new()
			} else {
				format!("@{EXTENSION_ID}.1")
			},
			std::env::var("P0_MANIFEST_EXTRA").unwrap_or_default()
		));
	}
	text
}

struct AllowAdmission;

impl Admitter for AllowAdmission {
	type Future<'client> = future::Ready<Admission>;

	fn admit<'client>(&'client self, query: AdmitInvocation) -> Self::Future<'client> {
		future::ready(Admission {
			invocation_id: query.invocation_id,
			allow: true,
			..Admission::default()
		})
	}
}

#[derive(Clone, Copy, Debug)]
enum Route {
	NativeManifest,
	TrustedModule,
}

struct Host {
	label:     &'static str,
	rev:       String,
	server:    Arc<EnvServer>,
	data_task: JoinHandle<()>,
	shutdown:  CancellationToken,
	project:   PathBuf,
	_tree:     tempfile::TempDir,
}

struct Conn {
	client: EnvClient,
	rev:    String,
	serve:  JoinHandle<()>,
}

impl Host {
	async fn open(label: &'static str) -> Result<Self> {
		Self::open_with(label, Route::TrustedModule).await
	}

	async fn open_with(label: &'static str, route: Route) -> Result<Self> {
		omp_observability::logging::init();
		let tree = tempfile::tempdir().context("phase 0 tree")?;
		let project = tree.path().join("project");
		let ext = tree.path().join("ext");
		let home = tree.path().join("home");
		let state = tree.path().join("state");
		for dir in [&project, &ext.join("src"), &home, &state] {
			fs::create_dir_all(dir).context("phase 0 directory")?;
		}
		fs::write(ext.join("omp.toml"), manifest()).context("write omp.toml")?;
		let diagnostics = project.join("host-stderr.log");
		fs::write(
			ext.join("src").join(format!("{EXTENSION_ID}.py")),
			EXTENSION.replace("__P0_DIAG__", &diagnostics.to_string_lossy()),
		)
		.context("write extension module")?;
		let module = ext.join("src").join(format!("{EXTENSION_ID}.py"));
		let mut spec = match route {
			Route::NativeManifest => {
				let admitted = admit_native_extensions(&project, &home, NativeAdmissionOptions {
					explicit_roots:    &[ext.clone()],
					mode:              NativeLoadMode::ExplicitOnly,
					include_workspace: false,
					setting_overrides: &[],
					disabled:          &[],
				})
				.map_err(|source| {
					error(format!("production admission refused the extension: {source}"))
				})?;
				let [admitted] = admitted.as_slice() else {
					return Err(error(format!(
						"expected one admitted extension, got {}",
						admitted.len()
					)));
				};
				admitted.spec.clone()
			},
			Route::TrustedModule => omp_app::cli::trusted_extension(
				omp_envd::site::validate_trusted_module(&module)
					.map_err(|source| error(format!("trusted module refused: {source}")))?,
			),
		};
		if std::env::var_os("P0_GRANT_PROCESS").is_some() {
			spec.data_grants = omp_envd::policy::Grants::supported(["env.process"]);
		}
		let rev = format!("{}.1", spec.key.extension());
		eprintln!(
			"P0|{label}|admission|route={route:?} key={:?} rev={rev} grants={:?} declarations={:?}",
			spec.key,
			spec.data_grants,
			spec
				.manifest
				.static_declarations()
				.tools
				.iter()
				.map(|row| row.id.as_str())
				.collect::<Vec<_>>(),
		);
		let executable = match std::env::var_os("P0_HOST_WRAPPER") {
			Some(wrapper) => PathBuf::from(wrapper),
			None => omp_binary().context("resolve extension-host executable")?,
		};
		let mut config =
			ExtHostConfig::new(executable, Principal::new(sf!("p0"), sf!("Phase 0")), sf!(SESSION), 1);
		let mut binding = ExtensionDataBinding::scoped(
			&state,
			spec.key.clone(),
			SESSION,
			1,
			spec.data_grants.clone(),
		);
		binding
			.prepare_endpoint()
			.context("prepare scoped extension DATA socket")?;
		spec.data_socket = Some(binding.path().to_owned());
		config.extensions.push(spec);
		let con = Arc::new(omp_con::Ctx::new());
		let convars = Arc::new(omp_envd::exthost::ConvarControlFactory::new(Arc::clone(&con)));
		let opened = Instant::now();
		let server = Arc::new(
			EnvServer::open_local(
				&project,
				&state,
				Registry::new(),
				config,
				&con,
				convars,
				RegistryBridges::default(),
			)
			.await
			.context("open environment host")?,
		);
		for line in fs::read_to_string(&diagnostics).unwrap_or_default().lines() {
			eprintln!("P0|{label}|host_stderr|{line}");
		}
		let identities = server.extension_control_identities();
		eprintln!(
			"P0|{label}|control_children|count={} open_ms={}",
			identities.len(),
			opened.elapsed().as_millis()
		);
		for identity in &identities {
			let evidence = server
				.extension_registry_evidence(identity)
				.context("CONTROL child omitted frozen registry evidence")?;
			for tool in evidence.tools.iter() {
				eprintln!(
					"P0|{label}|registered|name={} rev={} extension={} kind={} place={} mode={}",
					tool
						.definition
						.as_ref()
						.map_or("", |definition| definition.name.as_str()),
					tool.rev,
					tool.extension_id,
					tool.kind,
					tool.place,
					tool.execution_mode,
				);
			}
		}
		let shutdown = CancellationToken::new();
		let data_server = Arc::clone(&server);
		let data_shutdown = shutdown.clone();
		let data_task = tokio::spawn(async move {
			let _ = data_server
				.serve_extension_uds(binding, data_shutdown)
				.await;
		});
		Ok(Self { label, rev, server, data_task, shutdown, project, _tree: tree })
	}

	async fn connect(&self, name: &str) -> Result<Conn> {
		let (client, transport) = EnvClient::in_process(64);
		client.set_admitter(AllowAdmission);
		let server = Arc::clone(&self.server);
		let serve = tokio::spawn(async move { server.serve_in_process(transport).await });
		within(
			"environment hello",
			DEFAULT_TIMEOUT,
			client.hello(ClientHello {
				client: name.to_owned(),
				schema_rev: SCHEMA_REV,
				..ClientHello::default()
			}),
		)
		.await??;
		Ok(Conn { client, rev: self.rev.clone(), serve })
	}

	fn marker(&self, name: &str) -> PathBuf {
		self.project.join(format!("{name}.jsonl"))
	}

	fn env_path(&self) -> Result<EnvPath> {
		let uri = Url::from_directory_path(&self.project)
			.map_err(|()| error(format!("project path is not absolute")))?;
		EnvPath::new(uri.as_str()).map_err(|source| error(format!("typed cwd: {source:?}")))
	}

	fn close(self) {
		self.shutdown.cancel();
		self.data_task.abort();
	}
}

impl Conn {
	async fn sever(self, label: &str) {
		let Self { client, mut serve, .. } = self;
		drop(client);
		let ended = tokio::time::timeout(Duration::from_secs(5), &mut serve)
			.await
			.is_ok();
		if !ended {
			serve.abort();
		}
		eprintln!("P0|{label}|sever|server_connection_ended_on_eof={ended}");
	}
}

async fn open_call(
	conn: &Conn,
	id: &str,
	tool: &str,
	args: &Value,
	deadline_ms: u64,
) -> Result<Invocation> {
	let mut invocation = within(
		"open invocation",
		DEFAULT_TIMEOUT,
		conn.client.invoke(InvokeTool {
			invocation_id: id.to_owned(),
			name: tool.to_owned(),
			rev: conn.rev.clone(),
			deadline_ms,
			..InvokeTool::default()
		}),
	)
	.await??;
	match within("invocation acceptance", DEFAULT_TIMEOUT, invocation.next_event()).await?? {
		Some(InvocationEvent::Accepted(_)) => {},
		other => return Err(error(format!("{tool}: expected acceptance, got {other:?}"))),
	}
	within(
		"commit arguments",
		DEFAULT_TIMEOUT,
		invocation.commit_args(
			Bytes::from(serde_json::to_vec(args)?),
			Bytes::from_static(b"p0-effect-token"),
			1000,
			None,
		),
	)
	.await??;
	Ok(invocation)
}

enum Next {
	Update(Value),
	Verdict { is_error: bool, outcome: Value },
	Closed,
}

async fn next(invocation: &mut Invocation, limit: Duration) -> Result<Next> {
	match within("invocation event", limit, invocation.next_event()).await?? {
		Some(InvocationEvent::Update(update)) => {
			Ok(Next::Update(serde_json::from_slice(&update.json).unwrap_or(Value::Null)))
		},
		Some(InvocationEvent::Verdict(verdict)) => Ok(Next::Verdict {
			is_error: verdict.is_error,
			outcome:  serde_json::from_slice(&verdict.json)
				.unwrap_or_else(|_| json!({ "undecodable": String::from_utf8_lossy(&verdict.json) })),
		}),
		Some(other) => Err(error(format!("unexpected invocation event {other:?}"))),
		None => Ok(Next::Closed),
	}
}

async fn call(host: &Host, conn: &Conn, id: &str, tool: &str, args: &Value) -> Result<Value> {
	let started = Instant::now();
	let mut invocation = open_call(conn, id, tool, args, 0).await?;
	loop {
		match next(&mut invocation, DEFAULT_TIMEOUT).await? {
			Next::Update(update) => eprintln!("P0|{}|{id}|update|{update}", host.label),
			Next::Verdict { is_error, outcome } => {
				eprintln!(
					"P0|{}|{id}|verdict|is_error={is_error} elapsed_ms={} outcome={outcome}",
					host.label,
					started.elapsed().as_millis()
				);
				return Ok(outcome);
			},
			Next::Closed => return Err(error(format!("{id}: stream closed before verdict"))),
		}
	}
}

fn marker_events(path: &Path) -> Vec<Value> {
	fs::read_to_string(path)
		.unwrap_or_default()
		.lines()
		.filter_map(|line| serde_json::from_str(line).ok())
		.collect()
}

fn has_event(path: &Path, event: &str) -> bool {
	marker_events(path)
		.iter()
		.any(|record| record["event"] == event)
}

async fn wait_event(path: &Path, event: &str, limit: Duration) -> bool {
	let until = Instant::now() + limit;
	while Instant::now() < until {
		if has_event(path, event) {
			return true;
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	has_event(path, event)
}

fn print_markers(label: &str, name: &str, path: &Path) {
	for record in marker_events(path) {
		eprintln!("P0|{label}|{name}|extension_marker|{record}");
	}
}

async fn await_started(host: &Host, marker: &Path) -> Result<u64> {
	if !wait_event(marker, "started", DEFAULT_TIMEOUT).await {
		return Err(error(format!("{}: body never started", host.label)));
	}
	Ok(marker_events(marker)
		.iter()
		.find(|record| record["event"] == "started")
		.and_then(|record| record["pid"].as_u64())
		.unwrap_or_default())
}

async fn host_pid(host: &Host, conn: &Conn, id: &str) -> Result<u64> {
	let outcome =
		call(host, conn, id, "p0_normalize", &json!({"stdout": "", "stderr": "", "exit": 0})).await?;
	Ok(outcome["value"]["pid"].as_u64().unwrap_or_default())
}

async fn terminal_after(
	label: &str,
	name: &str,
	invocation: &mut Invocation,
	since: Instant,
	limit: Duration,
) -> Result<()> {
	loop {
		match next(invocation, limit).await {
			Ok(Next::Update(update)) => eprintln!("P0|{label}|{name}|update|{update}"),
			Ok(Next::Verdict { is_error, outcome }) => {
				eprintln!(
					"P0|{label}|{name}|terminal|is_error={is_error} ms_after_stop={} outcome={outcome}",
					since.elapsed().as_millis()
				);
				return Ok(());
			},
			Ok(Next::Closed) => {
				eprintln!(
					"P0|{label}|{name}|terminal|stream_closed ms_after_stop={}",
					since.elapsed().as_millis()
				);
				return Ok(());
			},
			Err(source) => {
				eprintln!(
					"P0|{label}|{name}|terminal|none_within_ms={} error={source}",
					limit.as_millis()
				);
				return Ok(());
			},
		}
	}
}

#[tokio::test]
async fn p0_capability_1_register_2_receive_3_return() -> Result<()> {
	let host = Host::open("c123").await?;
	let conn = host.connect("p0-c123").await?;
	let args = json!({
		"stdout": "ok 1 - parses\nnot ok 2 - ünïcode ✓\n",
		"stderr": "warning: deprecated\n",
		"exit": 2,
		"format": "tap",
	});
	let outcome = call(&host, &conn, "p0-normalize", "p0_normalize", &args).await?;
	let decoded: CallOutcome<Value, Value> = serde_json::from_value(outcome)?;
	let CallOutcome::Ok(payload) = decoded else {
		return Err(error(format!("normalize did not succeed: {decoded:?}")));
	};
	assert_eq!(payload["received"], args, "payload must round-trip exactly");
	assert_eq!(payload["exit"], 2);
	assert_eq!(payload["failures"], json!([]));
	assert_eq!(payload["artifact"], Value::Null);
	assert_eq!(payload["summary"]["stdout_bytes"], 39);
	let pid = payload["pid"].as_u64().context("payload pid")?;
	assert_ne!(pid, u64::from(std::process::id()), "body must run in the extension host");
	eprintln!("P0|c123|pids|test={} extension_host={pid}", std::process::id());
	let net =
		call(&host, &conn, "p0-net", "p0_net", &json!({"host": "1.1.1.1", "port": 443})).await?;
	eprintln!("P0|c123|sandboxed_network|{net}");
	conn.sever("c123").await;
	host.close();
	Ok(())
}

#[tokio::test]
async fn p0_capability_1_register_via_native_manifest() -> Result<()> {
	let host = Host::open_with("c1n", Route::NativeManifest).await?;
	let conn = host.connect("p0-c1n").await?;
	let attempt = open_call(
		&conn,
		"p0-native",
		"p0_normalize",
		&json!({"stdout": "", "stderr": "", "exit": 0}),
		0,
	)
	.await;
	eprintln!(
		"P0|c1n|invoke_after_native_admission|{}",
		attempt
			.as_ref()
			.map_or_else(|source| format!("refused: {source}"), |_| String::from("opened"))
	);
	conn.sever("c1n").await;
	host.close();
	Ok(())
}

async fn cancel_case(label: &'static str, tool: &str, stop: &str) -> Result<()> {
	let host = Host::open(label).await?;
	let conn = host.connect(label).await?;
	let marker = host.marker(label);
	let pid_before = host_pid(&host, &conn, "pid-before").await?;
	let args = json!({"marker": marker.to_string_lossy(), "seconds": 30});
	let mut invocation = open_call(&conn, "long-call", tool, &args, 0).await?;
	let body_pid = await_started(&host, &marker).await?;
	tokio::time::sleep(Duration::from_millis(200)).await;
	let since = Instant::now();
	let mut retained = None;
	match stop {
		"cancel_frame" => {
			invocation.guard().cancel();
			retained = Some(invocation);
		},
		"interrupt" => {
			within("interrupt", DEFAULT_TIMEOUT, invocation.interrupt(sf!("phase 0 stop request")))
				.await??;
			retained = Some(invocation);
		},
		"drop_handle" => drop(invocation),
		other => return Err(error(format!("unknown stop {other}"))),
	}
	eprintln!("P0|{label}|stop|kind={stop} tool={tool}");
	if let Some(mut invocation) = retained {
		terminal_after(label, "long-call", &mut invocation, since, Duration::from_secs(10)).await?;
	}
	tokio::time::sleep(Duration::from_secs(3)).await;
	print_markers(label, "long-call", &marker);
	let observed = ["cancelled_error", "on_cancel", "base_exception"]
		.iter()
		.filter(|event| has_event(&marker, event))
		.copied()
		.collect::<Vec<_>>();
	let completed = has_event(&marker, "completed");
	let pid_after = host_pid(&host, &conn, "pid-after").await?;
	eprintln!(
		"P0|{label}|summary|stop={stop} tool={tool} observed_in_extension={observed:?} \
		 completed={completed} host_pid_before={pid_before} body_pid={body_pid} \
		 host_pid_after={pid_after} host_restarted={}",
		pid_before != pid_after
	);
	conn.sever(label).await;
	host.close();
	Ok(())
}

#[tokio::test]
async fn p0_capability_4a_cancel_frame_async_body() -> Result<()> {
	cancel_case("c4a", "p0_wait", "cancel_frame").await
}

#[tokio::test]
async fn p0_capability_4b_interrupt_async_body() -> Result<()> {
	cancel_case("c4b", "p0_wait", "interrupt").await
}

#[tokio::test]
async fn p0_capability_4c_drop_handle_async_body() -> Result<()> {
	cancel_case("c4c", "p0_wait", "drop_handle").await
}

#[tokio::test]
async fn p0_capability_4d_cancel_frame_sync_body() -> Result<()> {
	cancel_case("c4d", "p0_wait_sync", "cancel_frame").await
}

#[tokio::test]
async fn p0_capability_4f_sync_cancel_blast_radius() -> Result<()> {
	let label = "c4f";
	let host = Host::open(label).await?;
	let conn = host.connect(label).await?;
	let sync_marker = host.marker("c4f-sync");
	let sibling_marker = host.marker("c4f-sibling");
	let mut sibling = open_call(
		&conn,
		"sibling",
		"p0_wait",
		&json!({"marker": sibling_marker.to_string_lossy(), "seconds": 5}),
		0,
	)
	.await?;
	await_started(&host, &sibling_marker).await?;
	let blocking = open_call(
		&conn,
		"blocking",
		"p0_wait_sync",
		&json!({"marker": sync_marker.to_string_lossy(), "seconds": 30}),
		0,
	)
	.await?;
	let pid_before = await_started(&host, &sync_marker).await?;
	tokio::time::sleep(Duration::from_millis(200)).await;
	let since = Instant::now();
	blocking.guard().cancel();
	let mut blocking = blocking;
	terminal_after(label, "blocking", &mut blocking, since, Duration::from_secs(10)).await?;
	terminal_after(label, "sibling", &mut sibling, since, Duration::from_secs(10)).await?;
	tokio::time::sleep(Duration::from_secs(6)).await;
	print_markers(label, "blocking", &sync_marker);
	print_markers(label, "sibling", &sibling_marker);
	let pid_after = host_pid(&host, &conn, "pid-after").await?;
	eprintln!(
		"P0|{label}|summary|host_pid_before={pid_before} host_pid_after={pid_after} \
		 sibling_completed={} sibling_cancel_observed={}",
		has_event(&sibling_marker, "completed"),
		has_event(&sibling_marker, "cancelled_error"),
	);
	drop(blocking);
	drop(sibling);
	conn.sever(label).await;
	host.close();
	Ok(())
}

#[tokio::test]
async fn p0_capability_4e_deadline_expiry() -> Result<()> {
	let label = "c4e";
	let host = Host::open(label).await?;
	let conn = host.connect(label).await?;
	let marker = host.marker(label);
	let args = json!({"marker": marker.to_string_lossy(), "seconds": 4});
	let since = Instant::now();
	let mut invocation = open_call(&conn, "deadline-call", "p0_wait", &args, 1500).await?;
	terminal_after(label, "deadline-call", &mut invocation, since, Duration::from_secs(10)).await?;
	tokio::time::sleep(Duration::from_secs(5)).await;
	print_markers(label, "deadline-call", &marker);
	eprintln!(
		"P0|{label}|summary|deadline_ms=1500 body_seconds=4 cancelled_error={} on_cancel={} \
		 completed={}",
		has_event(&marker, "cancelled_error"),
		has_event(&marker, "on_cancel"),
		has_event(&marker, "completed"),
	);
	conn.sever(label).await;
	host.close();
	Ok(())
}

#[tokio::test]
async fn p0_capability_5a_reattach_after_connection_close() -> Result<()> {
	let label = "c5a";
	let host = Host::open(label).await?;
	let marker = host.marker(label);
	let args = json!({"marker": marker.to_string_lossy(), "seconds": 3});
	let first = host.connect("c5a-first").await?;
	let invocation = open_call(&first, "survivor", "p0_wait", &args, 0).await?;
	await_started(&host, &marker).await?;
	drop(invocation);
	first.sever(label).await;
	tokio::time::sleep(Duration::from_secs(4)).await;
	print_markers(label, "survivor", &marker);
	eprintln!(
		"P0|{label}|after_close|cancelled_error={} completed={}",
		has_event(&marker, "cancelled_error"),
		has_event(&marker, "completed"),
	);
	let second = host.connect("c5a-second").await?;
	let since = Instant::now();
	let mut again = open_call(&second, "survivor", "p0_wait", &args, 0).await?;
	terminal_after(label, "survivor-reinvoked", &mut again, since, Duration::from_secs(10)).await?;
	let starts = marker_events(&marker)
		.iter()
		.filter(|record| record["event"] == "started")
		.count();
	eprintln!("P0|{label}|reinvoke_same_id|body_starts_total={starts}");
	second.sever(label).await;
	host.close();
	Ok(())
}

#[tokio::test]
async fn p0_capability_5b_relinquished_call() -> Result<()> {
	let label = "c5b";
	let host = Host::open(label).await?;
	let marker = host.marker(label);
	let args = json!({"marker": marker.to_string_lossy(), "seconds": 2});
	let conn = host.connect("c5b-owner").await?;
	let invocation = open_call(&conn, "relinquished", "p0_wait", &args, 0).await?;
	await_started(&host, &marker).await?;
	let stream = invocation.relinquish();
	drop(stream);
	tokio::time::sleep(Duration::from_secs(3)).await;
	print_markers(label, "relinquished", &marker);
	eprintln!(
		"P0|{label}|relinquish_then_drop_stream|cancelled_error={} completed={}",
		has_event(&marker, "cancelled_error"),
		has_event(&marker, "completed"),
	);
	let closing = host.marker("c5b-close");
	let close_args = json!({"marker": closing.to_string_lossy(), "seconds": 2});
	let invocation = open_call(&conn, "relinquished-close", "p0_wait", &close_args, 0).await?;
	await_started(&host, &closing).await?;
	let stream = invocation.relinquish();
	conn.sever(label).await;
	drop(stream);
	tokio::time::sleep(Duration::from_secs(3)).await;
	print_markers(label, "relinquished-close", &closing);
	eprintln!(
		"P0|{label}|relinquish_then_close_connection|cancelled_error={} completed={}",
		has_event(&closing, "cancelled_error"),
		has_event(&closing, "completed"),
	);
	host.close();
	Ok(())
}

fn ticking_script(marker: &Path, ticks: u32) -> String {
	let marker = marker.to_string_lossy();
	format!(
		"/bin/sh -c 'trap \"echo term-observed >> {marker}; exit 143\" TERM; echo started >> \
		 {marker}; i=0; while [ $i -lt {ticks} ]; do echo tick-$i; /bin/sleep 0.1; i=$((i+1)); \
		 done; echo finished >> {marker}'"
	)
}

#[tokio::test]
async fn p0_subprocess_6a_env_named_process_reattach() -> Result<()> {
	let label = "s6a";
	let host = Host::open(label).await?;
	let marker = host.project.join("s6a-process.txt");
	let first = host.connect("s6a-first").await?;
	let started = within(
		"start named process",
		DEFAULT_TIMEOUT,
		first.client.start_process(&host.env_path()?, StartProcess {
			name: "p0-sub".into(),
			spec: Some(ProcessSpec {
				source: Some(Script { text: ticking_script(&marker, 20), ..Default::default() }),
				..Default::default()
			}),
			..Default::default()
		}),
	)
	.await??;
	eprintln!("P0|{label}|started|{started:?}");
	first.sever(label).await;
	let second = host.connect("s6a-second").await?;
	let mut attachment = within(
		"attach output",
		DEFAULT_TIMEOUT,
		second.client.attach_output(AttachOutput {
			name: "p0-sub".into(),
			generation: 1,
			..Default::default()
		}),
	)
	.await??;
	let mut output = String::new();
	let mut status = None;
	let until = Instant::now() + Duration::from_secs(15);
	while Instant::now() < until && status.is_none() {
		match tokio::time::timeout(Duration::from_secs(5), attachment.next_event()).await {
			Ok(Ok(Some(ProcessAttachmentEvent::Attached(attached)))) => {
				eprintln!("P0|{label}|attached|{attached:?}");
			},
			Ok(Ok(Some(ProcessAttachmentEvent::Output(chunk)))) => {
				output.push_str(&String::from_utf8_lossy(&chunk.data));
			},
			Ok(Ok(Some(ProcessAttachmentEvent::State(state)))) => {
				if let Some(process) = state.process.as_ref()
					&& process.status.is_some()
				{
					status = Some(format!("{:?}", process.status));
				}
			},
			Ok(Ok(None)) => break,
			Ok(Err(source)) => {
				eprintln!("P0|{label}|attach_error|{source}");
				break;
			},
			Err(_) => break,
		}
	}
	eprintln!(
		"P0|{label}|reattached|ticks={} status={status:?} marker={:?}",
		output.matches("tick-").count(),
		fs::read_to_string(&marker).unwrap_or_default()
	);
	let stop_marker = host.project.join("s6a-stop.txt");
	within(
		"start long named process",
		DEFAULT_TIMEOUT,
		second
			.client
			.start_process(&host.env_path()?, StartProcess {
				name: "p0-sub-long".into(),
				spec: Some(ProcessSpec {
					source: Some(Script {
						text: ticking_script(&stop_marker, 300),
						..Default::default()
					}),
					..Default::default()
				}),
				..Default::default()
			}),
	)
	.await??;
	tokio::time::sleep(Duration::from_millis(700)).await;
	let since = Instant::now();
	let stopped = within(
		"stop long named process",
		DEFAULT_TIMEOUT,
		second.client.stop_process(StopProcess {
			name: "p0-sub-long".into(),
			grace_ms: 2000,
			generation: 1,
			..Default::default()
		}),
	)
	.await??;
	tokio::time::sleep(Duration::from_millis(800)).await;
	eprintln!(
		"P0|{label}|stop|accepted={stopped:?} ms={} marker={:?}",
		since.elapsed().as_millis(),
		fs::read_to_string(&stop_marker).unwrap_or_default()
	);
	second.sever(label).await;
	host.close();
	Ok(())
}

#[tokio::test]
async fn p0_subprocess_6b_extension_delegates_to_named_process() -> Result<()> {
	let label = "s6b";
	let host = Host::open(label).await?;
	let marker = host.project.join("s6b-process.txt");
	let first = host.connect("s6b-first").await?;
	call(
		&host,
		&first,
		"start",
		"p0_proc_start",
		&json!({
			"name": "p0-delegate",
			"script": ticking_script(&marker, 20),
		}),
	)
	.await?;
	first.sever(label).await;
	let second = host.connect("s6b-second").await?;
	call(
		&host,
		&second,
		"collect",
		"p0_proc_collect",
		&json!({
			"name": "p0-delegate",
			"seconds": 8,
		}),
	)
	.await?;
	eprintln!("P0|{label}|reattach_marker|{:?}", fs::read_to_string(&marker).unwrap_or_default());
	let stop_marker = host.project.join("s6b-stop.txt");
	call(
		&host,
		&second,
		"start-long",
		"p0_proc_start",
		&json!({
			"name": "p0-delegate-long",
			"script": ticking_script(&stop_marker, 300),
		}),
	)
	.await?;
	tokio::time::sleep(Duration::from_millis(500)).await;
	call(&host, &second, "stop-long", "p0_proc_stop", &json!({"name": "p0-delegate-long"})).await?;
	tokio::time::sleep(Duration::from_millis(500)).await;
	eprintln!("P0|{label}|cancel_marker|{:?}", fs::read_to_string(&stop_marker).unwrap_or_default());
	second.sever(label).await;
	host.close();
	Ok(())
}
