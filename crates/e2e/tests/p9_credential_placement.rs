//! P9: an operator-granted user extension reaches an authenticated REST
//! endpoint without ever holding the key (ADR 0075 S4).
//!
//! The proof runs the production path end to end. A credential grant is
//! written into the operator's `grants.toml`, the driver's native admission
//! turns it into the extension's `credential_grants`, an in-process
//! environment authority spawns the extension host, and the extension calls
//! `omp.env.http_post(url, credential="buildbuddy")` against a loopback TLS
//! fake endpoint. The endpoint sees the catalog's header with the key; nothing
//! the extension can read, in its return value, its response or its
//! environment, holds the key.
//!
//! A user extension's host is a `sandboxed`-tier host launched under the OS
//! sandbox with an exact Unix-socket grant that only Seatbelt enforces, so the
//! proof skips off macOS and, as in P9's other proofs, fails on macOS when
//! Seatbelt is unavailable.
#![cfg(unix)]

use std::{
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
	},
	time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use omp_ai::{
	auth::{CredentialError, CredentialLease, LeaseMeta},
	id::{AccountId, PrincipalId},
};
use omp_catalog::{AuthSpecId, ProviderId};
use omp_core::{SecretString, sf};
use omp_e2e::{
	Context as _, Result, error,
	support::{InProcessEnvHarness, OperatorHome, Scratch, within},
};
use omp_env::{EnvClient, InvocationEvent};
use omp_envd::placement::{JsonlPlacementAudit, PlacementBinding, PlacementCredentials};
use omp_ext::{
	Layer,
	credential_grant::{CredentialGrant, CredentialOperation, CredentialOrigin},
	trust::GrantsFile,
};
use omp_proto::env::v1::InvokeTool;
use omp_tool::CallOutcome;
use serde_json::{Value, json};
use tokio::{
	io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
	net::TcpListener,
};
use tokio_rustls::{TlsAcceptor, rustls};

const EXTENSION: &str = "placeproof";
const KEY: &str = "bb-e2e-key-0123456789abcdef";
const STEP_LIMIT: Duration = Duration::from_secs(30);

const SOURCE: &str = r#"
import os

import omp

URL = "__URL__"
KEY = "__KEY__"


@omp.tool("place", rev=1)
async def place():
    try:
        response = await omp.env.http_post(
            URL, body=b"payload", credential="buildbuddy", redirects=0
        )
    except omp.env.PlacementUnavailable as refused:
        return {"parts": [], "details": {"outcome": "refused", "reason": str(refused)}}
    return {
        "parts": [],
        "details": {
            "outcome": "sent",
            "status": response.status,
            "body": response.body.decode(),
            "key_in_headers": KEY in repr(dict(response.headers)),
            "key_in_environment": any(KEY in value for value in os.environ.values()),
        },
    }
"#;

fn manifest() -> String {
	format!(
		r#"id = "{EXTENSION}"
version = "0.1.0"
omp_api = 1
entry = "{EXTENSION}"
capabilities = ["env.net"]

[[declarations]]
id = "place"
kind = "soft"
module = "{EXTENSION}"
key = "place@{EXTENSION}.1"
trigger = "lazy"
api = 1
failure = "fault"
"#
	)
}

/// Whether this host can run a user extension's sandboxed host: only Seatbelt
/// enforces its exact Unix-socket grant, so other hosts skip and an
/// unavailable Seatbelt on macOS is an error.
fn runs_user_extension_hosts() -> Result<bool> {
	if !cfg!(target_os = "macos") {
		eprintln!(
			"P9 credential placement skipped: a user extension's host needs the Seatbelt sandbox \
			 backend, which only macOS has"
		);
		return Ok(false);
	}
	if let Some(failure) = omp_sandbox::backend_status(omp_sandbox::Backend::Seatbelt).failure() {
		return Err(error(format!(
			"P9 credential placement needs the Seatbelt sandbox backend, and its probe failed: \
			 {failure} ({failure:?})"
		)));
	}
	Ok(true)
}

/// The loopback TLS seam: a self-signed certificate for `127.0.0.1`, trusted
/// by the client the host places requests with and used by the fake endpoint.
struct Tls {
	acceptor: TlsAcceptor,
	client:   omp_http::Client,
}

fn tls() -> Result<Tls> {
	omp_http::install_tls_provider();
	let rcgen::CertifiedKey { cert, key_pair } =
		rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
			.map_err(|source| error(format!("certificate: {source}")))?;
	let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der());
	let config = rustls::ServerConfig::builder()
		.with_no_client_auth()
		.with_single_cert(vec![cert.der().clone()], key.into())
		.map_err(|source| error(format!("server config: {source}")))?;
	let root = reqwest::Certificate::from_pem(cert.pem().as_bytes())
		.map_err(|source| error(format!("root certificate: {source}")))?;
	let client = omp_http::client_builder()
		.redirect(reqwest::redirect::Policy::none())
		.no_proxy()
		.add_root_certificate(root)
		.build()
		.map_err(|source| error(format!("client: {source}")))?;
	Ok(Tls { acceptor: TlsAcceptor::from(Arc::new(config)), client: client.into() })
}

struct Endpoint {
	port:     u16,
	accepted: Arc<AtomicUsize>,
	requests: Arc<Mutex<Vec<String>>>,
}

async fn read_request(stream: &mut (impl AsyncRead + Unpin)) -> String {
	let mut data = Vec::new();
	let mut chunk = [0_u8; 1024];
	loop {
		let read = stream.read(&mut chunk).await.unwrap_or(0);
		if read == 0 {
			break;
		}
		data.extend_from_slice(&chunk[..read]);
		if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
			let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
			let want = head
				.lines()
				.find_map(|line| {
					line
						.strip_prefix("content-length: ")?
						.trim()
						.parse::<usize>()
						.ok()
				})
				.unwrap_or(0);
			if data.len() >= end + 4 + want {
				break;
			}
		}
	}
	String::from_utf8_lossy(&data).into_owned()
}

/// A fake authenticated endpoint that answers `accepted` and never echoes a
/// request header, as an endpoint must not for a grant to be given (D10).
async fn endpoint(tls: &Tls) -> Result<Endpoint> {
	let listener = TcpListener::bind(("127.0.0.1", 0))
		.await
		.context("bind endpoint")?;
	let port = listener.local_addr().context("endpoint address")?.port();
	let accepted = Arc::new(AtomicUsize::new(0));
	let requests = Arc::new(Mutex::new(Vec::new()));
	let (acceptor, count, log) =
		(tls.acceptor.clone(), Arc::clone(&accepted), Arc::clone(&requests));
	tokio::spawn(async move {
		while let Ok((tcp, _)) = listener.accept().await {
			count.fetch_add(1, Ordering::SeqCst);
			let (acceptor, log) = (acceptor.clone(), Arc::clone(&log));
			tokio::spawn(async move {
				let Ok(mut stream) = acceptor.accept(tcp).await else {
					return;
				};
				let request = read_request(&mut stream).await;
				log.lock().expect("requests").push(request);
				let body = "accepted";
				let response = format!(
					"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
					body.len()
				);
				let _ = stream.write_all(response.as_bytes()).await;
				let _ = stream.shutdown().await;
			});
		}
	});
	Ok(Endpoint { port, accepted, requests })
}

/// The host's stored key for `buildbuddy`, standing in for the credential
/// store: the extension never receives it.
struct StoredKey;

#[async_trait]
impl PlacementCredentials for StoredKey {
	async fn lease(
		&self,
		_provider: &ProviderId,
		_spec: &AuthSpecId,
	) -> Result<CredentialLease, CredentialError> {
		Ok(CredentialLease::api_key(
			LeaseMeta {
				account:    AccountId::from(sf!("buildbuddy:e2e")),
				principal:  PrincipalId::from("e2e"),
				generation: 1,
				expires_at: None,
			},
			SecretString::from(KEY.to_owned()),
		))
	}
}

fn find_outcome(value: &Value) -> Option<Value> {
	match value {
		Value::Object(map) if map.contains_key("outcome") => Some(value.clone()),
		Value::Object(map) => map.values().find_map(find_outcome),
		Value::Array(items) => items.iter().find_map(find_outcome),
		Value::String(text) => serde_json::from_str::<Value>(text)
			.ok()
			.and_then(|parsed| find_outcome(&parsed)),
		_ => None,
	}
}

async fn call_place(client: &EnvClient) -> Result<Value> {
	let mut invocation = within(
		"open extension invocation",
		STEP_LIMIT,
		client.invoke(InvokeTool {
			invocation_id: "p9-placement-place".to_owned(),
			name: "place".to_owned(),
			rev: format!("{EXTENSION}.1"),
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
			Bytes::from_static(b"p9-placement-token"),
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
					other => return Err(error(format!("extension tool returned {other:?}"))),
				}
			},
			Some(InvocationEvent::Update(_)) => {},
			Some(other) => return Err(error(format!("unexpected extension event {other:?}"))),
			None => return Err(error("extension invocation closed before its verdict")),
		}
	};
	find_outcome(&payload)
		.with_context(|| format!("the extension tool answered {payload} without an outcome"))
}

#[tokio::test]
async fn granted_extension_request_carries_the_host_placed_key() -> Result<()> {
	if !runs_user_extension_hosts()? {
		return Ok(());
	}
	let operator = OperatorHome::new()?;
	let scratch = Scratch::new()?;
	let tls = tls()?;
	let fake = endpoint(&tls).await?;
	let source = SOURCE
		.replace("__URL__", &format!("https://127.0.0.1:{}/run", fake.port))
		.replace("__KEY__", KEY);
	operator.install_extension(EXTENSION, &manifest(), &source)?;

	// The operator's credential grant, bound to the extension as installed.
	let identity =
		omp_driver::discovery::native::user_extension_grant_identity(operator.home(), EXTENSION)
			.context("reading the installed extension")?
			.context("the extension is not installed in the user layer")?;
	let grants_file = operator.grants_file();
	std::fs::create_dir_all(grants_file.parent().context("grant store directory")?)
		.context("creating the grant store directory")?;
	GrantsFile::persist_credential_grant(&grants_file, CredentialGrant {
		id:                identity.id.clone(),
		publisher:         identity.publisher.clone(),
		layer:             Layer::Client,
		capability_digest: identity.capability_digest.clone(),
		provider:          sf!("buildbuddy"),
		origins:           vec![
			CredentialOrigin::new("https", "127.0.0.1", Some(fake.port))
				.map_err(|source| error(format!("origin: {source}")))?,
		],
		operations:        vec![
			CredentialOperation::new("POST", "/run")
				.map_err(|source| error(format!("operation: {source}")))?,
		],
		granted_at:        sf!("2026-11-26T00:00:00Z"),
		granted_by:        sf!("p9-credential-placement"),
	})
	.context("writing the credential grant")?
	.map_err(|source| error(format!("grant refused: {source}")))?;

	let admission = operator.admit(scratch.project(), EXTENSION)?;
	assert!(
		admission.sandbox.confines(),
		"the Seatbelt probe passed, so admission must see a confining sandbox: {:?}",
		admission.sandbox
	);
	assert_eq!(
		admission.extension.spec.credential_grants.providers(),
		vec![sf!("buildbuddy")],
		"admission must turn the operator's record into the extension's credential grant"
	);

	let audit_path = operator.home().join("placement-audit.jsonl");
	let harness =
		InProcessEnvHarness::spawn_with_admitted_extension(scratch, &admission.extension).await?;
	harness
		.server()
		.placement_authority()
		.bind(PlacementBinding {
			credentials: Arc::new(StoredKey),
			audit:       Arc::new(JsonlPlacementAudit::new(audit_path.clone())),
			client:      Some(tls.client.clone()),
		})
		.map_err(|_| error("the placement authority was already bound"))?;

	let answer = call_place(harness.client()).await?;

	assert_eq!(answer["outcome"], "sent", "the extension's request was refused: {answer}");
	assert_eq!(answer["status"], 200);
	assert_eq!(answer["body"], "accepted");
	// Nothing the extension can read holds the key.
	assert_eq!(answer["key_in_headers"], false, "the key came back in a response header");
	assert_eq!(answer["key_in_environment"], false, "the key is in the extension's environment");
	assert!(!answer.to_string().contains(KEY), "the extension's return value holds the key");
	// The endpoint received the catalog's header with the key, once.
	assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
	let seen = fake.requests.lock().expect("requests").clone();
	assert_eq!(seen.len(), 1);
	assert!(
		seen[0]
			.to_ascii_lowercase()
			.contains(&format!("x-buildbuddy-api-key: {KEY}")),
		"the placed header did not arrive: {}",
		seen[0]
	);
	// The audit row names the extension and endpoint and never the key.
	let audit = std::fs::read_to_string(&audit_path).context("reading the placement audit")?;
	assert!(audit.contains("credential_placed") && audit.contains(EXTENSION), "{audit}");
	assert!(!audit.contains(KEY), "the audit file holds the key: {audit}");
	Ok(())
}
