//! Dyn `ci/*` device bridge (ADR 0049 follow-up to D2.3).
//!
//! T2 extensions implement [`omp_ci::CiHost`] by mounting dyn devices at
//! `ci/<op>` (snake_case [`omp_ci::CiOp`], plus `ci/capabilities`) and
//! answering `omp.devices.call`. This module is the in-core half: a
//! [`DynCiAdapter`] that dispatches each [`CiHost`] method to those paths.
//! [`CiBackend`] is a Dyn-only type alias so the production
//! [`super::ci_router`] stays unboxed over [`CiHost`] futures.
//!
//! Schema source of truth: `docs/adr/0049-ci-dyn-device-schema.md`.
//! Capabilities ceilings stay enforced in `omp-ci-delegated` at registration.

use std::{future::Future, pin::Pin, sync::Arc};

use omp_ci::{
	ArtifactPage, Capabilities, CiError, CiHost, CiOp, CiProvider, Job, LogPage, Pipeline, RunRef,
	Trigger,
};
use omp_ci_delegated::{DelegatedBuilder, PermitAll, RegisterError};
use omp_core::Str;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use strum::IntoStaticStr as _;

/// Canonical GitLab SaaS host on the dyn CI arm (ADR 0049 T2 CI).
///
/// Production [`super::ci_router`] registers this host (and
/// [`CI_GITHUB_HOST`]) on Dyn with a shared [`LiveCiDeviceClient`]. Catalog
/// mounts live in `extensions/ci/`.
pub const CI_DYN_HOST: &str = "gitlab.com";

/// Canonical GitHub SaaS host on the dyn CI arm (ADR 0049). Same bound
/// [`LiveCiDeviceClient`] as [`CI_DYN_HOST`].
pub const CI_GITHUB_HOST: &str = "github.com";

/// Device paths under `ci/*`.
///
/// Primary paths match [`CiOp`] snake_case. [`CiHost::capabilities`] is not a
/// [`CiOp`] and uses [`paths::CAPABILITIES`].
pub mod paths {
	/// Not a [`CiOp`] → [`CiHost::capabilities`].
	pub const CAPABILITIES: &str = "ci/capabilities";
	/// [`CiOp::Status`] → [`CiHost::status`].
	pub const STATUS: &str = "ci/status";
	/// [`CiOp::Artifacts`] → [`CiHost::artifacts`].
	pub const ARTIFACTS: &str = "ci/artifacts";
	/// [`CiOp::LogStream`] → [`CiHost::log_stream`].
	pub const LOG_STREAM: &str = "ci/log_stream";
	/// [`CiOp::Cancel`] → [`CiHost::cancel`].
	pub const CANCEL: &str = "ci/cancel";
	/// [`CiOp::RerunFailed`] → [`CiHost::rerun_failed`].
	pub const RERUN_FAILED: &str = "ci/rerun_failed";
	/// [`CiOp::RerunJob`] → [`CiHost::rerun_job`].
	pub const RERUN_JOB: &str = "ci/rerun_job";
	/// [`CiOp::Trigger`] → [`CiHost::trigger`].
	pub const TRIGGER: &str = "ci/trigger";
	/// [`CiOp::PlayManual`] → [`CiHost::play_manual`].
	pub const PLAY_MANUAL: &str = "ci/play_manual";
}

/// `ci/<CiOp snake_case>` for one operation.
#[inline]
#[must_use]
pub fn ci_device_path(op: CiOp) -> Str {
	omp_core::sf!("ci/{}", op.into_str())
}

/// Invokes one mounted `ci/<op>` dyn device.
///
/// Production uses [`LiveCiDeviceClient`] (catalog / `omp.devices.call`).
/// Tests may inject a fake. The trait returns an unboxed future;
/// [`DynCiAdapter`] stays generic over the client so [`CiHost`] futures stay
/// unboxed.
pub trait CiDeviceClient: Clone + Send + Sync + 'static {
	/// Calls the device at `path` with JSON `args`.
	///
	/// Success payloads are the serde form of the matching `omp_ci` result
	/// type. Failures are typed [`CiError`] values from the client.
	fn call(&self, path: &str, args: Value) -> impl Future<Output = Result<Value, CiError>> + Send;
}

/// Object-safe invoker behind [`LiveCiDeviceClient`].
///
/// Cold I/O boundary: one allocation per CI device round-trip (extension
/// callback / network). Production installs
/// [`super::super::tools::DeviceControlFactory`]'s catalog path; tests install
/// a recording fake.
pub trait CiDeviceInvoker: Send + Sync + 'static {
	/// Invokes the mounted device at `path`.
	fn invoke(
		&self,
		path: &str,
		args: Value,
	) -> Pin<Box<dyn Future<Output = Result<Value, CiError>> + Send + '_>>;
}

/// Maps a `ci/<op>` path to a [`CiOp`] for error reporting.
///
/// Unknown paths (including [`paths::CAPABILITIES`]) fall back to
/// [`CiOp::Status`].
#[must_use]
pub fn ci_op_for_path(path: &str) -> CiOp {
	path
		.strip_prefix("ci/")
		.and_then(|name| name.parse().ok())
		.unwrap_or(CiOp::Status)
}

/// Live production [`CiDeviceClient`]: dispatches mounted `ci/*` devices.
///
/// Holds a late-bound [`CiDeviceInvoker`]. Until [`Self::bind`] installs one
/// (or after [`Self::unbind`]), every call returns
/// [`CiError::Unavailable`]. The production router still registers every host
/// in [`super::ForgeDynHosts`] so Delegated can reach the dyn arm;
/// Unavailable until a CI extension mounts devices (or the invoker is bound)
/// is intentional.
///
/// Bind from composition once [`crate::tools::DeviceControlFactory`] exists:
/// `client.bind(devices.ci_invoker())`. That invoker looks up mounted leaves
/// and dispatches `omp.devices.call` — the same destination as
/// `DeviceControlFactory::invoke`.
#[derive(Clone, Default)]
pub struct LiveCiDeviceClient {
	invoker: Arc<RwLock<Option<Arc<dyn CiDeviceInvoker>>>>,
}

impl LiveCiDeviceClient {
	/// Installs the catalog-backed (or test) invoker.
	pub fn bind(&self, invoker: Arc<dyn CiDeviceInvoker>) {
		*self.invoker.write() = Some(invoker);
	}

	/// Clears the invoker (devices unmounted / session teardown).
	pub fn unbind(&self) {
		*self.invoker.write() = None;
	}

	/// Whether an invoker is currently bound.
	#[must_use]
	pub fn is_bound(&self) -> bool {
		self.invoker.read().is_some()
	}
}

impl CiDeviceClient for LiveCiDeviceClient {
	fn call(&self, path: &str, args: Value) -> impl Future<Output = Result<Value, CiError>> + Send {
		let invoker = self.invoker.read().clone();
		let path = path.to_owned();
		async move {
			let Some(invoker) = invoker else {
				return Err(CiError::Unavailable { op: ci_op_for_path(&path) });
			};
			invoker.invoke(&path, args).await
		}
	}
}

/// Placeholder client that always returns [`CiError::Unavailable`].
///
/// Prefer [`LiveCiDeviceClient`] for production [`CiBackend`]. Kept for tests
/// that want a permanently-dead dyn arm without a bind slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableCiClient;

impl CiDeviceClient for UnavailableCiClient {
	fn call(&self, path: &str, _args: Value) -> impl Future<Output = Result<Value, CiError>> + Send {
		std::future::ready(Err(CiError::Unavailable { op: ci_op_for_path(path) }))
	}
}

/// [`CiHost`] that dispatches each method to a `ci/<op>` dyn device.
#[derive(Clone)]
pub struct DynCiAdapter<C> {
	client: C,
}

impl<C> DynCiAdapter<C> {
	/// Builds an adapter over `client`.
	#[must_use]
	pub const fn new(client: C) -> Self {
		Self { client }
	}
}

impl<C: CiDeviceClient> DynCiAdapter<C> {
	async fn invoke_decode<T: DeserializeOwned>(
		&self,
		op: CiOp,
		path: &str,
		args: Value,
	) -> Result<T, CiError> {
		let value = self.client.call(path, args).await?;
		serde_json::from_value(value).map_err(|_| CiError::Unavailable { op })
	}
}

impl<C: CiDeviceClient> CiHost for DynCiAdapter<C> {
	fn capabilities(
		&self,
		provider: &CiProvider,
	) -> impl Future<Output = Result<Capabilities, CiError>> + Send {
		let this = self.clone();
		let provider = provider.clone();
		async move {
			this
				.invoke_decode(CiOp::Status, paths::CAPABILITIES, json!({ "provider": provider }))
				.await
		}
	}

	fn status(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		let this = self.clone();
		let run = run.clone();
		async move {
			this
				.invoke_decode(CiOp::Status, paths::STATUS, json!({ "run": run }))
				.await
		}
	}

	fn artifacts(&self, run: &RunRef) -> impl Future<Output = Result<ArtifactPage, CiError>> + Send {
		let this = self.clone();
		let run = run.clone();
		async move {
			this
				.invoke_decode(CiOp::Artifacts, paths::ARTIFACTS, json!({ "run": run }))
				.await
		}
	}

	fn log_stream(&self, run: &RunRef) -> impl Future<Output = Result<LogPage, CiError>> + Send {
		let this = self.clone();
		let run = run.clone();
		async move {
			this
				.invoke_decode(CiOp::LogStream, paths::LOG_STREAM, json!({ "run": run }))
				.await
		}
	}

	fn cancel(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		let this = self.clone();
		let run = run.clone();
		async move {
			this
				.invoke_decode(CiOp::Cancel, paths::CANCEL, json!({ "run": run }))
				.await
		}
	}

	fn rerun_failed(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		let this = self.clone();
		let run = run.clone();
		async move {
			this
				.invoke_decode(CiOp::RerunFailed, paths::RERUN_FAILED, json!({ "run": run }))
				.await
		}
	}

	fn rerun_job(&self, run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
		let this = self.clone();
		let run = run.clone();
		async move {
			this
				.invoke_decode(CiOp::RerunJob, paths::RERUN_JOB, json!({ "run": run }))
				.await
		}
	}

	fn trigger(&self, request: Trigger) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		let this = self.clone();
		async move {
			this
				.invoke_decode(CiOp::Trigger, paths::TRIGGER, json!({ "request": request }))
				.await
		}
	}

	fn play_manual(&self, run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
		let this = self.clone();
		let run = run.clone();
		async move {
			this
				.invoke_decode(CiOp::PlayManual, paths::PLAY_MANUAL, json!({ "run": run }))
				.await
		}
	}
}

/// Concrete [`CiHost`] held by the production CI router — Dyn only.
///
/// After the in-core `github.rs` deletion there is no Github arm; every
/// registered host is a [`DynCiAdapter`]. The alias keeps call sites and
/// Delegated type parameters stable. The default client is
/// [`LiveCiDeviceClient`].
pub type CiBackend<C = LiveCiDeviceClient> = DynCiAdapter<C>;

/// Registers a dyn-backed CI host on a [`DelegatedBuilder`].
///
/// This is the seam T2 extensions (and tests) use to place a host such as
/// [`CI_GITHUB_HOST`] / [`CI_DYN_HOST`] on the Dyn arm. The production
/// [`super::ci_router`] registers every host in [`super::ForgeDynHosts`] with
/// a shared [`LiveCiDeviceClient`]. Catalog mounts for `ci/*` are owned by
/// `extensions/ci/` (`omp.ci`). Adapter host-claim discovery
/// (`GITLAB_HOST` / `GITHUB_HOST` / convar / catalog claims) lives in
/// [`super::ForgeDynHosts`] — the same claim set as forge.
///
/// `capabilities` is the Delegated ceiling (`Capabilities::allows`); dyn
/// device answers cannot widen it.
///
/// # Errors
///
/// Returns [`RegisterError`] when `host` is invalid or already registered.
pub fn register_dyn_ci_host<C: CiDeviceClient>(
	builder: &mut DelegatedBuilder<CiBackend<C>, PermitAll>,
	host: &str,
	client: C,
	capabilities: Capabilities,
) -> Result<(), RegisterError> {
	builder.register(host, DynCiAdapter::new(client), capabilities)
}

#[cfg(test)]
mod tests {
	use std::{future::Future, sync::Arc};

	use omp_ci::{
		Capabilities, CiError, CiHost as _, CiOp, CiProvider, CiStatus, Job, LogPage, Pipeline,
		RunRef,
	};
	use omp_ci_delegated::{Delegated, PermitAll};
	use omp_core::Str;
	use parking_lot::Mutex;
	use serde_json::{Value, json};

	use super::{
		CI_DYN_HOST, CI_GITHUB_HOST, CiBackend, CiDeviceClient, CiDeviceInvoker, DynCiAdapter,
		LiveCiDeviceClient, ci_device_path, ci_op_for_path, paths, register_dyn_ci_host,
	};

	fn dyn_caps() -> Capabilities {
		Capabilities { log_stream: true, ..Capabilities::none() }
	}

	#[derive(Clone)]
	struct FakeClient {
		calls:    Arc<Mutex<Vec<(String, Value)>>>,
		pipeline: Pipeline,
		log:      LogPage,
	}

	impl CiDeviceClient for FakeClient {
		fn call(
			&self,
			path: &str,
			args: Value,
		) -> impl Future<Output = Result<Value, CiError>> + Send {
			self.calls.lock().push((path.to_string(), args));
			let result = match path {
				paths::STATUS => Ok(serde_json::to_value(&self.pipeline).expect("pipeline")),
				paths::LOG_STREAM => Ok(serde_json::to_value(&self.log).expect("log")),
				paths::CAPABILITIES => Ok(serde_json::to_value(dyn_caps()).expect("caps")),
				other => {
					let op = other
						.strip_prefix("ci/")
						.and_then(|name| name.parse().ok())
						.unwrap_or(CiOp::Status);
					Err(CiError::Unsupported { op })
				},
			};
			std::future::ready(result)
		}
	}

	fn sample_provider(host: &str) -> CiProvider {
		CiProvider::try_new(host).unwrap()
	}

	fn sample_run(host: &str, pipeline: &str, job: Option<&str>) -> RunRef {
		RunRef::try_new(sample_provider(host), pipeline, job.map(|job| Str::new(job))).unwrap()
	}

	fn sample_pipeline(host: &str) -> Pipeline {
		Pipeline::new(sample_provider(host), "42", "ci", CiStatus::Failure, [Job::new(
			"7",
			"test",
			CiStatus::Failure,
		)
		.with_web_url("https://gitlab.example/acme/widget/-/jobs/7")])
		.with_web_url("https://gitlab.example/acme/widget/-/pipelines/42")
		.with_revision("abc123")
	}

	fn sample_log(host: &str) -> LogPage {
		LogPage::new(sample_run(host, "42", Some("7")), "boom\n", true)
	}

	#[test]
	fn ci_device_paths_match_op_snake_case() {
		assert_eq!(ci_device_path(CiOp::Status).as_str(), paths::STATUS);
		assert_eq!(ci_device_path(CiOp::LogStream).as_str(), paths::LOG_STREAM);
		assert_eq!(ci_device_path(CiOp::RerunFailed).as_str(), paths::RERUN_FAILED);
		assert_eq!(ci_device_path(CiOp::PlayManual).as_str(), paths::PLAY_MANUAL);
		assert_eq!(ci_op_for_path(paths::LOG_STREAM), CiOp::LogStream);
		assert_eq!(ci_op_for_path(paths::CAPABILITIES), CiOp::Status);
	}

	#[tokio::test]
	async fn delegated_routes_dyn_host_to_device_client() {
		let host = "gitlab.example";
		let calls = Arc::new(Mutex::new(Vec::new()));
		let fake = FakeClient {
			calls:    Arc::clone(&calls),
			pipeline: sample_pipeline(host),
			log:      sample_log(host),
		};
		let caps = dyn_caps();
		let mut builder = Delegated::<CiBackend<FakeClient>, _>::builder(PermitAll);
		register_dyn_ci_host(
			&mut builder,
			CI_GITHUB_HOST,
			FakeClient {
				calls:    Arc::new(Mutex::new(Vec::new())),
				pipeline: sample_pipeline(CI_GITHUB_HOST),
				log:      sample_log(CI_GITHUB_HOST),
			},
			caps,
		)
		.expect("github.com");
		register_dyn_ci_host(&mut builder, host, fake, caps).expect("gitlab");
		let router = builder.build();

		let run = sample_run(host, "42", None);
		let pipeline = router.status(&run).await.expect("status");
		assert_eq!(pipeline.id.as_str(), "42");
		assert_eq!(pipeline.status, CiStatus::Failure);
		assert_eq!(pipeline.jobs.len(), 1);
		assert_eq!(pipeline.jobs[0].id.as_str(), "7");

		let log_run = sample_run(host, "42", Some("7"));
		let page = router.log_stream(&log_run).await.expect("log");
		assert_eq!(page.text.as_str(), "boom\n");
		assert!(page.complete);

		let recorded = calls.lock().clone();
		assert_eq!(recorded.len(), 2);
		assert_eq!(recorded[0].0, paths::STATUS);
		assert_eq!(recorded[0].1["run"]["pipeline"], "42");
		assert_eq!(recorded[1].0, paths::LOG_STREAM);
		assert_eq!(recorded[1].1["run"]["job"], "7");
	}

	#[tokio::test]
	async fn unregistered_dyn_host_stays_unregistered() {
		let mut builder = Delegated::<CiBackend<FakeClient>, _>::builder(PermitAll);
		register_dyn_ci_host(
			&mut builder,
			CI_GITHUB_HOST,
			FakeClient {
				calls:    Arc::new(Mutex::new(Vec::new())),
				pipeline: sample_pipeline(CI_GITHUB_HOST),
				log:      sample_log(CI_GITHUB_HOST),
			},
			dyn_caps(),
		)
		.expect("github.com");
		let router = builder.build();
		let run = sample_run("gitlab.example", "1", None);
		let error = router.status(&run).await.expect_err("unregistered");
		assert!(
			matches!(
				error,
				CiError::Unregistered { ref provider, op: CiOp::Status }
					if provider == "gitlab.example"
			),
			"{error}"
		);
	}

	#[tokio::test]
	async fn dyn_adapter_alone_decodes_status_and_log() {
		let host = "gitlab.example";
		let fake = FakeClient {
			calls:    Arc::new(Mutex::new(Vec::new())),
			pipeline: sample_pipeline(host),
			log:      sample_log(host),
		};
		let adapter = DynCiAdapter::new(fake);
		let pipeline = adapter
			.status(&sample_run(host, "42", None))
			.await
			.expect("status");
		assert_eq!(pipeline.revision.as_deref(), Some("abc123"));
		let page = adapter
			.log_stream(&sample_run(host, "42", Some("7")))
			.await
			.expect("log");
		assert_eq!(page.text.as_str(), "boom\n");
	}

	#[tokio::test]
	async fn github_com_on_dyn_arm_uses_device_client() {
		let host = CI_GITHUB_HOST;
		let calls = Arc::new(Mutex::new(Vec::new()));
		let fake = FakeClient {
			calls:    Arc::clone(&calls),
			pipeline: sample_pipeline(host),
			log:      sample_log(host),
		};
		let caps = dyn_caps();
		let mut builder = Delegated::<CiBackend<FakeClient>, _>::builder(PermitAll);
		register_dyn_ci_host(&mut builder, host, fake, caps).expect("github.com dyn");
		let router = builder.build();

		let pipeline = router
			.status(&sample_run(host, "42", None))
			.await
			.expect("status");
		assert_eq!(pipeline.id.as_str(), "42");
		assert_eq!(pipeline.status, CiStatus::Failure);

		let page = router
			.log_stream(&sample_run(host, "42", Some("7")))
			.await
			.expect("log");
		assert_eq!(page.text.as_str(), "boom\n");

		let recorded = calls.lock().clone();
		assert_eq!(recorded.len(), 2);
		assert_eq!(recorded[0].0, paths::STATUS);
		assert_eq!(recorded[1].0, paths::LOG_STREAM);
	}

	struct RecordingInvoker {
		calls:    Arc<Mutex<Vec<(String, Value)>>>,
		pipeline: Pipeline,
		log:      LogPage,
	}

	impl CiDeviceInvoker for RecordingInvoker {
		fn invoke(
			&self,
			path: &str,
			args: Value,
		) -> std::pin::Pin<Box<dyn Future<Output = Result<Value, CiError>> + Send + '_>> {
			self.calls.lock().push((path.to_string(), args));
			let result = match path {
				paths::STATUS => Ok(serde_json::to_value(&self.pipeline).expect("pipeline")),
				paths::LOG_STREAM => Ok(serde_json::to_value(&self.log).expect("log")),
				other => Err(CiError::Unavailable { op: ci_op_for_path(other) }),
			};
			Box::pin(std::future::ready(result))
		}
	}

	#[tokio::test]
	async fn live_client_unbound_is_unavailable() {
		let client = LiveCiDeviceClient::default();
		assert!(!client.is_bound());
		let error = client
			.call(paths::STATUS, json!({}))
			.await
			.expect_err("unbound");
		assert!(matches!(error, CiError::Unavailable { op: CiOp::Status }));
	}

	#[tokio::test]
	async fn live_client_bound_invoker_serves_dyn_host() {
		let host = CI_DYN_HOST;
		let calls = Arc::new(Mutex::new(Vec::new()));
		let client = LiveCiDeviceClient::default();
		client.bind(Arc::new(RecordingInvoker {
			calls:    Arc::clone(&calls),
			pipeline: sample_pipeline(host),
			log:      sample_log(host),
		}));
		assert!(client.is_bound());

		let caps = dyn_caps();
		let mut builder = Delegated::<CiBackend<LiveCiDeviceClient>, _>::builder(PermitAll);
		register_dyn_ci_host(&mut builder, CI_DYN_HOST, client, caps).expect("gitlab.com");
		let router = builder.build();

		let run = sample_run(host, "42", None);
		let pipeline = router.status(&run).await.expect("status via live client");
		assert_eq!(pipeline.id.as_str(), "42");
		assert_eq!(pipeline.status, CiStatus::Failure);

		let page = router
			.log_stream(&sample_run(host, "42", Some("7")))
			.await
			.expect("log via live client");
		assert_eq!(page.text.as_str(), "boom\n");

		let recorded = calls.lock().clone();
		assert_eq!(recorded.len(), 2);
		assert_eq!(recorded[0].0, paths::STATUS);
		assert_eq!(recorded[1].0, paths::LOG_STREAM);
	}

	#[tokio::test]
	async fn delegated_caps_still_block_writes_on_dyn_host() {
		let host = "gitlab.example";
		let fake = FakeClient {
			calls:    Arc::new(Mutex::new(Vec::new())),
			pipeline: sample_pipeline(host),
			log:      sample_log(host),
		};
		let mut builder = Delegated::<CiBackend<FakeClient>, _>::builder(PermitAll);
		register_dyn_ci_host(&mut builder, host, fake.clone(), dyn_caps()).expect("dyn");
		let router = builder.build();
		let run = sample_run(host, "42", None);
		let error = router.cancel(&run).await.expect_err("not capable");
		assert!(
			matches!(error, CiError::NotCapable { op: CiOp::Cancel, ref provider } if provider == host),
			"{error}"
		);
		assert!(fake.calls.lock().is_empty(), "Delegated must not reach the dyn client");
	}
}
