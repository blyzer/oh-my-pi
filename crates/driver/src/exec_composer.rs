//! Shared composition of remote execution requests and REAPI backends.
//!
//! Operator configuration determines the destination and credentials. Input
//! capture stays local; callers authorize the destination before connecting.

use std::{
	collections::BTreeMap, env, fs, future::Future, io, path::Path, sync::Arc, time::Duration,
};

use bytes::Bytes;
use omp_adw::{ApprovalScope, NetworkScope, Posture, WriteScope};
use omp_ai::auth::command::CommandCredentialResolver;
use omp_con::Ctx;
use omp_core::{Hash32, SecretString, Str};
use omp_env::{BlobDownloadEvent, EnvClient};
use omp_envd::CommandCredentialExecutorFactory as _;
use omp_exec::{
	BackendId, BuildCommand, BuildSystemAdapter, CachePolicy, CargoJust, Digest, EnvEntry,
	ExecutionError, ExecutionRequest, InputSet, Platform, ReattachHandle, RepoPins,
};
use omp_exec_reapi::{
	ExecuteConfig, ReapiBackend, Toolchain,
	cas::{BlobSource, InputSource},
	endpoint::Endpoint,
	profile::{Credential, Profile},
	tree::InputFile,
};
use omp_observability::attrs::omp_exec as exec_attrs;
use omp_proto::{
	blob::v1::{GetRequest, StatRequest},
	env::v1::CaptureInputsRequest,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::{bridges::CommandCredentials, cfg::CfgFiles, workspace_trust::evaluate_workspace};

omp_con::var! {
	/// Remote REAPI address. Empty disables remote execution.
	pub static SV_EXEC_REMOTE_ENDPOINT = sv_exec_remote_endpoint: Str {
		default: Str::default(), flags: archive,
	};
	/// Remote action-cache policy: `off`, `read`, or `read-write`.
	pub static SV_EXEC_REMOTE_CACHE = sv_exec_remote_cache: Str {
		default: Str::new_static("off"), flags: archive,
	};
	/// REAPI profile name: `reapi` or `buildbuddy`.
	pub static SV_EXEC_REMOTE_PROFILE = sv_exec_remote_profile: Str {
		default: Str::new_static("reapi"), flags: archive,
	};
	/// Command credential source, prefixed with `!`; never the key itself.
	pub static SV_EXEC_REMOTE_CREDENTIAL = sv_exec_remote_credential: Str {
		default: Str::default(), flags: archive,
	};
	/// Digest-pinned remote image for the repository's Cargo/just toolchain.
	pub static SV_EXEC_REMOTE_IMAGE = sv_exec_remote_image: Str {
		default: Str::default(), flags: archive,
	};
	/// Extra operator-approved upload deny patterns, comma separated.
	pub static SV_EXEC_UPLOAD_EXCLUDE = sv_exec_upload_exclude: Str {
		default: Str::default(), flags: archive,
	};
	/// Paths exempted by the operator from upload denial, comma separated.
	pub static SV_EXEC_UPLOAD_ALLOW = sv_exec_upload_allow: Str {
		default: Str::default(), flags: archive,
	};
}

/// Enforced by the Linux runner profile: the remote action cannot write the
/// local workspace and BuildBuddy's Firecracker worker has networking off.
pub const REMOTE_POSTURE: Posture = Posture {
	write:    WriteScope::ReadOnly,
	network:  NetworkScope::Disabled,
	approval: ApprovalScope::Yolo,
};
/// An error while composing or running remote execution.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
	/// The endpoint or image is not configured.
	#[error("remote execution is not configured with an endpoint and digest-pinned image")]
	MissingConfiguration,
	/// The environment credential is missing or empty.
	#[error("remote credential environment variable is missing or empty")]
	EnvironmentCredential,
	/// The credential source has an unsupported form.
	#[error("remote credential must be an env:NAME or !command source")]
	CredentialSource,
	/// The credential command failed.
	#[error("operator remote credential command failed")]
	Credential(#[source] omp_ai::auth::command::CommandCredentialError),
	/// The repository toolchain pins could not be read.
	#[error("repository toolchain pins could not be read")]
	Pins(#[source] io::Error),
	/// The build command could not be planned.
	#[error("CargoJust could not plan this command")]
	Plan(#[source] omp_exec::PlanError),
	/// The endpoint is invalid or insecure.
	#[error("remote endpoint is not secure")]
	Endpoint(#[source] omp_exec_reapi::endpoint::EndpointError),
	/// The environment could not capture inputs.
	#[error("project Environment input capture failed")]
	Capture(#[source] omp_env::ClientError),
	/// A tracked input was refused by an upload rule.
	#[error("tracked input was refused: {path} matched {rule}")]
	InputRefused {
		/// The refused path.
		path: Str,
		/// The rule that refused the path.
		rule: Str,
	},
	/// The environment capture manifest is invalid.
	#[error("invalid environment capture manifest")]
	Manifest,
	/// The remote execution failed.
	#[error("remote action failed: {0}")]
	Execute(#[source] ExecutionError),
	/// The remote action failed before acceptance; the error may permit local
	/// fallback (ADR 0040 Part C.3).
	#[error("remote action failed before acceptance: {0}")]
	PreAcceptanceFailed(#[source] ExecutionError),
	/// The environment could not materialize an artifact.
	#[error("remote artifact could not be materialized into the Environment blob store")]
	Materialize(#[source] omp_env::ClientError),
	/// The selected profile is not on the generic execution allowlist.
	#[error("the selected remote profile is not available to generic execution")]
	UnsupportedProfile,
	/// A reattach handle names a backend generic execution does not compose.
	#[error("the reattach handle names backend {backend}, which generic execution does not compose")]
	UnknownBackend {
		/// The backend the handle names.
		backend: BackendId,
	},
	/// The project state directory could not be prepared.
	#[error("project state directory could not be prepared")]
	StateDirectory(#[source] io::Error),
	/// The project Environment could not be opened.
	#[error("project Environment could not be opened")]
	Environment(#[source] omp_envd::EnvdError),
}

pub(crate) fn config_list(value: impl AsRef<str>) -> Vec<String> {
	value
		.as_ref()
		.split(",")
		.map(|part| part.trim())
		.filter(|part| !part.is_empty())
		.map(|part| part.to_string())
		.collect()
}
fn repo_pins(root: &Path) -> Result<RepoPins, io::Error> {
	let rust = fs::read_to_string(root.join("rust-toolchain.toml"))?;
	let ci = fs::read_to_string(root.join(".github/workflows/ci.yml"))?;
	let python = fs::read_to_string(root.join("crates/py/scripts/fetch-python.sh"))?;
	let channel = rust
		.lines()
		.find_map(|line| line.trim().strip_prefix("channel = "))
		.map(|value| value.trim_matches('"'));
	let nextest = ci
		.lines()
		.find_map(|line| line.trim().strip_prefix("tool: nextest@"));
	let tag = python.lines().find_map(|line| line.strip_prefix("TAG="));
	let version = python.lines().find_map(|line| line.strip_prefix("VER="));
	let (Some(channel), Some(nextest), Some(tag), Some(version)) = (channel, nextest, tag, version)
	else {
		return Err(io::Error::new(io::ErrorKind::InvalidData, "toolchain pins missing"));
	};
	Ok(RepoPins {
		rust_toolchain: Str::new(channel),
		nextest:        Str::new(nextest.trim()),
		cpython_tag:    Str::new(tag.trim()),
		cpython:        Str::new(version.trim()),
	})
}

/// Reads captured inputs from the project environment.
#[derive(Clone)]
pub struct EnvironmentInputs(EnvClient);

impl EnvironmentInputs {
	async fn bytes(&self, digest: Digest) -> io::Result<Bytes> {
		let download = self
			.0
			.blob_get(GetRequest {
				hash:   Bytes::copy_from_slice(digest.hash.as_bytes()),
				offset: 0,
				length: 0,
			})
			.await
			.map_err(io::Error::other)?;
		let mut output = Vec::new();
		download
			.write_verified(digest.hash, digest.size, digest.size, &mut output)
			.await
			.map_err(io::Error::other)?;
		Ok(Bytes::from(output))
	}
}

impl BlobSource for EnvironmentInputs {
	fn read(&self, digest: Digest) -> impl Future<Output = io::Result<Bytes>> + Send + '_ {
		self.bytes(digest)
	}

	fn read_range(
		&self,
		digest: Digest,
		offset: u64,
		len: u64,
	) -> impl Future<Output = io::Result<Bytes>> + Send + '_ {
		async move {
			if offset > digest.size || len > digest.size - offset {
				return Err(io::Error::new(io::ErrorKind::InvalidInput, "blob range exceeds size"));
			}
			let mut download = self
				.0
				.blob_get(GetRequest {
					hash: Bytes::copy_from_slice(digest.hash.as_bytes()),
					offset,
					length: len,
				})
				.await
				.map_err(io::Error::other)?;
			let mut output = Vec::with_capacity(len.min(64 * 1024) as usize);
			let mut completed = false;
			while let Some(event) = download.next_event().await.map_err(io::Error::other)? {
				match event {
					BlobDownloadEvent::Chunk(chunk) => {
						if !chunk.hash.is_empty() && chunk.hash.as_ref() != digest.hash.as_bytes()
							|| chunk.size.is_some_and(|size| size != digest.size)
							|| output.len().saturating_add(chunk.data.len()) as u64 > len
						{
							return Err(io::Error::new(
								io::ErrorKind::InvalidData,
								"blob range metadata mismatch",
							));
						}
						output.extend_from_slice(&chunk.data);
					},
					BlobDownloadEvent::Complete(_) => {
						completed = true;
						break;
					},
				}
			}
			if !completed || output.len() as u64 != len {
				return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "blob range incomplete"));
			}
			Ok(Bytes::from(output))
		}
	}
}

impl InputSource for EnvironmentInputs {
	fn files(
		&self,
		manifest: Digest,
	) -> impl Future<Output = io::Result<Vec<InputFile>>> + Send + '_ {
		async move {
			let bytes = self.bytes(manifest).await?;
			if !bytes.starts_with(b"OMPWS2\0\0\0\0\0") {
				return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid capture manifest"));
			}
			let mut offset = 11usize;
			let mut files = Vec::new();
			while offset < bytes.len() {
				let count = bytes.get(offset..offset + 4).ok_or_else(|| {
					io::Error::new(io::ErrorKind::InvalidData, "truncated path length")
				})?;
				let length =
					u32::from_be_bytes(count.try_into().map_err(|_| io::ErrorKind::InvalidData)?)
						as usize;
				offset += 4;
				let path = bytes
					.get(offset..offset + length)
					.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated path"))?;
				let path = std::str::from_utf8(path)
					.map_err(|_| io::ErrorKind::InvalidData)?
					.to_owned();
				offset += length;
				let mode = bytes
					.get(offset..offset + 4)
					.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated file mode"))?;
				let executable =
					u32::from_be_bytes(mode.try_into().map_err(|_| io::ErrorKind::InvalidData)?) & 0o111
						!= 0;
				offset += 4;
				let hash = bytes.get(offset..offset + 32).ok_or_else(|| {
					io::Error::new(io::ErrorKind::InvalidData, "truncated file digest")
				})?;
				let hash: [u8; 32] = hash.try_into().map_err(|_| io::ErrorKind::InvalidData)?;
				offset += 32;
				let stat = self
					.0
					.blob_stat(StatRequest { hash: Bytes::copy_from_slice(&hash) })
					.await
					.map_err(io::Error::other)?;
				if !stat.present {
					return Err(io::Error::new(io::ErrorKind::NotFound, "capture blob missing"));
				}
				files.push(InputFile {
					path,
					digest: Digest::new(Hash32::new(hash), stat.size),
					executable,
				});
			}
			Ok(files)
		}
	}
}
/// A non-secret destination suitable for persisting with an execution handle.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestinationIdentity {
	/// The backend identifier used in execution handles.
	pub backend:  BackendId,
	/// The normalized transport URL, including scheme, host and port.
	pub endpoint: Str,
	/// The REAPI instance name.
	pub instance: Str,
}

/// A snapshot of operator-controlled execution configuration.
pub struct ExecutionSettings {
	endpoint_text:     Str,
	endpoint:          Endpoint,
	image:             Str,
	credential_source: Str,
	profile:           Str,
	cache:             Str,
	deny_globs:        Str,
	allow_paths:       Str,
}

/// A planned request with locally captured inputs and its repository pins.
pub struct PreparedExecution {
	/// The request to submit after destination approval.
	pub request: ExecutionRequest,
	/// The toolchain pins used to plan and configure the backend.
	pub pins:    RepoPins,
}

const BACKEND_NAME: &str = "remote";
const INSTANCE: &str = "";

fn generic_profile(name: &str) -> Result<Profile, RemoteError> {
	match name {
		"reapi" => Ok(Profile::reapi()),
		_ => Err(RemoteError::UnsupportedProfile),
	}
}

impl ExecutionSettings {
	/// Reads and validates operator settings for generic REAPI execution.
	///
	/// # Errors
	///
	/// Returns an error for invalid settings or a vendor-specific profile.
	pub fn generic(ctx: &Ctx) -> Result<Self, RemoteError> {
		let profile = SV_EXEC_REMOTE_PROFILE.get(ctx);
		generic_profile(&profile)?;
		Self::snapshot(ctx, profile)
	}

	pub(crate) fn for_adw(ctx: &Ctx) -> Result<Self, RemoteError> {
		Self::snapshot(ctx, SV_EXEC_REMOTE_PROFILE.get(ctx))
	}

	fn snapshot(ctx: &Ctx, profile: Str) -> Result<Self, RemoteError> {
		let endpoint_text = SV_EXEC_REMOTE_ENDPOINT.get(ctx);
		let image = SV_EXEC_REMOTE_IMAGE.get(ctx);
		if endpoint_text.is_empty() || image.is_empty() || !image.contains("@sha256:") {
			return Err(RemoteError::MissingConfiguration);
		}
		let endpoint = Endpoint::parse(&endpoint_text).map_err(RemoteError::Endpoint)?;
		Ok(Self {
			endpoint_text,
			endpoint,
			image,
			credential_source: SV_EXEC_REMOTE_CREDENTIAL.get(ctx),
			profile,
			cache: SV_EXEC_REMOTE_CACHE.get(ctx),
			deny_globs: SV_EXEC_UPLOAD_EXCLUDE.get(ctx),
			allow_paths: SV_EXEC_UPLOAD_ALLOW.get(ctx),
		})
	}

	fn generic_profile(&self) -> Result<(Profile, CachePolicy), RemoteError> {
		Ok((generic_profile(&self.profile)?, self.cache.parse().unwrap_or_default()))
	}

	pub(crate) fn profile_name(&self) -> &str {
		&self.profile
	}

	pub(crate) fn cache_name(&self) -> &str {
		&self.cache
	}

	pub(crate) fn endpoint_text(&self) -> &str {
		&self.endpoint_text
	}

	/// Returns the destination identity without credentials or action inputs.
	pub fn destination_identity(&self) -> DestinationIdentity {
		DestinationIdentity {
			backend:  BackendId::new_static(BACKEND_NAME),
			endpoint: Str::from(self.endpoint.transport_url()),
			instance: Str::new_static(INSTANCE),
		}
	}

	/// Resolves the configured operator credential without connecting to REAPI.
	///
	/// # Errors
	///
	/// Returns an error when the credential source or command fails.
	pub async fn credential(
		&self,
		environment: &EnvClient,
		root: &Path,
		cancel: &CancellationToken,
	) -> Result<Credential, RemoteError> {
		if let Some(credential) = self.environment_credential()? {
			return Ok(credential);
		}
		let command = self
			.credential_source
			.strip_prefix("!")
			.ok_or(RemoteError::CredentialSource)?;
		let executor = CommandCredentials.make(environment.clone(), root);
		let secret = CommandCredentialResolver::new(executor, Duration::from_secs(1))
			.resolve(&command, cancel.clone())
			.await
			.map_err(RemoteError::Credential)?;
		Ok(Credential::new(secret))
	}

	fn environment_credential(&self) -> Result<Option<Credential>, RemoteError> {
		if let Some(name) = self.credential_source.strip_prefix("env:") {
			let value = env::var(name)
				.ok()
				.filter(|value| !value.trim().is_empty())
				.ok_or(RemoteError::EnvironmentCredential)?;
			Ok(Some(Credential::new(SecretString::from(value))))
		} else if self.credential_source.starts_with('!') {
			Ok(None)
		} else {
			Err(RemoteError::CredentialSource)
		}
	}

	/// Plans the command and captures its inputs without contacting the
	/// destination.
	///
	/// # Errors
	///
	/// Returns an error when planning or environment capture fails.
	pub async fn prepare(
		&self,
		environment: &EnvClient,
		root: &Path,
		command: &BuildCommand,
	) -> Result<PreparedExecution, RemoteError> {
		let pins = repo_pins(root).map_err(RemoteError::Pins)?;
		let plan = CargoJust::default()
			.plan(command, &pins)
			.map_err(RemoteError::Plan)?;
		let mut request = plan.requests.first().ok_or(RemoteError::Manifest)?.clone();
		if !request.argv.is_empty() {
			let mut argv = request.argv.to_vec();
			argv[0] = Str::new_static("/usr/local/bin/just");
			request.argv = Arc::from(argv);
		}
		let capture = environment
			.capture_inputs(CaptureInputsRequest {
				include_untracked: true,
				deny_globs: config_list(&self.deny_globs),
				allow_paths: config_list(&self.allow_paths),
				wire_revision: omp_proto::SCHEMA_REV,
				..Default::default()
			})
			.instrument(tracing::info_span!("omp.exec.capture"))
			.await
			.map_err(RemoteError::Capture)?;
		tracing::info!(
			{ exec_attrs::INPUT_FILES } = capture.files,
			{ exec_attrs::INPUT_BYTES } = capture.bytes,
			"capture complete"
		);
		if let Some(refused) = capture.refused.first() {
			return Err(RemoteError::InputRefused {
				path: Str::new(&refused.path),
				rule: Str::new(&refused.rule),
			});
		}
		let hash: [u8; 32] = capture
			.manifest_hash
			.as_ref()
			.try_into()
			.map_err(|_| RemoteError::Manifest)?;
		request.inputs = Some(InputSet {
			manifest: Digest::new(Hash32::new(hash), capture.manifest_size),
			files:    capture.files,
			bytes:    capture.bytes,
			excluded: omp_exec::ExclusionCounts {
				rules:    Arc::from([]),
				symlinks: capture.excluded_symlinks,
			},
		});
		Ok(PreparedExecution { request, pins })
	}

	/// Builds the backend configuration for the validated destination.
	///
	/// This is the single vendor seam: generic callers pass the allowlisted
	/// profile to [`Self::connect`], while ADW passes its operator-selected
	/// profile here and attaches vendor-only fields before
	/// [`Self::connect_config`].
	pub(crate) fn config(
		&self,
		pins: &RepoPins,
		profile: Profile,
		cache: CachePolicy,
		credential: Credential,
	) -> ExecuteConfig {
		let mut config = resume_config(profile, cache, credential);
		config.toolchains =
			BTreeMap::from([(CargoJust::toolchain(pins).to_string().into(), Toolchain {
				image: Some(self.image.clone()),
				env:   Arc::from([EnvEntry {
					name:  Str::new_static("PATH"),
					value: Str::new_static("/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"),
				}]),
			})]);
		config
	}

	/// Connects the generic backend after the caller authorizes the destination.
	///
	/// # Errors
	///
	/// Returns a typed profile, transport, authentication, or capability error.
	pub async fn connect(
		&self,
		environment: EnvClient,
		pins: &RepoPins,
		credential: Credential,
	) -> Result<ReapiBackend<EnvironmentInputs>, RemoteError> {
		let (profile, cache) = self.generic_profile()?;
		self
			.connect_config(environment, self.config(pins, profile, cache, credential))
			.await
	}

	pub(crate) async fn connect_config(
		&self,
		environment: EnvClient,
		config: ExecuteConfig,
	) -> Result<ReapiBackend<EnvironmentInputs>, RemoteError> {
		ReapiBackend::connect(self.endpoint.clone(), config, EnvironmentInputs(environment))
			.await
			.map_err(RemoteError::Execute)
	}
}

/// The configuration of a backend that only resumes accepted work.
///
/// A resume plans and uploads nothing, so it maps no toolchain to an image.
fn resume_config(profile: Profile, cache: CachePolicy, credential: Credential) -> ExecuteConfig {
	ExecuteConfig {
		id: BackendId::new_static(BACKEND_NAME),
		instance: Str::new_static(INSTANCE),
		profile,
		credential: Some(credential),
		platform: Platform::LINUX_X86_64,
		enforced: REMOTE_POSTURE,
		cache,
		platform_properties: BTreeMap::new(),
		toolchains: BTreeMap::new(),
		buildbuddy_canceller: None,
	}
}

/// Input source for a resume, which never uploads a new request.
#[derive(Clone)]
pub struct ResumeInputs;

impl BlobSource for ResumeInputs {
	fn read(&self, _digest: Digest) -> impl Future<Output = io::Result<Bytes>> + Send + '_ {
		std::future::ready(Err(io::Error::other("resume has no input blobs")))
	}

	fn read_range(
		&self,
		_digest: Digest,
		_offset: u64,
		_len: u64,
	) -> impl Future<Output = io::Result<Bytes>> + Send + '_ {
		std::future::ready(Err(io::Error::other("resume has no input blobs")))
	}
}

impl InputSource for ResumeInputs {
	fn files(
		&self,
		_manifest: Digest,
	) -> impl Future<Output = io::Result<Vec<InputFile>>> + Send + '_ {
		std::future::ready(Err(io::Error::other("resume has no input manifest")))
	}
}

/// A backend connected to resume work the operator's destination accepted.
///
/// A command credential retains its project Environment until the run ends.
pub struct ResumeConnection {
	backend:      ReapiBackend<ResumeInputs>,
	_environment: Option<omp_envd::ProjectEnvironment>,
}

impl ResumeConnection {
	/// Connects the backend `handle` names at the configured destination.
	///
	/// Call it only after the job's journaled destination matched
	/// [`ExecutionSettings::destination_identity`]: this is the first contact
	/// with the destination. Command credentials resolve in the project
	/// Environment at `root`, under the workspace trust stored in `data_dir`.
	/// Environment credentials do not start a project Environment. A daemon
	/// spawned for a command credential uses a two-second idle timeout.
	///
	/// # Errors
	///
	/// Refuses a handle that names another backend before anything starts, and
	/// returns the profile, Environment, credential, or connection failure.
	pub async fn open(
		settings: &ExecutionSettings,
		handle: &ReattachHandle,
		ctx: Arc<Ctx>,
		root: &Path,
		data_dir: &Path,
		cancel: &CancellationToken,
	) -> Result<Self, RemoteError> {
		if handle.backend.as_str() != BACKEND_NAME {
			return Err(RemoteError::UnknownBackend { backend: handle.backend.clone() });
		}
		let (profile, cache) = settings.generic_profile()?;
		let (credential, environment) = if let Some(credential) = settings.environment_credential()? {
			(credential, None)
		} else {
			let state_dir = omp_env::project_state::directory(data_dir, root)
				.map_err(RemoteError::StateDirectory)?;
			fs::create_dir_all(&state_dir).map_err(RemoteError::StateDirectory)?;
			// No kernel is composed here, so the workspace's own stored decision
			// applies, failing closed when none was made.
			let workspace_trust = evaluate_workspace(data_dir, root).trust();
			let environment =
				omp_envd::ProjectEnvironment::attach(root, &state_dir, omp_envd::AttachOptions {
					py_eval:            false,
					approval_mode:      None,
					trusted_extensions: Vec::new(),
					contributed_values: Vec::new(),
					con:                ctx,
					bridges:            omp_envd::RegistryBridges {
						content: omp_envd::ActiveContentInputs {
							workspace_trust,
							..omp_envd::ActiveContentInputs::default()
						},
						..omp_envd::RegistryBridges::default()
					},
					spawn_idle_timeout: Some(2),
					spawn_policy:       CfgFiles::new(Some(root))
						.ok()
						.and_then(|files| files.daemon_policy(workspace_trust).ok()),
				})
				.await
				.map_err(RemoteError::Environment)?;
			let credential = settings
				.credential(environment.client(), root, cancel)
				.await?;
			(credential, Some(environment))
		};
		let backend = ReapiBackend::connect(
			settings.endpoint.clone(),
			resume_config(profile, cache, credential),
			ResumeInputs,
		)
		.await
		.map_err(RemoteError::Execute)?;
		Ok(Self { backend, _environment: environment })
	}

	/// The connected backend.
	#[must_use]
	pub const fn backend(&self) -> &ReapiBackend<ResumeInputs> {
		&self.backend
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn context(endpoint: &str) -> Ctx {
		let ctx = Ctx::new();
		SV_EXEC_REMOTE_ENDPOINT
			.set(&ctx, Str::new(endpoint))
			.unwrap();
		SV_EXEC_REMOTE_IMAGE
			.set(&ctx, Str::new_static("image@sha256:abc"))
			.unwrap();
		ctx
	}

	fn identity(endpoint: &str) -> DestinationIdentity {
		ExecutionSettings::generic(&context(endpoint))
			.unwrap()
			.destination_identity()
	}

	fn pins() -> RepoPins {
		RepoPins {
			rust_toolchain: Str::new_static("1.90.0"),
			nextest:        Str::new_static("0.9.100"),
			cpython_tag:    Str::new_static("20250101"),
			cpython:        Str::new_static("3.13.1"),
		}
	}

	#[test]
	fn generic_composer_allowlists_only_reapi() {
		for profile in ["buildbuddy", "other", ""] {
			let ctx = context("grpcs://example.com");
			SV_EXEC_REMOTE_PROFILE
				.set(&ctx, Str::new_static(profile))
				.unwrap();
			assert!(
				matches!(ExecutionSettings::generic(&ctx), Err(RemoteError::UnsupportedProfile)),
				"{profile} was not refused",
			);
		}
		let ctx = context("grpcs://example.com");
		SV_EXEC_REMOTE_PROFILE
			.set(&ctx, Str::new_static("reapi"))
			.unwrap();
		SV_EXEC_REMOTE_CACHE
			.set(&ctx, Str::new_static("READ"))
			.unwrap();
		let (profile, cache) = ExecutionSettings::generic(&ctx)
			.unwrap()
			.generic_profile()
			.unwrap();
		assert_eq!(profile, Profile::reapi());
		assert_eq!(cache, CachePolicy::Read);
	}

	#[test]
	fn config_carries_the_validated_destination_and_pinned_toolchain() {
		let settings = ExecutionSettings::generic(&context("grpcs://example.com")).unwrap();
		let pins = pins();
		let config =
			settings.config(&pins, Profile::reapi(), CachePolicy::ReadWrite, Credential::new("token"));
		assert_eq!(config.id.as_str(), "remote");
		assert_eq!(config.instance.as_str(), "");
		assert_eq!(config.instance, settings.destination_identity().instance);
		assert_eq!(config.profile, Profile::reapi());
		assert_eq!(config.credential.as_ref().map(Credential::expose), Some("token"));
		assert_eq!(config.platform, Platform::LINUX_X86_64);
		assert_eq!(config.enforced, REMOTE_POSTURE);
		assert_eq!(config.cache, CachePolicy::ReadWrite);
		assert!(config.platform_properties.is_empty());
		assert_eq!(config.toolchains.len(), 1);
		let toolchain = &config.toolchains[CargoJust::toolchain(&pins).to_string().as_str()];
		assert_eq!(toolchain.image.as_deref(), Some("image@sha256:abc"));
		assert_eq!(&*toolchain.env, [EnvEntry {
			name:  Str::new_static("PATH"),
			value: Str::new_static("/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"),
		}]);
		assert!(config.buildbuddy_canceller.is_none());
	}

	#[test]
	fn destination_identity_is_stable_and_distinguishes_endpoints() {
		let first = identity("grpcs://example.com");
		let same = identity("grpcs://example.com");
		let equivalent = identity("grpcs://example.com:443");
		let other = identity("grpcs://other.example.com");
		assert_eq!(first, same);
		assert_eq!(first, equivalent);
		assert_ne!(first, other);
		assert_ne!(identity("grpcs://example.com:8443"), identity("grpcs://example.com"));
		assert_ne!(identity("grpc://127.0.0.1:8980"), identity("grpcs://127.0.0.1:8980"));
		assert_eq!(first.backend.as_str(), "remote");
		assert_eq!(first.instance.as_str(), "");
		let json = serde_json::to_string(&first).unwrap();
		assert_eq!(serde_json::from_str::<DestinationIdentity>(&json).unwrap(), first);
	}

	#[test]
	fn resume_config_names_the_destination_and_pins_no_toolchain() {
		let settings = ExecutionSettings::generic(&context("grpcs://example.com")).unwrap();
		let config = resume_config(Profile::reapi(), CachePolicy::Read, Credential::new("token"));
		assert_eq!(config.id, settings.destination_identity().backend);
		assert_eq!(config.instance, settings.destination_identity().instance);
		assert_eq!(config.profile, Profile::reapi());
		assert_eq!(config.credential.as_ref().map(Credential::expose), Some("token"));
		assert_eq!(config.platform, Platform::LINUX_X86_64);
		assert_eq!(config.enforced, REMOTE_POSTURE);
		assert_eq!(config.cache, CachePolicy::Read);
		assert!(config.platform_properties.is_empty());
		assert!(config.toolchains.is_empty());
		assert!(config.buildbuddy_canceller.is_none());
	}

	#[tokio::test]
	async fn resume_refuses_a_handle_for_another_backend_before_opening_anything() {
		let settings = ExecutionSettings::generic(&context("grpcs://example.com")).unwrap();
		let handle = ReattachHandle::new(
			BackendId::new_static("other"),
			Str::new_static("operations/1"),
			omp_exec::ExecutionId::new_static("exec-1"),
		);
		let missing = Path::new("/nonexistent/omp-resume");
		let opened = ResumeConnection::open(
			&settings,
			&handle,
			Arc::new(Ctx::new()),
			missing,
			missing,
			&CancellationToken::new(),
		)
		.await;
		assert!(
			matches!(&opened, Err(RemoteError::UnknownBackend { backend }) if backend.as_str() == "other")
		);
	}

	#[tokio::test]
	async fn resume_environment_credential_does_not_start_project_environment() {
		let temp = tempfile::tempdir().unwrap();
		let data_dir = temp.path().join("uncreated-data");
		let mut settings = ExecutionSettings::generic(&context("grpc://127.0.0.1:1")).unwrap();
		settings.credential_source = Str::new_static("env:PATH");
		let handle = ReattachHandle::new(
			BackendId::new_static("remote"),
			Str::new_static("operations/1"),
			omp_exec::ExecutionId::new_static("exec-1"),
		);
		let opened = tokio::time::timeout(
			Duration::from_secs(5),
			ResumeConnection::open(
				&settings,
				&handle,
				Arc::new(Ctx::new()),
				temp.path(),
				&data_dir,
				&CancellationToken::new(),
			),
		)
		.await
		.expect("connection attempt is bounded");
		assert!(opened.is_err());
		assert!(!data_dir.exists(), "environment credential must not start an environment");
		settings.credential_source = Str::new_static("!printf token");
		assert!(settings.environment_credential().unwrap().is_none());
	}
}
