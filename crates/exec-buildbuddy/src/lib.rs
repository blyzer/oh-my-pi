//! BuildBuddy's REST Run API backend for git-backed remote verification.
//!
//! This is distinct from `omp-exec-reapi`: Run clones a repository and applies
//! captured git patches, while REAPI uploads the complete content-addressed
//! tree.

use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode, Uri, header};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
	client::legacy::{Client, connect::HttpConnector},
	rt::TokioExecutor,
};
use omp_adw::Posture;
use omp_core::{CowBytes, ExposeSecret, SecretString, Str, base64};
use omp_exec::{
	BackendCapabilities, BackendId, CacheSupport, CancelSupport, ExecutionBackend, ExecutionError,
	ExecutionEvent, ExecutionRequest, ExecutionRun, NeedSet, Platform, Reattach,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

mod run;
#[cfg(test)]
mod tests;

#[cfg(feature = "buildbuddy-cancel")]
use omp_exec_reapi::BuildBuddyCanceller;
pub use run::BuildBuddyRun;

const MAX_PATCH_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;

type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// Repository identity and a security-filtered working-tree patch, prepared by
/// the trusted environment capture authority, not by project scripts.
#[derive(Clone, Debug)]
pub struct RunInput {
	/// Operator-authorized repository URL.
	pub repo:       Str,
	/// Pinned Git commit used as the base of `patches`.
	pub commit_sha: Str,
	/// Branch, for runner reuse and invocation metadata.
	pub branch:     Option<Str>,
	/// Git patches for this exact commit, ordered before execution.
	pub patches:    Arc<[Bytes]>,
}

/// Supplies a captured input without giving the network backend filesystem
/// authority.
pub trait RunInputSource: Send + Sync + 'static {
	/// Resolves a request to an authorized repository and captured patch.
	fn prepare(
		&self,
		request: &ExecutionRequest,
	) -> impl Future<Output = Result<RunInput, PreparationError>> + Send + '_;
}

/// One already captured input; useful for an operator-run command or a test.
#[derive(Clone)]
pub struct PreparedInputSource(pub RunInput);

impl RunInputSource for PreparedInputSource {
	fn prepare(
		&self,
		_request: &ExecutionRequest,
	) -> impl Future<Output = Result<RunInput, PreparationError>> + Send + '_ {
		std::future::ready(Ok(self.0.clone()))
	}
}

/// Capture authority failed before any remote work was accepted.
#[derive(Debug, Error)]
pub enum PreparationError {
	/// The capture's underlying failure.
	#[error("could not prepare captured Run API input")]
	Source(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Operator-owned Run API configuration.
#[derive(Clone)]
pub struct Config {
	/// Name used in settled records and reattach handles.
	pub id:                  BackendId,
	/// API origin, e.g. `https://app.buildbuddy.io`; HTTP is loopback-only.
	pub endpoint:            Str,
	/// API key; never sent in the action environment or URL.
	pub credential:          SecretString,
	/// Supported platforms.
	pub platforms:           Arc<[Platform]>,
	/// Posture that the runner actually enforces.
	pub enforced:            Posture,
	/// Runner properties configured by the operator.
	pub platform_properties: BTreeMap<Str, Str>,
	/// Vendor invocation cancellation, if configured.
	#[cfg(feature = "buildbuddy-cancel")]
	pub canceller:           Option<BuildBuddyCanceller>,
}

/// A connected Run API client; construction validates the destination before
/// the key can be transmitted.
pub struct BuildBuddyBackend<S> {
	config:       Arc<Config>,
	client:       HttpClient,
	capabilities: BackendCapabilities,
	source:       Arc<S>,
}

impl<S> Clone for BuildBuddyBackend<S> {
	fn clone(&self) -> Self {
		Self {
			config:       Arc::clone(&self.config),
			client:       self.client.clone(),
			capabilities: self.capabilities.clone(),
			source:       Arc::clone(&self.source),
		}
	}
}

impl<S: RunInputSource> BuildBuddyBackend<S> {
	/// Builds the client after validating TLS and its API origin.
	///
	/// # Errors
	/// Refuses non-loopback plaintext, credentials in URLs, or a URL with a
	/// path.
	pub fn new(config: Config, source: S) -> Result<Self, ApiError> {
		validate_origin(&config.endpoint)?;
		let _ = rustls::crypto::ring::default_provider().install_default();
		let connector = HttpsConnectorBuilder::new()
			.with_webpki_roots()
			.https_or_http()
			.enable_http1()
			.build();
		let client = Client::builder(TokioExecutor::new()).build(connector);
		let capabilities = BackendCapabilities {
			platforms:      Arc::clone(&config.platforms),
			needs:          NeedSet::new(),
			max_timeout:    None,
			action_cache:   CacheSupport::None,
			cancel:         if cfg!(feature = "buildbuddy-cancel") {
				#[cfg(feature = "buildbuddy-cancel")]
				{
					if config.canceller.is_some() {
						CancelSupport::BestEffort
					} else {
						CancelSupport::None
					}
				}
				#[cfg(not(feature = "buildbuddy-cancel"))]
				{
					CancelSupport::None
				}
			} else {
				CancelSupport::None
			},
			max_blob_bytes: Some(MAX_PATCH_BYTES as u64),
			warm_caches:    false,
			enforced:       config.enforced,
		};
		Ok(Self { config: Arc::new(config), client, capabilities, source: Arc::new(source) })
	}

	async fn submit(&self, input: &RunInput, request: &ExecutionRequest) -> Result<Str, ApiError> {
		if input.repo.is_empty() || input.commit_sha.is_empty() {
			return Err(ApiError::MissingRepositoryIdentity);
		}
		let size: usize = input.patches.iter().map(Bytes::len).sum();
		if size > MAX_PATCH_BYTES {
			return Err(ApiError::PatchTooLarge);
		}
		let command = shell_command(&request.argv, &request.cwd)?;
		let patches: Vec<String> = input
			.patches
			.iter()
			.map(|p| base64::encode(p).into_string())
			.collect();
		let payload = RunRequest {
			repo: &input.repo,
			branch: input.branch.as_deref(),
			commit_sha: &input.commit_sha,
			patches,
			steps: [Step { run: &command }],
			platform_properties: &self.config.platform_properties,
			timeout: format!("{}s", request.timeout.as_secs().max(1)),
			wait_until: "STARTED",
		};
		let response: RunResponse = post(&self.client, &self.config, "Run", &payload).await?;
		if response.invocation_id.is_empty() {
			return Err(ApiError::MissingInvocation);
		}
		Ok(Str::from(response.invocation_id))
	}
}

async fn post<T: Serialize, R: DeserializeOwned>(
	client: &HttpClient,
	config: &Config,
	operation: &str,
	payload: &T,
) -> Result<R, ApiError> {
	let uri: Uri =
		format!("{}/api/v1/{operation}", config.endpoint.trim_end_matches('/')).parse()?;
	let mut key = header::HeaderValue::from_str(config.credential.expose_secret())?;
	key.set_sensitive(true);
	let body = Bytes::from(serde_json::to_vec(payload)?);
	let request = Request::builder()
		.method(Method::POST)
		.uri(uri)
		.header(header::CONTENT_TYPE, "application/json")
		.header("x-buildbuddy-api-key", key)
		.body(Full::new(body))?;
	let response = client.request(request).await?;
	if !response.status().is_success() {
		return Err(ApiError::Status(response.status()));
	}
	let mut response_body = response.into_body();
	let mut bytes = Vec::new();
	while let Some(frame) = response_body.frame().await {
		if let Ok(data) = frame?.into_data() {
			if bytes.len().saturating_add(data.len()) > MAX_RESPONSE_BYTES {
				return Err(ApiError::ResponseTooLarge);
			}
			bytes.extend_from_slice(&data);
		}
	}
	Ok(serde_json::from_slice(&bytes)?)
}

impl<S: RunInputSource> ExecutionBackend for BuildBuddyBackend<S> {
	type Run = BuildBuddyRun;

	fn id(&self) -> &BackendId<str> {
		&self.config.id
	}

	fn capabilities(&self) -> &BackendCapabilities {
		&self.capabilities
	}

	fn start(
		&self,
		request: ExecutionRequest,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		async move {
			let input = self
				.source
				.prepare(&request)
				.await
				.map_err(|source| ExecutionError::InputCapture { source: Arc::new(source) })?;
			let invocation = self
				.submit(&input, &request)
				.await
				.map_err(|source| ExecutionError::Transport { source: Arc::new(source) })?;
			Ok(BuildBuddyRun::new(
				self.config.clone(),
				self.client.clone(),
				invocation,
				request.id,
				request.timeout,
			))
		}
	}

	fn reattach(
		&self,
		handle: Reattach,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		async move {
			if handle.backend != self.config.id || handle.operation.is_empty() {
				return Err(ExecutionError::Config { source: Arc::new(ApiError::InvalidHandle) });
			}
			Ok(BuildBuddyRun::new(
				self.config.clone(),
				self.client.clone(),
				handle.operation,
				handle.id,
				Duration::from_secs(1800),
			))
		}
	}
}

fn validate_origin(origin: &str) -> Result<(), ApiError> {
	let uri: Uri = origin.parse()?;
	let authority = uri.authority().ok_or(ApiError::InvalidEndpoint)?;
	if uri.path() != "" && uri.path() != "/"
		|| uri.query().is_some()
		|| authority.as_str().contains('@')
	{
		return Err(ApiError::InvalidEndpoint);
	}
	match uri.scheme_str() {
		Some("https") => Ok(()),
		Some("http") if matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]") => Ok(()),
		_ => Err(ApiError::InvalidEndpoint),
	}
}

fn shell_command(argv: &[Str], cwd: &Str) -> Result<String, ApiError> {
	if argv.is_empty()
		|| argv[0].contains('/')
		|| argv[0].is_empty()
		|| cwd.starts_with('/')
		|| cwd.split('/').any(|part| part == "..")
	{
		return Err(ApiError::InvalidCommand);
	}
	let mut command = String::new();
	if !cwd.is_empty() {
		command.push_str("cd -- ");
		command.push_str(&quote(cwd));
		command.push_str(" && ");
	}
	for (index, arg) in argv.iter().enumerate() {
		if index > 0 {
			command.push(' ');
		}
		command.push_str(&quote(arg));
	}
	Ok(command)
}

fn quote(value: &str) -> String {
	let mut quoted = String::from("'");
	for character in value.chars() {
		if character == '\'' {
			quoted.push_str("'\\''");
		} else {
			quoted.push(character);
		}
	}
	quoted.push('\'');
	quoted
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunRequest<'a> {
	repo:                &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	branch:              Option<&'a str>,
	commit_sha:          &'a str,
	patches:             Vec<String>,
	steps:               [Step<'a>; 1],
	platform_properties: &'a BTreeMap<Str, Str>,
	timeout:             String,
	wait_until:          &'static str,
}

#[derive(Serialize)]
struct Step<'a> {
	run: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunResponse {
	invocation_id: String,
}

/// A typed REST transport, protocol or request error. No response body or key
/// is included in diagnostics.
#[derive(Debug, Error)]
pub enum ApiError {
	/// API origin is not an HTTPS origin (or a loopback HTTP test origin).
	#[error("invalid or unsafe Run API origin")]
	InvalidEndpoint,
	/// URI syntax is invalid.
	#[error("invalid Run API URI")]
	Uri(#[from] hyper::http::uri::InvalidUri),
	/// An HTTP header is invalid.
	#[error("invalid Run API header")]
	Header(#[from] hyper::http::header::InvalidHeaderValue),
	/// Request construction failed.
	#[error("could not build Run API request")]
	Request(#[from] hyper::http::Error),
	/// An HTTP connection failed.
	#[error("Run API request failed")]
	Transport(#[from] hyper_util::client::legacy::Error),
	/// Response body failed while streaming.
	#[error("Run API response body failed")]
	Body(#[from] hyper::Error),
	/// JSON encoding or decoding failed.
	#[error("Run API JSON was invalid")]
	Json(#[from] serde_json::Error),
	/// Server returned an unsuccessful HTTP status.
	#[error("Run API returned HTTP {0}")]
	Status(StatusCode),
	/// Response exceeded the bounded JSON limit.
	#[error("Run API JSON response exceeded the size limit")]
	ResponseTooLarge,
	/// Patch payload exceeded the maximum size.
	#[error("captured git patch exceeds the Run API limit")]
	PatchTooLarge,
	/// No pinned commit and repository was provided.
	#[error("Run API requires a pinned repository commit")]
	MissingRepositoryIdentity,
	/// An unsafe command path or empty argv was requested.
	#[error("Run API command is not a safe relative argv")]
	InvalidCommand,
	/// Server accepted a run without naming it.
	#[error("Run API returned no invocation id")]
	MissingInvocation,
	/// Reattach handle does not belong to this backend.
	#[error("Run API reattach handle is invalid")]
	InvalidHandle,
	/// Invocation ended without a complete verdict.
	#[error("Run API invocation ended without a complete verdict")]
	IncompleteInvocation,
	/// Log pages exceed the bounded output size.
	#[error("Run API log exceeded the size limit")]
	LogTooLarge,
}
