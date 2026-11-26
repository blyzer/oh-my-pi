//! Capability-gated, bounded HTTP egress owned by the Environment.

use std::{env, sync::Arc, time::Duration};

use bytes::{Bytes, BytesMut};
use futures::StreamExt as _;
use http::{
	HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
	header::{
		AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, LOCATION, PROXY_AUTHORIZATION,
	},
};
use omp_proto::env::v1 as pb;
use strum::Display;
use thiserror::Error;
use tokio::time;
use url::Url;

use super::{
	mcp::transport::header_policy::is_credential_header,
	placement::{
		PlacementAuditRow, PlacementAuthority, PlacementBinding, PlacementRefusal, ResolvedProvider,
		check_caller_headers, lease_credential, resolve_provider,
	},
	worker_pool::MAX_TUNNEL_BUFFER_BYTES,
};

// Reuse the existing retained wire-buffer ceiling rather than defining a
// second Environment response-size policy.
const MAX_REDIRECTS: u32 = 10;

/// The grants and identity a placed request is decided under, taken from the
/// connection it arrived on. The extension never supplies any of it.
pub struct PlacementRequest<'a> {
	/// Host-held grants of the requesting extension.
	pub(crate) grants:     &'a omp_ext::credential_grant::CredentialGrants,
	/// Connection identity of the requesting extension.
	pub(crate) owner:      &'a str,
	/// Protocol request id of the request.
	pub(crate) request_id: u64,
}

/// One placed request, resolved once and re-decided on every hop (ADR 0075 D5).
struct Placed<'a> {
	provider: &'a str,
	context:  &'a PlacementRequest<'a>,
	resolved: ResolvedProvider,
	binding:  &'a PlacementBinding,
}

impl Placed<'_> {
	/// Decides one hop and writes the key into `headers`, just before the hop
	/// is sent. Returns the placed value so the response can be checked for an
	/// echo (D10); it is zeroized on drop.
	async fn place(
		&self,
		method: &Method,
		url: &Url,
		hop: u32,
		headers: &mut HeaderMap,
	) -> Result<zeroize::Zeroizing<Vec<u8>>, PlacementRefusal> {
		self
			.context
			.grants
			.authorize(self.provider, method.as_str(), url)
			.map_err(|refusal| {
				if hop == 0 {
					PlacementRefusal::Grant(refusal)
				} else {
					PlacementRefusal::RedirectHop(refusal)
				}
			})?;
		let lease = lease_credential(self.binding, &self.resolved).await?;
		let row = PlacementAuditRow {
			extension: omp_core::Str::new(self.context.owner),
			provider: omp_core::Str::new(self.provider),
			host: omp_core::Str::new(url.host_str().unwrap_or_default()),
			port: url.port_or_known_default().unwrap_or_default(),
			method: omp_core::Str::new(method.as_str()),
			request_id: self.context.request_id,
			hop,
		};
		if let Err(error) = self.binding.audit.record(&row).await {
			tracing::warn!(kind = ?error.kind(), "credential placement audit write failed");
			return Err(PlacementRefusal::AuditFailed);
		}
		lease
			.apply_header(&self.resolved.header, headers)
			.map_err(|_| PlacementRefusal::CredentialStoreFailed)?;
		Ok(zeroize::Zeroizing::new(
			headers
				.get(&self.resolved.name)
				.map(|value| value.as_bytes().to_vec())
				.unwrap_or_default(),
		))
	}
}

#[derive(Clone)]
pub struct HttpEgressHost {
	client:    omp_http::Client,
	placement: Arc<PlacementAuthority>,
}

impl HttpEgressHost {
	pub(crate) fn new() -> Self {
		let client = omp_http::no_redirect_client();
		Self { client, placement: Arc::new(PlacementAuthority::default()) }
	}

	/// The credential authority this host places keys with, bound at
	/// composition. Unbound, a placed request is refused.
	pub(crate) const fn placement(&self) -> &Arc<PlacementAuthority> {
		&self.placement
	}

	#[tracing::instrument(
		name = "http_egress_request",
		level = "debug",
		skip_all,
		fields(method = %request.method, host = tracing::field::Empty),
	)]
	pub(crate) async fn request(
		&self,
		request: pb::HttpRequest,
		placement: Option<PlacementRequest<'_>>,
	) -> Result<pb::HttpResponse, HttpEgressError> {
		if let Ok(url) = Url::parse(&request.url)
			&& let Some(host) = url.host_str()
		{
			tracing::Span::current().record("host", host);
		}
		let timeout_ms = request.timeout_ms;
		let request = self.request_once(request, placement);
		if timeout_ms == 0 {
			request.await
		} else {
			match time::timeout(Duration::from_millis(timeout_ms), request).await {
				Ok(result) => result,
				Err(_) => {
					tracing::warn!("HTTP egress request timed out");
					Err(HttpEgressError::TimedOut)
				},
			}
		}
	}

	/// Resolves a placed request before the first hop: the grant, the catalog
	/// provider, the caller's headers, and the bound authority, in that order.
	fn begin_placement<'a>(
		&'a self,
		provider: &'a str,
		context: &'a PlacementRequest<'a>,
		method: &Method,
		url: &Url,
		headers: &HeaderMap,
	) -> Result<Placed<'a>, PlacementRefusal> {
		context
			.grants
			.authorize(provider, method.as_str(), url)
			.map_err(PlacementRefusal::Grant)?;
		let resolved = resolve_provider(provider)?;
		check_caller_headers(headers, &resolved.name)?;
		let binding = self
			.placement
			.binding()
			.ok_or(PlacementRefusal::NoAuthority)?;
		Ok(Placed { provider, context, resolved, binding })
	}

	async fn request_once(
		&self,
		request: pb::HttpRequest,
		placement: Option<PlacementRequest<'_>>,
	) -> Result<pb::HttpResponse, HttpEgressError> {
		if request.redirects > MAX_REDIRECTS {
			return Err(HttpEgressError::InvalidArgument(format!(
				"HTTP egress redirects must be between 0 and {MAX_REDIRECTS}"
			)));
		}
		let mut method = parse_method(&request.method)?;
		let mut url = parse_url(&request.url)?;
		let mut headers = parse_headers(&request.headers)?;
		let mut body = request.body;
		let mut followed = 0;
		// A request that names a provider is placed or refused: it never falls
		// back to going out without the host's decision (D6).
		let placed = match request.credential.as_deref() {
			None => None,
			Some(provider) => {
				let context = placement.as_ref().ok_or(PlacementRefusal::Grant(
					omp_ext::credential_grant::CredentialRefusal::NoGrant,
				))?;
				Some(self.begin_placement(provider, context, &method, &url, &headers)?)
			},
		};

		loop {
			let mut hop_headers = headers.clone();
			let placed_value = match &placed {
				Some(placed) => Some(
					placed
						.place(&method, &url, followed, &mut hop_headers)
						.await?,
				),
				None => None,
			};
			let client = placed
				.as_ref()
				.and_then(|placed| placed.binding.client.as_ref())
				.unwrap_or(&self.client);
			let response = client
				.request(method.clone(), url.as_str())
				.headers(hop_headers)
				.body(body.clone())
				.send()
				.await
				.map_err(HttpEgressError::transport)?;
			let status_code = response.status();
			if followed < request.redirects
				&& let Some(location) = redirect_location(status_code, response.headers())
			{
				let next_url = parse_redirect_url(&url, &location)?;
				if !same_origin(&url, &next_url) {
					headers.remove(AUTHORIZATION);
					headers.remove(COOKIE);
					headers.remove(HOST);
					headers.remove(PROXY_AUTHORIZATION);
					strip_credential_headers(&mut headers);
				}
				if redirects_to_get(status_code, &method) {
					method = Method::GET;
					body = Bytes::new();
					headers.remove(CONTENT_LENGTH);
					headers.remove(CONTENT_TYPE);
				}
				url = next_url;
				followed += 1;
				continue;
			}

			let status = u32::from(status_code.as_u16());
			let response_headers = response_headers(response.headers());
			let body = read_bounded(response).await?;
			if let Some(value) = &placed_value
				&& echoes_placed_value(&response_headers, &body, value)
			{
				return Err(PlacementRefusal::ResponseEchoesCredential.into());
			}
			return Ok(pb::HttpResponse {
				status,
				headers: response_headers,
				body,
				final_url: if followed == 0 {
					request.url
				} else {
					url.as_str().to_owned()
				},
				props: None,
			});
		}
	}
}

/// Whether the response carries the placed value in a header or the body.
/// Defense in depth (ADR 0075 D10), not a guarantee: an endpoint can re-encode
/// the value.
fn echoes_placed_value(headers: &[pb::HttpHeader], body: &[u8], value: &[u8]) -> bool {
	let contains = |haystack: &[u8]| {
		!value.is_empty() && haystack.windows(value.len()).any(|window| window == value)
	};
	headers
		.iter()
		.any(|header| contains(header.value.as_bytes()))
		|| contains(body)
}

#[derive(Debug, Error)]
pub enum HttpEgressError {
	#[error("invalid HTTP egress request: {0}")]
	InvalidArgument(String),
	#[error("HTTP egress request timed out")]
	TimedOut,
	#[error("HTTP egress response exceeds the bounded frame limit")]
	ResponseTooLarge,
	#[error(transparent)]
	Placement(#[from] PlacementRefusal),
	#[error(
		"HTTP egress cannot use the SOCKS proxy configured by {variable}; unset {variable} or \
		 configure an HTTP proxy"
	)]
	UnsupportedSocksProxy {
		/// Highest-precedence proxy environment variable selecting SOCKS.
		variable: ProxyVariable,
		/// Original transport failure.
		#[source]
		source:   reqwest::Error,
	},
	#[error("HTTP egress transport failed: {0}")]
	Transport(String),
}

impl HttpEgressError {
	/// Creates an invalid-argument error, rendering the cause once at this
	/// model-facing fault boundary.
	fn invalid_argument(cause: impl std::fmt::Display) -> Self {
		Self::InvalidArgument(cause.to_string())
	}

	/// Creates a contextualized invalid-argument error, rendering the cause
	/// once at this model-facing fault boundary.
	fn invalid_argument_in(context: impl std::fmt::Display, cause: impl std::fmt::Display) -> Self {
		Self::InvalidArgument(format!("{context}: {cause}"))
	}

	fn transport(error: reqwest::Error) -> Self {
		let diagnostic = error.to_string();
		let proxy_failure = {
			let diagnostic = diagnostic.to_ascii_lowercase();
			diagnostic.contains("socks") || diagnostic.contains("proxy")
		};
		if proxy_failure && let Some(variable) = configured_socks_proxy() {
			return Self::UnsupportedSocksProxy { variable, source: error };
		}
		Self::Transport(diagnostic)
	}
}
/// Proxy environment variables in the same precedence order used by HTTP
/// clients for HTTPS destinations.
#[derive(Clone, Copy, Debug, Eq, Display, PartialEq)]
pub enum ProxyVariable {
	/// HTTPS-specific proxy.
	#[strum(to_string = "HTTPS_PROXY")]
	Https,
	/// Protocol-independent proxy.
	#[strum(to_string = "ALL_PROXY")]
	All,
	/// HTTP-specific proxy.
	#[strum(to_string = "HTTP_PROXY")]
	Http,
}

fn configured_socks_proxy() -> Option<ProxyVariable> {
	[
		("HTTPS_PROXY", ProxyVariable::Https),
		("ALL_PROXY", ProxyVariable::All),
		("HTTP_PROXY", ProxyVariable::Http),
	]
	.into_iter()
	.find_map(|(name, variable)| {
		let value = env::var_os(name)?;
		value
			.to_str()
			.is_some_and(|value| {
				let value = value.trim();
				value
					.get(..5)
					.is_some_and(|scheme| scheme.eq_ignore_ascii_case("socks"))
			})
			.then_some(variable)
	})
}

/// Removes every header whose name looks like a credential, using the same
/// predicate as MCP redirects ([`is_credential_header`]). A custom header such
/// as `x-buildbuddy-api-key` or `x-apikey` is then never replayed to another
/// origin after a redirect.
fn strip_credential_headers(headers: &mut HeaderMap) {
	let credential_names = headers
		.keys()
		.filter(|name| is_credential_header(name))
		.cloned()
		.collect::<Vec<HeaderName>>();
	for name in credential_names {
		headers.remove(name);
	}
}

fn parse_method(method: &str) -> Result<Method, HttpEgressError> {
	match method {
		"GET" => Ok(Method::GET),
		"POST" => Ok(Method::POST),
		"PUT" => Ok(Method::PUT),
		_ => {
			Err(HttpEgressError::InvalidArgument(format!("unsupported HTTP egress method {method:?}")))
		},
	}
}

fn parse_url(value: &str) -> Result<Url, HttpEgressError> {
	let url = Url::parse(value).map_err(HttpEgressError::invalid_argument)?;
	if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
		return Err(HttpEgressError::InvalidArgument(
			"HTTP egress URL must use http or https and include a host".to_owned(),
		));
	}
	Ok(url)
}

fn redirect_location(status: StatusCode, headers: &HeaderMap) -> Option<String> {
	if !matches!(
		status,
		StatusCode::MOVED_PERMANENTLY
			| StatusCode::FOUND
			| StatusCode::SEE_OTHER
			| StatusCode::TEMPORARY_REDIRECT
			| StatusCode::PERMANENT_REDIRECT
	) {
		return None;
	}
	headers
		.get(LOCATION)
		.and_then(|location| location.to_str().ok())
		.map(str::to_owned)
}

fn parse_redirect_url(base: &Url, location: &str) -> Result<Url, HttpEgressError> {
	let url = base
		.join(location)
		.map_err(|error| HttpEgressError::invalid_argument_in("invalid redirect URL", error))?;
	if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
		return Err(HttpEgressError::InvalidArgument(
			"HTTP redirect URL must use http or https and include a host".to_owned(),
		));
	}
	Ok(url)
}

fn same_origin(left: &Url, right: &Url) -> bool {
	left.scheme() == right.scheme()
		&& left.host_str() == right.host_str()
		&& left.port_or_known_default() == right.port_or_known_default()
}

fn redirects_to_get(status: StatusCode, method: &Method) -> bool {
	(status == StatusCode::SEE_OTHER && *method != Method::GET && *method != Method::HEAD)
		|| (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
			&& *method == Method::POST)
}

fn parse_headers(headers: &[pb::HttpHeader]) -> Result<HeaderMap, HttpEgressError> {
	let mut parsed = HeaderMap::with_capacity(headers.len());
	for header in headers {
		let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|error| {
			HttpEgressError::invalid_argument_in(
				format!("invalid HTTP header name {:?}", header.name),
				error,
			)
		})?;
		let value = HeaderValue::from_str(&header.value).map_err(|error| {
			HttpEgressError::invalid_argument_in(
				format!("invalid HTTP header value for {:?}", header.name),
				error,
			)
		})?;
		parsed.append(name, value);
	}
	Ok(parsed)
}

fn response_headers(headers: &HeaderMap) -> Vec<pb::HttpHeader> {
	headers
		.iter()
		.map(|(name, value)| pb::HttpHeader {
			name:  name.as_str().to_owned(),
			value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
			props: None,
		})
		.collect()
}

async fn read_bounded(response: reqwest::Response) -> Result<Bytes, HttpEgressError> {
	if response
		.content_length()
		.is_some_and(|length| length > MAX_TUNNEL_BUFFER_BYTES as u64)
	{
		return Err(HttpEgressError::ResponseTooLarge);
	}
	let mut body = BytesMut::with_capacity(
		response
			.content_length()
			.and_then(|length| usize::try_from(length).ok())
			.unwrap_or_default()
			.min(MAX_TUNNEL_BUFFER_BYTES),
	);
	let mut stream = response.bytes_stream();
	while let Some(chunk) = stream.next().await {
		let chunk = chunk.map_err(HttpEgressError::transport)?;
		if body.len().saturating_add(chunk.len()) > MAX_TUNNEL_BUFFER_BYTES {
			return Err(HttpEgressError::ResponseTooLarge);
		}
		body.extend_from_slice(&chunk);
	}
	Ok(body.freeze())
}

#[cfg(test)]
mod tests {
	use tokio::{
		io::{AsyncReadExt as _, AsyncWriteExt as _},
		net::{TcpListener, TcpStream},
		sync::oneshot,
	};

	use super::*;

	fn header(name: &str, value: &str) -> pb::HttpHeader {
		pb::HttpHeader { name: name.into(), value: value.into(), ..Default::default() }
	}

	const MAX_HEAD_BYTES: usize = 16 * 1024;

	/// Reads one request head, through the blank line that ends it, as lowercase
	/// text.
	async fn read_head(stream: &mut TcpStream) -> String {
		let mut head = Vec::new();
		let mut chunk = [0_u8; 1024];
		while !head.windows(4).any(|window| window == b"\r\n\r\n") {
			assert!(head.len() < MAX_HEAD_BYTES, "request head exceeds {MAX_HEAD_BYTES} bytes");
			let read = stream.read(&mut chunk).await.expect("read request");
			assert_ne!(read, 0, "connection closed before the request head ended");
			head.extend_from_slice(&chunk[..read]);
		}
		String::from_utf8_lossy(&head).to_ascii_lowercase()
	}

	/// Serves one connection per entry of `responses`, in order, and returns
	/// the lowercased request head each connection received.
	fn serve(listener: TcpListener, responses: Vec<String>) -> oneshot::Receiver<Vec<String>> {
		let (sender, receiver) = oneshot::channel();
		tokio::spawn(async move {
			let mut heads = Vec::new();
			for response in responses {
				let (mut stream, _) = listener.accept().await.expect("accept");
				heads.push(read_head(&mut stream).await);
				stream
					.write_all(response.as_bytes())
					.await
					.expect("write response");
				stream.shutdown().await.ok();
			}
			let _ = sender.send(heads);
		});
		receiver
	}

	fn redirect_to(location: &str) -> String {
		format!(
			"HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: \
			 close\r\n\r\n"
		)
	}

	const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

	fn request_to(url: String) -> pb::HttpRequest {
		pb::HttpRequest {
			method: "GET".into(),
			url,
			headers: vec![
				header("accept", "text/plain"),
				header("authorization", "Bearer a"),
				header("x-buildbuddy-api-key", "k1"),
				header("x-request-id", "r1"),
			],
			timeout_ms: 5_000,
			redirects: 3,
			..Default::default()
		}
	}

	#[tokio::test]
	async fn a_cross_origin_redirect_drops_credential_named_headers() {
		let first = TcpListener::bind(("127.0.0.1", 0))
			.await
			.expect("bind first");
		let second = TcpListener::bind(("127.0.0.1", 0))
			.await
			.expect("bind second");
		let first_port = first.local_addr().expect("first addr").port();
		let second_port = second.local_addr().expect("second addr").port();
		let _first = serve(first, vec![redirect_to(&format!("http://127.0.0.1:{second_port}/x"))]);
		let heads = serve(second, vec![OK.to_owned()]);

		let response = HttpEgressHost::new()
			.request(request_to(format!("http://127.0.0.1:{first_port}/start")), None)
			.await
			.expect("redirected request");
		assert_eq!(response.status, 200);

		let head = &heads.await.expect("second server head")[0];
		assert!(head.contains("accept: text/plain"), "{head}");
		assert!(head.contains("x-request-id: r1"), "{head}");
		assert!(!head.contains("authorization"), "{head}");
		assert!(!head.contains("x-buildbuddy-api-key"), "{head}");
	}

	/// Same-origin redirects keep credential headers; only a method-rewriting
	/// redirect drops `content-length` and `content-type`.
	#[tokio::test]
	async fn a_same_origin_redirect_keeps_credential_headers() {
		let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
		let port = listener.local_addr().expect("addr").port();
		let heads = serve(listener, vec![
			redirect_to(&format!("http://127.0.0.1:{port}/final")),
			OK.to_owned(),
		]);

		let response = HttpEgressHost::new()
			.request(request_to(format!("http://127.0.0.1:{port}/start")), None)
			.await
			.expect("redirected request");
		assert_eq!(response.status, 200);

		let heads = heads.await.expect("server heads");
		assert!(heads[1].contains("authorization: bearer a"), "{}", heads[1]);
		assert!(heads[1].contains("x-buildbuddy-api-key: k1"), "{}", heads[1]);
	}

	#[test]
	fn credential_named_headers_are_stripped() {
		for name in [
			"x-api-key",
			"x-apikey",
			"apikey",
			"x-mbx-apikey",
			"x-auth-token",
			"x-buildbuddy-api-key",
			"x-amz-security-token",
			"x-amz-credential",
			"x-goog-api-key",
			"x-csrf-token",
			"x-rapidapi-key",
			"dd-api-key",
			"private-token",
			"x-client-secret",
			"x-credential-id",
			"x-auth-key",
			"x-access-key",
			"x-amz-signature",
			"x-functions-key",
		] {
			let mut headers = HeaderMap::new();
			headers.insert(HeaderName::from_static(name), HeaderValue::from_static("v"));
			strip_credential_headers(&mut headers);
			assert!(headers.is_empty(), "{name} must be stripped");
		}
	}

	#[test]
	fn ordinary_headers_are_kept() {
		let names =
			["accept", "content-type", "user-agent", "x-api-version", "x-request-id", "x-trace-id"];
		let mut headers = HeaderMap::new();
		for name in names {
			headers.insert(HeaderName::from_static(name), HeaderValue::from_static("v"));
		}
		strip_credential_headers(&mut headers);
		let mut kept = headers.keys().map(HeaderName::as_str).collect::<Vec<_>>();
		kept.sort_unstable();
		assert_eq!(kept, names);
	}
}
