//! Operator-selected remote-only placement for ADW Cargo/just code phases.
//!
//! The Environment owns capture and blobs; the driver owns the REAPI network
//! service. No project cfg can choose a remote endpoint or credential.

use std::{collections::BTreeMap, env, fs, future::Future, io, path::Path, sync::Arc, time::Duration};

use bytes::Bytes;
use omp_adw::{ApprovalScope, NetworkScope, Phase, Posture, WriteScope};
use omp_ai::auth::command::CommandCredentialResolver;
use omp_con::Ctx;
use omp_core::{Hash32, SecretString, Str, StrMut};
use omp_env::{BlobDownloadEvent, EnvClient};
use omp_envd::CommandCredentialExecutorFactory as _;
use omp_exec::{
	Artifact, BackendId, BuildCommand, BuildSystemAdapter, CargoJust, Digest, Eligibility, EnvEntry,
	ExecutionBackend, ExecutionError, ExecutionEvent, ExecutionId, ExecutionRun, InputSet, Intent,
	Platform, RepoPins,
};
use omp_exec_reapi::{
	ExecuteConfig, ReapiBackend, Toolchain,
	cas::{BlobSource, InputSource},
	endpoint::Endpoint,
	profile::{Credential, Profile},
	tree::InputFile,
};
use omp_proto::{
	blob::v1::{Chunk, GetRequest, StatRequest},
	env::v1::CaptureInputsRequest,
};
use tokio_util::sync::CancellationToken;

use super::production::{ProductionAdwHost, ProductionError};
use crate::bridges::CommandCredentials;

/// Placement supported by Slice 1. No fallback or local backend is installed.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::Display,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum AdwExecPolicy {
	/// Run existing local Environment commands.
	#[default]
	LocalOnly,
	/// Offload only eligible code phases, with no fallback on remote errors.
	RemoteOnly,
}
omp_con::con_enum!(AdwExecPolicy);

omp_con::var! {
	/// ADW execution placement; always operator-controlled, never project cfg.
	pub static SV_EXEC_POLICY = sv_exec_policy: AdwExecPolicy {
		default: AdwExecPolicy::LocalOnly,
		flags: archive,
	};
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

/// The only phase class Slice 1 may offload. Unknown recipes remain local.
pub fn eligible(binary: &str, args: &[Str]) -> bool {
	binary == "just"
		&& args
			.first()
			.and_then(|arg| omp_exec::recipe_class(arg))
			.is_some_and(|class| {
				matches!(class.eligibility, Eligibility::RemotePreferred | Eligibility::RemoteRequired)
			})
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
	#[error("remote execution is not configured with an endpoint and digest-pinned image")]
	MissingConfiguration,
	#[error("remote credential environment variable is missing or empty")]
	EnvironmentCredential,
	#[error("remote credential must be an env:NAME or !command source")]
	CredentialSource,
	#[error("operator remote credential command failed")]
	Credential(#[source] omp_ai::auth::command::CommandCredentialError),
	#[error("repository toolchain pins could not be read")]
	Pins(#[source] io::Error),
	#[error("CargoJust could not plan this command")]
	Plan(#[source] omp_exec::PlanError),
	#[error("remote endpoint is not secure")]
	Endpoint(#[source] omp_exec_reapi::endpoint::EndpointError),
	#[error("project Environment input capture failed")]
	Capture(#[source] omp_env::ClientError),
	#[error("tracked input was refused: {path} matched {rule}")]
	InputRefused { path: Str, rule: Str },
	#[error("invalid environment capture manifest")]
	Manifest,
	#[error("remote action failed: {0}")]
	Execute(#[source] ExecutionError),
	#[error("BuildBuddy cancellation client could not connect")]
	CancelConnect(#[source] omp_exec_reapi::buildbuddy_cancel::CancelConnectError),
	#[error("remote artifact could not be materialized into the Environment blob store")]
	Materialize(#[source] omp_env::ClientError),
}

fn config_list(value: Str) -> Vec<String> {
	value
		.split(",")
		.map(|part| part.trim())
		.filter(|part| !part.is_empty())
		.map(|part| part.to_string())
		.collect()
}

fn cache_policy(ctx: &Ctx) -> omp_exec::CachePolicy {
	match SV_EXEC_REMOTE_CACHE.get(ctx).as_str() {
		"read" => omp_exec::CachePolicy::Read,
		"read-write" => omp_exec::CachePolicy::ReadWrite,
		_ => omp_exec::CachePolicy::Off,
	}
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

#[derive(Clone)]
struct EnvironmentInputs(EnvClient);

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

async fn materialize_artifact(
	backend: &ReapiBackend<EnvironmentInputs>,
	environment: &EnvClient,
	artifact: &Artifact,
) -> Result<Str, RemoteError> {
	let bytes = backend
		.download_blob(artifact.blob)
		.await
		.map_err(RemoteError::Execute)?;
	let upload = environment.blob_put().map_err(RemoteError::Materialize)?;
	upload
		.send_chunk(Chunk {
			data: bytes,
			hash: Bytes::copy_from_slice(artifact.blob.hash.as_bytes()),
			size: Some(artifact.blob.size),
		})
		.await
		.map_err(RemoteError::Materialize)?;
	upload.commit().await.map_err(RemoteError::Materialize)?;
	Ok(Str::new(format!("artifact://sha256/{}", artifact.blob.hash.to_hex())))
}

impl ProductionAdwHost {
	pub(super) async fn run_remote(
		&self,
		phase: &Phase,
		args: &[Str],
		cwd: Option<&str>,
		cancel: &CancellationToken,
	) -> Result<super::PhaseResult, ProductionError> {
		let ctx: &Ctx = &self.ctx;
		let endpoint = SV_EXEC_REMOTE_ENDPOINT.get(ctx);
		let endpoint_text = endpoint.clone();
		let image = SV_EXEC_REMOTE_IMAGE.get(ctx);
		if endpoint.is_empty() || image.is_empty() || !image.contains("@sha256:") {
			return Err(RemoteError::MissingConfiguration.into());
		}
		let endpoint = Endpoint::parse(&endpoint_text).map_err(RemoteError::Endpoint)?;
		let environment = self.environment().await?.clone();
		let credential_source = SV_EXEC_REMOTE_CREDENTIAL.get(ctx);
		let credential = if let Some(name) = credential_source.strip_prefix("env:") {
			let value = env::var(name)
				.ok()
				.filter(|value| !value.trim().is_empty())
				.ok_or(RemoteError::EnvironmentCredential)?;
			SecretString::from(value)
		} else {
			let Some(command) = credential_source.strip_prefix("!") else {
				return Err(RemoteError::CredentialSource.into());
			};
			let executor = CommandCredentials.make(environment.clone(), &self.root);
			CommandCredentialResolver::new(executor, Duration::from_secs(1))
				.resolve(&command, cancel.clone())
				.await
				.map_err(RemoteError::Credential)?
		};
		let pins = repo_pins(&self.root).map_err(RemoteError::Pins)?;
		let command = BuildCommand {
			id:          ExecutionId::new(Str::from(uuid_v4())),
			argv:        Arc::from([vec![Str::new_static("just")], args.to_vec()].concat()),
			cwd:         cwd.map(Str::new),
			intent:      Intent::Test,
			requirement: phase.requires,
			target:      Platform::LINUX_X86_64,
			host:        host_platform(),
		};
		let plan = CargoJust::default()
			.plan(&command, &pins)
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
				deny_globs: config_list(SV_EXEC_UPLOAD_EXCLUDE.get(ctx)),
				allow_paths: config_list(SV_EXEC_UPLOAD_ALLOW.get(ctx)),
				wire_revision: omp_proto::SCHEMA_REV,
				..Default::default()
			})
			.await
			.map_err(RemoteError::Capture)?;
		if let Some(refused) = capture.refused.first() {
			return Err(
				RemoteError::InputRefused {
					path: Str::new(&refused.path),
					rule: Str::new(&refused.rule),
				}
				.into(),
			);
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
		let buildbuddy = SV_EXEC_REMOTE_PROFILE.get(ctx).as_str() == "buildbuddy";
		let profile = if buildbuddy {
			Profile::buildbuddy()
		} else if SV_EXEC_REMOTE_PROFILE.get(ctx).as_str() == "reapi" {
			Profile::reapi()
		} else {
			return Err(RemoteError::MissingConfiguration.into());
		};
		let credential = Credential::new(credential);
		let buildbuddy_canceller = if buildbuddy {
			Some(
				omp_exec_reapi::buildbuddy_cancel::BuildBuddyCanceller::connect(
					&endpoint_text,
					credential.clone(),
				)
				.await
				.map_err(RemoteError::CancelConnect)?,
			)
		} else {
			None
		};
		let config = ExecuteConfig {
			id: BackendId::new_static("remote"),
			instance: Str::empty(),
			profile,
			credential: Some(credential),
			platform: Platform::LINUX_X86_64,
			enforced: REMOTE_POSTURE,
			cache: cache_policy(ctx),
			platform_properties: BTreeMap::new(),
			toolchains: BTreeMap::from([(
				CargoJust::toolchain(&pins).to_string().into(),
				Toolchain {
					image: Some(image),
					env:   Arc::from([EnvEntry {
						name:  Str::new_static("PATH"),
						value: Str::new_static("/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"),
					}]),
				},
			)]),
			buildbuddy_canceller,
		};
		let materialize_client = environment.clone();
		let backend = ReapiBackend::connect(endpoint, config, EnvironmentInputs(environment))
			.await
			.map_err(RemoteError::Execute)?;
		let mut run = backend.start(request).await.map_err(RemoteError::Execute)?;
		loop {
			let event = tokio::select! {
				event = run.next_event() => event,
				() = cancel.cancelled() => {
					let _ = run.cancel().await;
					return Err(ProductionError::Cancelled);
				},
			};
			match event {
				Some(ExecutionEvent::Settled(Ok(result))) => {
					let mut feedback =
						StrMut::new(format!("remote command exited {:?}\n", result.outcome));
					if let Some(logs) = result.logs {
						let stdout =
							backend.download_blob(logs.stdout).await.map_err(RemoteError::Execute)?;
						let stderr =
							backend.download_blob(logs.stderr).await.map_err(RemoteError::Execute)?;
						feedback.push_str("stdout:\n");
						feedback.push_str(&String::from_utf8_lossy(&stdout));
						feedback.push_str("\nstderr:\n");
						feedback.push_str(&String::from_utf8_lossy(&stderr));
					}
					for artifact in result.artifacts.iter() {
						let _ = materialize_artifact(&backend, &materialize_client, artifact).await?;
					}
					return Ok(super::PhaseResult {
						accepted: result.outcome.is_success(),
						feedback: feedback.freeze(),
					});
				},
				Some(ExecutionEvent::Settled(Err(source))) => {
					return Err(RemoteError::Execute(source).into());
				},
				Some(_) => {},
				None => return Err(RemoteError::Manifest.into()),
			}
		}
	}
}

fn host_platform() -> Platform {
	#[cfg(target_arch = "aarch64")]
	let arch = omp_exec::Arch::Aarch64;
	#[cfg(not(target_arch = "aarch64"))]
	let arch = omp_exec::Arch::X86_64;
	#[cfg(target_os = "macos")]
	let os = omp_exec::Os::Macos;
	#[cfg(not(target_os = "macos"))]
	let os = omp_exec::Os::Linux;
	Platform { os, arch }
}

fn uuid_v4() -> String {
	use rand::RngExt as _;
	let mut rng = rand::rng();
	let bytes: [u8; 16] = rng.random();
	let first = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
	let second = u16::from_be_bytes([bytes[4], bytes[5]]);
	let third = u16::from_be_bytes([bytes[6], bytes[7]]) & 0x0fff;
	let fourth = u16::from_be_bytes([bytes[8], bytes[9]]) & 0x3fff | 0x8000;
	let tail =
		u64::from_be_bytes([0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]]);
	format!("{first:08x}-{second:04x}-4{third:03x}-{fourth:04x}-{tail:012x}")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_remote_preferred_just_recipes_are_eligible() {
		assert!(eligible("just", &[Str::new_static("test-pkg")]));
		assert!(eligible("just", &[Str::new_static("clippy")]));
		assert!(!eligible("just", &[Str::new_static("e2e-p7")]));
		assert!(!eligible("cargo", &[Str::new_static("test")]));
		assert!(!eligible("just", &[Str::new_static("unknown")]));
	}

	#[test]
	fn operator_policy_and_upload_lists_are_typed_at_the_boundary() {
		assert_eq!("remote-only".parse::<AdwExecPolicy>(), Ok(AdwExecPolicy::RemoteOnly));
		assert_eq!(config_list(Str::new_static(" .env , *.key, ")), [".env", "*.key"]);
	}

	#[test]
	fn uuid_has_v4_shape() {
		let value = uuid_v4();
		assert_eq!(value.len(), 36);
		assert_eq!(value.as_bytes()[14], b'4');
		assert!(matches!(value.as_bytes()[19], b'8'..=b'9' | b'a'..=b'b'));
	}
}
