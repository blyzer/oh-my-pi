//! ADR 0075 S4 acceptance: credential placement over a loopback TLS server.
//!
//! Every test counts the connections a listener accepts, so "zero requests"
//! means no TCP connection was opened, not merely no request line read.

use std::{
	io,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicUsize, Ordering},
	},
};

use async_trait::async_trait;
use bytes::Bytes;
use omp_ai::{
	auth::{CredentialError, CredentialLease, LeaseMeta},
	id::{AccountId, PrincipalId},
};
use omp_catalog::{AuthSpecId, ProviderId};
use omp_core::{SecretString, sf};
use omp_ext::{
	Layer,
	credential_grant::{
		CredentialGrant, CredentialGrants, CredentialOperation, CredentialOrigin, CredentialRefusal,
	},
};
use omp_proto::env::v1 as pb;
use tokio::{
	io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
	net::TcpListener,
};
use tokio_rustls::{TlsAcceptor, rustls};

use crate::{
	http_egress::{HttpEgressError, HttpEgressHost, PlacementRequest},
	placement::{
		JsonlPlacementAudit, PlacementAudit, PlacementAuditRow, PlacementBinding,
		PlacementCredentials, PlacementRefusal,
	},
};

const KEY: &str = "bb-test-key-0123456789abcdef";
const OWNER: &str = "extension:client:sandboxed:omp.buildbuddy";

struct Tls {
	acceptor: TlsAcceptor,
	client:   omp_http::Client,
}

/// A self-signed certificate for `127.0.0.1`, trusted by the client and used
/// by the server: the TLS seam of these tests.
fn tls() -> Tls {
	omp_http::install_tls_provider();
	let rcgen::CertifiedKey { cert, key_pair } =
		rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).expect("certificate");
	let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der());
	let config = rustls::ServerConfig::builder()
		.with_no_client_auth()
		.with_single_cert(vec![cert.der().clone()], key.into())
		.expect("server config");
	let root = reqwest::Certificate::from_pem(cert.pem().as_bytes()).expect("root certificate");
	let client = omp_http::client_builder()
		.redirect(reqwest::redirect::Policy::none())
		.no_proxy()
		.add_root_certificate(root)
		.build()
		.expect("client");
	Tls { acceptor: TlsAcceptor::from(Arc::new(config)), client: client.into() }
}

struct Server {
	port:     u16,
	accepted: Arc<AtomicUsize>,
	requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
	fn connections(&self) -> usize {
		self.accepted.load(Ordering::SeqCst)
	}

	fn requests(&self) -> Vec<String> {
		self.requests.lock().expect("requests").clone()
	}
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

/// Serves TLS on a loopback port, counting every accepted TCP connection.
async fn serve(tls: &Tls, respond: impl Fn(&str) -> String + Send + Sync + 'static) -> Server {
	let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
	let port = listener.local_addr().expect("address").port();
	let accepted = Arc::new(AtomicUsize::new(0));
	let requests = Arc::new(Mutex::new(Vec::new()));
	let respond = Arc::new(respond);
	let acceptor = tls.acceptor.clone();
	let (count, log) = (Arc::clone(&accepted), Arc::clone(&requests));
	tokio::spawn(async move {
		while let Ok((tcp, _)) = listener.accept().await {
			count.fetch_add(1, Ordering::SeqCst);
			let (acceptor, respond, log) = (acceptor.clone(), Arc::clone(&respond), Arc::clone(&log));
			tokio::spawn(async move {
				let Ok(mut stream) = acceptor.accept(tcp).await else {
					return;
				};
				let request = read_request(&mut stream).await;
				let response = respond(&request);
				log.lock().expect("requests").push(request);
				let _ = stream.write_all(response.as_bytes()).await;
				let _ = stream.shutdown().await;
			});
		}
	});
	Server { port, accepted, requests }
}

fn ok(body: &str) -> String {
	format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn redirect(status: u16, location: &str) -> String {
	format!(
		"HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: \
		 close\r\n\r\n"
	)
}

fn grant_for(port: u16) -> CredentialGrants {
	CredentialGrants::new([CredentialGrant {
		id:                sf!("omp.buildbuddy"),
		publisher:         sf!("path:/ext/buildbuddy"),
		layer:             Layer::Client,
		capability_digest: sf!("digest"),
		provider:          sf!("buildbuddy"),
		origins:           vec![
			CredentialOrigin::new("https", "127.0.0.1", Some(port)).expect("origin"),
		],
		operations:        vec![CredentialOperation::new("POST", "/run").expect("operation")],
		granted_at:        sf!("2026-11-26T00:00:00Z"),
		granted_by:        sf!("test"),
	}])
}

struct FakeCredentials {
	calls:  AtomicUsize,
	stored: bool,
}

#[async_trait]
impl PlacementCredentials for FakeCredentials {
	async fn lease(
		&self,
		_provider: &ProviderId,
		_spec: &AuthSpecId,
	) -> Result<CredentialLease, CredentialError> {
		self.calls.fetch_add(1, Ordering::SeqCst);
		if !self.stored {
			return Err(CredentialError::Unavailable);
		}
		Ok(CredentialLease::api_key(
			LeaseMeta {
				account:    AccountId::from(sf!("buildbuddy:test")),
				principal:  PrincipalId::from("test"),
				generation: 1,
				expires_at: None,
			},
			SecretString::from(KEY.to_owned()),
		))
	}
}

#[derive(Default)]
struct RecordingAudit {
	rows: Mutex<Vec<PlacementAuditRow>>,
	fail: AtomicBool,
}

#[async_trait]
impl PlacementAudit for RecordingAudit {
	async fn record(&self, row: &PlacementAuditRow) -> io::Result<()> {
		if self.fail.load(Ordering::SeqCst) {
			return Err(io::Error::other("audit sink unavailable"));
		}
		self.rows.lock().expect("rows").push(row.clone());
		Ok(())
	}
}

struct Fixture {
	host:        HttpEgressHost,
	credentials: Arc<FakeCredentials>,
	audit:       Arc<RecordingAudit>,
	tls:         Tls,
}

fn fixture(bound: bool, stored: bool) -> Fixture {
	let tls = tls();
	let host = HttpEgressHost::new();
	let credentials = Arc::new(FakeCredentials { calls: AtomicUsize::new(0), stored });
	let audit = Arc::new(RecordingAudit::default());
	if bound {
		host
			.placement()
			.bind(PlacementBinding {
				credentials: credentials.clone(),
				audit:       audit.clone(),
				client:      Some(tls.client.clone()),
			})
			.unwrap_or_else(|_| panic!("authority already bound"));
	}
	Fixture { host, credentials, audit, tls }
}

fn request(port: u16, path: &str) -> pb::HttpRequest {
	pb::HttpRequest {
		method: "POST".to_owned(),
		url: format!("https://127.0.0.1:{port}{path}"),
		body: Bytes::from_static(b"payload"),
		timeout_ms: 5_000,
		redirects: 3,
		credential: Some("buildbuddy".to_owned()),
		..Default::default()
	}
}

fn header(name: &str, value: &str) -> pb::HttpHeader {
	pb::HttpHeader { name: name.to_owned(), value: value.to_owned(), props: None }
}

impl Fixture {
	async fn place(
		&self,
		grants: &CredentialGrants,
		request: pb::HttpRequest,
	) -> Result<pb::HttpResponse, HttpEgressError> {
		let context = PlacementRequest { grants, owner: OWNER, request_id: 7 };
		self.host.request(request, Some(context)).await
	}
}

fn refusal(result: Result<pb::HttpResponse, HttpEgressError>) -> PlacementRefusal {
	match result {
		Err(HttpEgressError::Placement(refusal)) => refusal,
		Err(other) => panic!("expected a placement refusal, got {other}"),
		Ok(response) => panic!("the request was sent and answered {}", response.status),
	}
}

#[tokio::test]
async fn no_grant_means_no_send() {
	let fixture = fixture(true, true);
	let server = serve(&fixture.tls, |_| ok("ok")).await;

	let none = CredentialGrants::default();
	let result = fixture.place(&none, request(server.port, "/run")).await;

	assert_eq!(refusal(result), PlacementRefusal::Grant(CredentialRefusal::NoGrant));
	assert_eq!(server.connections(), 0, "a connection was opened");
	assert_eq!(fixture.credentials.calls.load(Ordering::SeqCst), 0, "the key was leased anyway");
	assert!(fixture.audit.rows.lock().expect("rows").is_empty());

	// Another provider's grant never covers this one.
	let mut other = request(server.port, "/run");
	other.credential = Some("someone-else".to_owned());
	assert_eq!(
		refusal(fixture.place(&grant_for(server.port), other).await),
		PlacementRefusal::Grant(CredentialRefusal::ProviderNotGranted)
	);
	assert_eq!(server.connections(), 0);
}

#[tokio::test]
async fn placed_header_reaches_only_the_granted_host() {
	let fixture = fixture(true, true);
	let granted = serve(&fixture.tls, |_| ok("done")).await;
	let other = serve(&fixture.tls, |_| ok("other")).await;
	let grants = grant_for(granted.port);

	let response = fixture
		.place(&grants, request(granted.port, "/run"))
		.await
		.expect("granted request");

	assert_eq!(response.status, 200);
	let seen = granted.requests();
	assert_eq!(seen.len(), 1);
	assert!(
		seen[0]
			.to_ascii_lowercase()
			.contains(&format!("x-buildbuddy-api-key: {KEY}")),
		"the header did not arrive: {}",
		seen[0]
	);
	assert!(seen[0].ends_with("payload"), "the body was not sent");
	// Nothing returned to the extension holds the key.
	let returned = format!("{response:?}");
	assert!(!returned.contains(KEY), "{returned}");
	// The audit row names the extension, provider and host, never the key.
	let rows = fixture.audit.rows.lock().expect("rows").clone();
	assert_eq!(rows.len(), 1);
	assert_eq!(rows[0].extension.as_str(), OWNER);
	assert_eq!(rows[0].provider.as_str(), "buildbuddy");
	assert_eq!((rows[0].host.as_str(), rows[0].port, rows[0].hop), ("127.0.0.1", granted.port, 0));
	assert!(!format!("{rows:?}").contains(KEY));
	// The ungranted listener saw nothing.
	assert_eq!(other.connections(), 0);
	assert_eq!(granted.requests().len(), 1);
}

#[tokio::test]
async fn same_host_other_port_receives_zero_requests() {
	let fixture = fixture(true, true);
	let granted = serve(&fixture.tls, |_| ok("done")).await;
	let second = serve(&fixture.tls, |_| ok("second")).await;

	let result = fixture
		.place(&grant_for(granted.port), request(second.port, "/run"))
		.await;

	assert_eq!(refusal(result), PlacementRefusal::Grant(CredentialRefusal::OriginNotGranted));
	assert_eq!(second.connections(), 0, "a second port on the same host is another origin");
	assert_eq!(granted.connections(), 0);
}

#[tokio::test]
async fn ungranted_redirect_hop_receives_zero_requests() {
	let fixture = fixture(true, true);
	let tls = &fixture.tls;
	let target = serve(tls, |_| ok("target")).await;
	let target_port = target.port;
	for status in [302_u16, 307] {
		let location = format!("https://127.0.0.1:{target_port}/x");
		let granted = serve(tls, move |_| redirect(status, &location)).await;

		let result = fixture
			.place(&grant_for(granted.port), request(granted.port, "/run"))
			.await;

		assert_eq!(
			refusal(result),
			PlacementRefusal::RedirectHop(CredentialRefusal::OriginNotGranted),
			"status {status}"
		);
		assert_eq!(granted.connections(), 1, "the granted hop is sent once, status {status}");
		assert_eq!(
			target.connections(),
			0,
			"status {status}: the body was replayed to another origin"
		);
	}
	// A redirect inside the origin to an operation the grant omits is refused too.
	let port_cell = Arc::new(AtomicUsize::new(0));
	let seen_port = Arc::clone(&port_cell);
	let same = serve(tls, move |_| {
		redirect(302, &format!("https://127.0.0.1:{}/run", seen_port.load(Ordering::SeqCst)))
	})
	.await;
	port_cell.store(usize::from(same.port), Ordering::SeqCst);
	let result = fixture
		.place(&grant_for(same.port), request(same.port, "/run"))
		.await;
	// 302 turns the POST into a GET, which the grant does not list.
	assert_eq!(
		refusal(result),
		PlacementRefusal::RedirectHop(CredentialRefusal::OperationNotGranted)
	);
	assert_eq!(same.connections(), 1);
}

#[tokio::test]
async fn caller_host_header_refused() {
	let fixture = fixture(true, true);
	let server = serve(&fixture.tls, |_| ok("ok")).await;
	let mut request = request(server.port, "/run");
	request.headers = vec![header("Host", "evil.example")];

	let result = fixture.place(&grant_for(server.port), request).await;

	assert_eq!(refusal(result), PlacementRefusal::CallerHostHeader);
	assert_eq!(server.connections(), 0);
	assert_eq!(fixture.credentials.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn placed_header_set_by_request_refused() {
	let fixture = fixture(true, true);
	let server = serve(&fixture.tls, |_| ok("ok")).await;
	let mut request = request(server.port, "/run");
	request.headers = vec![header("X-BuildBuddy-Api-Key", "caller-chosen")];

	let result = fixture.place(&grant_for(server.port), request).await;

	assert_eq!(refusal(result), PlacementRefusal::PlacedHeaderSetByRequest);
	assert_eq!(server.connections(), 0);
}

#[tokio::test]
async fn non_https_refused() {
	let fixture = fixture(true, true);
	let server = serve(&fixture.tls, |_| ok("ok")).await;
	let mut request = request(server.port, "/run");
	request.url = format!("http://127.0.0.1:{}/run", server.port);

	let result = fixture.place(&grant_for(server.port), request).await;

	assert_eq!(refusal(result), PlacementRefusal::Grant(CredentialRefusal::NotHttps));
	assert_eq!(server.connections(), 0);
}

#[tokio::test]
async fn ungranted_operation_refused() {
	let fixture = fixture(true, true);
	let server = serve(&fixture.tls, |_| ok("ok")).await;
	let grants = grant_for(server.port);

	let other_path = fixture.place(&grants, request(server.port, "/admin")).await;
	let mut get = request(server.port, "/run");
	get.method = "GET".to_owned();
	let other_method = fixture.place(&grants, get).await;

	assert_eq!(refusal(other_path), PlacementRefusal::Grant(CredentialRefusal::OperationNotGranted));
	assert_eq!(
		refusal(other_method),
		PlacementRefusal::Grant(CredentialRefusal::OperationNotGranted)
	);
	assert_eq!(server.connections(), 0);
}

#[tokio::test]
async fn remote_environment_refuses_placement() {
	// An Environment composed without a credential authority, such as a remote
	// one, cannot place: the request is refused and the key is never asked for.
	let fixture = fixture(false, true);
	let server = serve(&fixture.tls, |_| ok("ok")).await;

	let result = fixture
		.place(&grant_for(server.port), request(server.port, "/run"))
		.await;

	assert_eq!(refusal(result), PlacementRefusal::NoAuthority);
	assert_eq!(server.connections(), 0);
	assert_eq!(fixture.credentials.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn audit_failure_blocks_send() {
	let fixture = fixture(true, true);
	let server = serve(&fixture.tls, |_| ok("ok")).await;
	fixture.audit.fail.store(true, Ordering::SeqCst);

	let result = fixture
		.place(&grant_for(server.port), request(server.port, "/run"))
		.await;

	assert_eq!(refusal(result), PlacementRefusal::AuditFailed);
	assert_eq!(server.connections(), 0, "the request was sent without an audit row");
}

#[tokio::test]
async fn no_stored_credential_means_no_send() {
	let fixture = fixture(true, false);
	let server = serve(&fixture.tls, |_| ok("ok")).await;

	let result = fixture
		.place(&grant_for(server.port), request(server.port, "/run"))
		.await;

	assert_eq!(refusal(result), PlacementRefusal::NoStoredCredential);
	assert_eq!(server.connections(), 0);
	assert_eq!(fixture.credentials.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn response_containing_placed_value_is_not_returned() {
	let fixture = fixture(true, true);
	// An endpoint that echoes the request, key included, in the body.
	let echo_body = serve(&fixture.tls, |request| ok(request)).await;
	let result = fixture
		.place(&grant_for(echo_body.port), request(echo_body.port, "/run"))
		.await;
	assert_eq!(refusal(result), PlacementRefusal::ResponseEchoesCredential);

	// One that reflects it in a response header.
	let echo_header = serve(&fixture.tls, |request| {
		let key = request
			.lines()
			.find_map(|line| {
				line
					.to_ascii_lowercase()
					.strip_prefix("x-buildbuddy-api-key: ")
					.map(str::to_owned)
			})
			.unwrap_or_default();
		format!(
			"HTTP/1.1 200 OK\r\nX-Echo: {key}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
		)
	})
	.await;
	let result = fixture
		.place(&grant_for(echo_header.port), request(echo_header.port, "/run"))
		.await;
	assert_eq!(refusal(result), PlacementRefusal::ResponseEchoesCredential);

	// Control: a response without the value is returned.
	let quiet = serve(&fixture.tls, |_| ok("fine")).await;
	let response = fixture
		.place(&grant_for(quiet.port), request(quiet.port, "/run"))
		.await
		.expect("quiet endpoint");
	assert_eq!(response.body.as_ref(), b"fine");
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogBuffer {
	fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
		self.0.lock().expect("log").extend_from_slice(bytes);
		Ok(bytes.len())
	}

	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
	type Writer = Self;

	fn make_writer(&'a self) -> Self::Writer {
		self.clone()
	}
}

#[tokio::test]
async fn plaintext_absent_from_logs_errors_and_traces() {
	let logs = LogBuffer::default();
	let subscriber = tracing_subscriber::fmt()
		.with_max_level(tracing::Level::TRACE)
		.with_ansi(false)
		.with_writer(logs.clone())
		.finish();
	let _guard = tracing::subscriber::set_default(subscriber);

	let directory = tempfile::tempdir().expect("tempdir");
	let audit_path = directory.path().join("placement-audit.jsonl");
	let tls = tls();
	let host = HttpEgressHost::new();
	let credentials = Arc::new(FakeCredentials { calls: AtomicUsize::new(0), stored: true });
	host
		.placement()
		.bind(PlacementBinding {
			credentials,
			audit: Arc::new(JsonlPlacementAudit::new(audit_path.clone())),
			client: Some(tls.client.clone()),
		})
		.unwrap_or_else(|_| panic!("authority already bound"));
	let echo = serve(&tls, |request| ok(request)).await;
	let quiet = serve(&tls, |_| ok("fine")).await;
	let place = |grants: CredentialGrants, request: pb::HttpRequest| {
		let host = host.clone();
		async move {
			let context = PlacementRequest { grants: &grants, owner: OWNER, request_id: 9 };
			host.request(request, Some(context)).await
		}
	};

	let mut outputs = Vec::new();
	outputs.push(format!("{:?}", place(grant_for(quiet.port), request(quiet.port, "/run")).await));
	let echoed = place(grant_for(echo.port), request(echo.port, "/run")).await;
	outputs.push(format!("{echoed:?}"));
	if let Err(error) = &echoed {
		outputs.push(error.to_string());
	}
	let mut spoof = request(quiet.port, "/run");
	spoof.headers = vec![header("x-buildbuddy-api-key", "caller")];
	let spoofed = place(grant_for(quiet.port), spoof).await;
	outputs.push(format!("{spoofed:?}"));
	outputs
		.push(format!("{:?}", place(CredentialGrants::default(), request(quiet.port, "/run")).await));

	for output in &outputs {
		assert!(!output.contains(KEY), "a result carries the key: {output}");
	}
	let log = String::from_utf8(logs.0.lock().expect("log").clone()).expect("utf-8 log");
	assert!(!log.contains(KEY), "a log line carries the key: {log}");
	let audit = std::fs::read_to_string(&audit_path).expect("audit file");
	assert!(audit.contains("credential_placed") && audit.contains("buildbuddy"), "{audit}");
	assert!(!audit.contains(KEY), "the audit file carries the key: {audit}");
}
