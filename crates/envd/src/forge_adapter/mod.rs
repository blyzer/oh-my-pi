//! Dyn-backed [`ForgeHost`] and [`CiHost`] routers (ADR 0049 D2.4+).
//!
//! Forge@1 / CI@1 ride [`ForgeBackend`] / [`CiBackend`] — both Dyn-only after
//! the in-core `github.rs` deletion. Checkout/push obtain vendor plan data
//! then run [`crate::forge_worktree`]. Numbered `pr://` / `issue://` reads and
//! list-URL search go through the same routers.
//! `PermitAll` is intentional: the desk gate is the tool commit, and the
//! routers are not exported, so a second approval would only repeat that gate.
//!
//! Hosts are registered explicitly. Dyn hosts come from [`ForgeDynHosts`]:
//! always `gitlab.com` and `github.com`, plus `sv_forge_dyn_hosts` and adapter
//! env `GITLAB_HOST`. The checkout host from `infer_repo` joins the Dyn arm
//! when it differs (same live client), so GHE / self-hosted listed via remote
//! still works when extensions answer; unlisted hosts without mounts →
//! [`omp_forge::ForgeError::Unavailable`] (honest). There is no in-core
//! Github fallback arm.
//!
//! Catalog mounts for `forge/<op>` are owned by the `omp.forge` extension
//! (host-dispatch to `omp_github` / `omp_gitlab`). Catalog mounts for
//! `ci/<op>` are owned by the `omp.ci` extension (host-dispatch to
//! `omp_github.ci` / `omp_gitlab.ci`).
//!
//! The CI registration advertises `log_stream` only. Status stays allowed
//! because the contract does not flag it. Cancel, rerun, trigger, artifacts,
//! and play-manual are `NotCapable` at the router.
//!
//! [`CheckoutForge`] is that same router for one checkout. Chat's git watcher
//! reads the branch's change request through `view_change_request` on it,
//! after a forge-neutral [`ForgeHost::search`] for the open head branch.
//!
//! Under `cap-forge`, the router holds a [`ForgeBackend`] type alias over
//! [`DynForgeAdapter`] (ADR 0049 D2.3). Production registers Dyn hosts via
//! [`LiveForgeDeviceClient`] (D2.8). Full automatic device-catalog host-claim
//! discovery remains a smaller TODO.
//!
//! Under `cap-ci`, the CI router holds a [`CiBackend`] type alias the same way.
//! Production registers [`CI_GITHUB_HOST`] (`github.com`) and [`CI_DYN_HOST`]
//! (`gitlab.com`) on Dyn via a shared [`LiveCiDeviceClient`], plus the
//! checkout host when it differs.

#[cfg(feature = "cap-ci")]
mod dyn_ci;
#[cfg(feature = "cap-forge")]
mod dyn_forge;
use std::{
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

#[cfg(feature = "cap-ci")]
pub(crate) use dyn_ci::{
	CI_DYN_HOST, CI_GITHUB_HOST, CiBackend, CiDeviceClient, CiDeviceInvoker, DynCiAdapter,
	LiveCiDeviceClient, UnavailableCiClient, ci_device_path, ci_op_for_path,
	paths as ci_device_paths, register_dyn_ci_host,
};
#[cfg(feature = "cap-forge")]
pub(crate) use dyn_forge::{
	DynForgeAdapter, ForgeBackend, ForgeDeviceClient, ForgeDeviceInvoker, GITHUB_DYN_HOST,
	GITLAB_HOST, LiveForgeDeviceClient, UnavailableForgeClient, forge_device_path,
	forge_op_for_path, paths as forge_device_paths, register_dyn_forge_host,
};
#[cfg(feature = "cap-ci")]
use omp_ci::Capabilities;
#[cfg(feature = "cap-ci")]
use omp_ci_delegated::{Delegated as CiDelegated, PermitAll as CiPermitAll};
#[cfg(feature = "cap-forge")]
use omp_con::Ctx;
use omp_core::{Str, sf};
#[cfg(feature = "cap-forge")]
use omp_forge::{ChangeRequest, ForgeError, ForgeHost, ForgeOp, RepoRef, SearchKind, SearchQuery};
#[cfg(feature = "cap-forge")]
use omp_forge_delegated::{Delegated as ForgeDelegated, PermitAll as ForgePermitAll};
#[cfg(feature = "cap-forge")]
use parking_lot::Mutex;
#[cfg(feature = "cap-forge")]
use smallvec::SmallVec;
#[cfg(feature = "cap-forge")]
use tokio::time;

use crate::forge_url;
#[cfg(feature = "cap-forge")]
use crate::tool_settings::SV_FORGE_DYN_HOSTS;

/// Hosts registered on the dyn forge arm (ADR 0049 D2.8).
///
/// Always includes [`GITLAB_HOST`] and [`GITHUB_DYN_HOST`]. Additional hosts
/// come from the `sv_forge_dyn_hosts` convar (comma/whitespace-separated ASCII
/// hosts) and from adapter env `GITLAB_HOST` when set and non-empty.
///
/// Self-hosted GitLab or GHE must appear via the convar (or `GITLAB_HOST` for
/// GitLab) to be pre-registered; an unlisted checkout host is still placed on
/// Dyn at router build so a remote that extensions answer can work without a
/// second registration path.
#[cfg(feature = "cap-forge")]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ForgeDynHosts {
	hosts: SmallVec<Str, 4>,
}

#[cfg(feature = "cap-forge")]
impl ForgeDynHosts {
	/// Resolves dyn hosts from archived convars plus `GITLAB_HOST`.
	#[must_use]
	pub fn from_con(ctx: &Ctx) -> Self {
		Self::resolve(SV_FORGE_DYN_HOSTS.get(ctx).as_str())
	}

	/// Resolves dyn hosts from a raw `sv_forge_dyn_hosts` list plus env.
	#[must_use]
	pub fn resolve(convar: &str) -> Self {
		let env = std::env::var("GITLAB_HOST").ok();
		Self::from_sources(convar, env.as_deref())
	}

	/// Resolves dyn hosts from operator list text and an optional `GITLAB_HOST`.
	#[must_use]
	pub fn from_sources(convar: &str, gitlab_host_env: Option<&str>) -> Self {
		let mut hosts = SmallVec::<Str, 4>::new();
		push_dyn_host(&mut hosts, GITLAB_HOST);
		push_dyn_host(&mut hosts, GITHUB_DYN_HOST);
		for host in split_dyn_hosts(convar) {
			push_dyn_host(&mut hosts, host);
		}
		if let Some(env_host) = gitlab_host_env {
			push_dyn_host(&mut hosts, env_host.trim());
		}
		Self { hosts }
	}

	/// Whether `host` is registered on the Dyn arm (ASCII case-insensitive).
	#[must_use]
	pub fn contains(&self, host: &str) -> bool {
		self
			.hosts
			.iter()
			.any(|known| known.as_str().eq_ignore_ascii_case(host))
	}

	/// Dyn hosts in registration order (`gitlab.com`, then `github.com`, …).
	#[must_use]
	pub fn iter(&self) -> impl Iterator<Item = &str> + Clone + '_ {
		self.hosts.iter().map(Str::as_str)
	}
}

/// Comma- or whitespace-separated ASCII host tokens from `sv_forge_dyn_hosts`.
#[cfg(feature = "cap-forge")]
fn split_dyn_hosts(raw: &str) -> impl Iterator<Item = &str> + '_ {
	raw.split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
		.map(str::trim)
		.filter(|host| !host.is_empty())
}

#[cfg(feature = "cap-forge")]
fn push_dyn_host(hosts: &mut SmallVec<Str, 4>, host: &str) {
	let host = host.trim();
	if host.is_empty() {
		return;
	}
	if hosts
		.iter()
		.any(|known| known.as_str().eq_ignore_ascii_case(host))
	{
		return;
	}
	hosts.push(Str::new(host));
}

/// Routes `forge@1` through [`ForgeBackend`] (Dyn only).
///
/// See the module docs for `PermitAll` and host registration.
///
/// # Host registration
///
/// - every host in [`ForgeDynHosts`] (always [`GITLAB_HOST`] +
///   [`GITHUB_DYN_HOST`], plus `sv_forge_dyn_hosts` / `GITLAB_HOST`) →
///   [`ForgeBackend`] over `devices`
/// - checkout host from `infer_repo` when it differs → same Dyn arm (GHE /
///   self-hosted still works when extensions answer; otherwise Unavailable)
///
/// `devices` may be unbound ([`LiveForgeDeviceClient`] returns
/// [`omp_forge::ForgeError::Unavailable`] until
/// [`LiveForgeDeviceClient::bind`]); registration still happens so Delegated
/// can reach the dyn arm. Automatic discovery of additional dyn hosts from
/// the device catalog / extension host claims remains TODO (ADR 0049 D2.8).
#[cfg(feature = "cap-forge")]
pub(crate) fn forge_router(
	devices: LiveForgeDeviceClient,
	dyn_hosts: &ForgeDynHosts,
	project_root: PathBuf,
	worktree_root: PathBuf,
) -> ForgeDelegated<ForgeBackend, ForgePermitAll> {
	let mut builder = ForgeDelegated::builder(ForgePermitAll);
	for host in dyn_hosts.iter() {
		register_dyn_forge_host(
			&mut builder,
			host,
			devices.clone(),
			project_root.clone(),
			worktree_root.clone(),
		)
		.expect("dyn forge host");
	}
	register_checkout_host(&project_root, |host| {
		if dyn_hosts.contains(host) {
			return;
		}
		// Unlisted checkout (GHE / self-hosted) on Dyn — honest Unavailable
		// until an extension answers for that host.
		let _ = register_dyn_forge_host(
			&mut builder,
			host,
			devices.clone(),
			project_root.clone(),
			worktree_root.clone(),
		);
	});
	builder.build()
}

/// Routes `ci@1` through [`CiBackend`] (Dyn only).
///
/// See the module docs for `PermitAll` and host registration.
///
/// # Host registration
///
/// - [`CI_GITHUB_HOST`] (`github.com`) → Dyn over `devices`
/// - [`CI_DYN_HOST`] (`gitlab.com`) → Dyn over the same client
/// - checkout host from `infer_repo` when it differs → same Dyn arm
///
/// `devices` may be unbound ([`LiveCiDeviceClient`] returns
/// [`omp_ci::CiError::Unavailable`] until [`LiveCiDeviceClient::bind`]);
/// both SaaS hosts are still registered so Delegated can reach the dyn arm.
/// Catalog mounts are owned by `extensions/ci/` (`omp.ci`).
#[cfg(feature = "cap-ci")]
pub(crate) fn ci_router(
	devices: LiveCiDeviceClient,
	project_root: &Path,
) -> CiDelegated<CiBackend, CiPermitAll> {
	let caps = dyn_ci_capabilities();
	let mut builder = CiDelegated::builder(CiPermitAll);
	register_dyn_ci_host(&mut builder, CI_GITHUB_HOST, devices.clone(), caps)
		.expect("github.com dyn ci host");
	register_dyn_ci_host(&mut builder, CI_DYN_HOST, devices.clone(), caps)
		.expect("gitlab.com dyn ci host");
	register_checkout_host(project_root, |host| {
		if host.eq_ignore_ascii_case(CI_DYN_HOST) || host.eq_ignore_ascii_case(CI_GITHUB_HOST) {
			return;
		}
		let _ = register_dyn_ci_host(&mut builder, host, devices.clone(), caps);
	});
	builder.build()
}

#[cfg(feature = "cap-ci")]
fn dyn_ci_capabilities() -> Capabilities {
	Capabilities { log_stream: true, ..Capabilities::none() }
}

fn register_checkout_host(project_root: &Path, mut register: impl FnMut(&str)) {
	let Ok(repo) = forge_url::infer_repo(project_root) else {
		return;
	};
	if repo.host().eq_ignore_ascii_case(forge_url::DEFAULT_FORGE_HOST) {
		return;
	}
	register(repo.host());
}

/// How long a branch change-request read may take before the watcher moves on.
#[cfg(feature = "cap-forge")]
const BRANCH_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Forge `Delegated` router for one checkout.
///
/// This is the same router `forge@1` builds ([`forge_router`]).
/// [`Self::view_branch`] searches open change requests whose head is `branch`,
/// then reads the record through [`ForgeHost::view_change_request`] when
/// exactly one hit. A checkout whose host is not on the Dyn set returns
/// `None` without a request.
#[cfg(feature = "cap-forge")]
#[derive(Clone)]
pub struct CheckoutForge {
	inner: Arc<CheckoutForgeInner>,
}

#[cfg(feature = "cap-forge")]
struct CheckoutForgeInner {
	root:      PathBuf,
	state_dir: PathBuf,
	devices:   LiveForgeDeviceClient,
	dyn_hosts: ForgeDynHosts,
	built:     Mutex<Option<BuiltForge>>,
}

#[cfg(feature = "cap-forge")]
#[derive(Clone)]
struct BuiltForge {
	router: ForgeDelegated<ForgeBackend, ForgePermitAll>,
}

#[cfg(feature = "cap-forge")]
impl CheckoutForge {
	/// Shares `devices` with the project environment.
	///
	/// The router is built on the first read against the same Dyn client the
	/// environment host binds.
	pub(crate) fn new(
		root: PathBuf,
		state_dir: PathBuf,
		devices: LiveForgeDeviceClient,
		dyn_hosts: ForgeDynHosts,
	) -> Self {
		Self {
			inner: Arc::new(CheckoutForgeInner {
				root,
				state_dir,
				devices,
				dyn_hosts,
				built: Mutex::new(None),
			}),
		}
	}

	/// Open change request whose head is `branch`, if the checkout has one.
	///
	/// Lookup is forge-neutral: [`ForgeHost::search`] with `head:` +
	/// `state:open`, then [`ForgeHost::view_change_request`] when exactly one
	/// hit. Works for `github.com` and `gitlab.com` through Dyn. `Ok(None)` is
	/// a checkout this adapter does not read, or a branch with no single open
	/// change request. Any other failure is returned so the caller can retry.
	pub async fn view_branch(&self, branch: &str) -> Result<Option<ChangeRequest>, ForgeError> {
		let branch = branch.trim();
		if branch.is_empty() || branch.starts_with('-') {
			return Ok(None);
		}
		if !serves_forge_checkout(&self.inner.root, &self.inner.dyn_hosts) {
			return Ok(None);
		}
		let read = async {
			let op = ForgeOp::ChangeRequestView;
			let built = self.built().ok_or(ForgeError::Unavailable { op })?;
			let repo = checkout_repo(&self.inner.root)?;
			let page = built
				.router
				.search(SearchQuery {
					kind:  SearchKind::ChangeRequests,
					repo:  Some(repo.clone()),
					text:  sf!("head:{branch} state:open"),
					limit: 10,
				})
				.await?;
			let numbers = page
				.as_slice()
				.iter()
				.filter(|hit| {
					hit.state.is_empty()
						|| hit.state.as_str().eq_ignore_ascii_case("open")
						|| hit.state.as_str().eq_ignore_ascii_case("opened")
				})
				.filter_map(|hit| hit.number)
				.collect::<Vec<_>>();
			let Some(number) = numbers.first().copied().filter(|_| numbers.len() == 1) else {
				return Ok(None);
			};
			built
				.router
				.view_change_request(&repo, number)
				.await
				.map(Some)
		};
		match time::timeout(BRANCH_READ_TIMEOUT, read).await {
			Ok(result) => result,
			Err(_elapsed) => Err(ForgeError::Unavailable { op: ForgeOp::ChangeRequestView }),
		}
	}

	fn built(&self) -> Option<BuiltForge> {
		let mut slot = self.inner.built.lock();
		if let Some(built) = slot.as_ref() {
			return Some(built.clone());
		}
		let built = BuiltForge {
			router: forge_router(
				self.inner.devices.clone(),
				&self.inner.dyn_hosts,
				self.inner.root.clone(),
				crate::forge_worktree::worktree_root(&self.inner.state_dir),
			),
		};
		*slot = Some(built.clone());
		Some(built)
	}
}

/// Repository the checkout's origin (or first remote) names.
#[cfg(feature = "cap-forge")]
pub(crate) fn checkout_repo(root: &Path) -> Result<RepoRef, ForgeError> {
	let repo =
		forge_url::infer_repo(root).map_err(|_| ForgeError::Unscoped { op: ForgeOp::RepoView })?;
	let mut parts = repo.slug().split('/');
	let owner = parts.next().unwrap_or_default();
	let name = parts.next().unwrap_or_default();
	RepoRef::try_new(repo.host(), owner, name).map_err(ForgeError::from)
}

/// Whether the checkout's inferred host is on the Dyn forge set.
///
/// Lights the branch chip for `github.com`, `gitlab.com`, and any host listed
/// via `sv_forge_dyn_hosts` / `GITLAB_HOST`. Unlisted remotes stay dark.
#[cfg(feature = "cap-forge")]
fn serves_forge_checkout(root: &Path, dyn_hosts: &ForgeDynHosts) -> bool {
	let Ok(repo) = forge_url::infer_repo(root) else {
		return false;
	};
	dyn_hosts.contains(repo.host())
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	#[cfg(feature = "cap-ci")]
	use omp_ci::{
		CiError, CiHost as _, CiOp, CiProvider, CiStatus, Job, LogPage, Pipeline, RunRef, Trigger,
	};
	use omp_core::Str;
	#[cfg(feature = "cap-forge")]
	use omp_forge::{
		ForgeError, ForgeHost as _, ForgeOp, RepoRef, SearchKind, SearchPage, SearchQuery,
	};

	#[cfg(feature = "cap-ci")]
	use super::{CI_DYN_HOST, CI_GITHUB_HOST, LiveCiDeviceClient};
	#[cfg(feature = "cap-forge")]
	use super::{
		ForgeDeviceInvoker, ForgeDynHosts, GITHUB_DYN_HOST, GITLAB_HOST, LiveForgeDeviceClient,
		forge_device_paths, forge_router,
	};

	#[cfg(feature = "cap-ci")]
	fn run(pipeline: &str) -> RunRef {
		RunRef::try_new(CiProvider::try_new("github.com").unwrap(), pipeline, None).unwrap()
	}

	#[cfg(feature = "cap-forge")]
	fn default_dyn_hosts() -> ForgeDynHosts {
		ForgeDynHosts::from_sources("", None)
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn forge_dyn_hosts_always_includes_gitlab_and_github_com() {
		let hosts = ForgeDynHosts::from_sources("", None);
		assert!(hosts.contains(GITLAB_HOST));
		assert!(hosts.contains(GITHUB_DYN_HOST));
		assert_eq!(hosts.iter().next(), Some(GITLAB_HOST));
		assert_eq!(hosts.iter().nth(1), Some(GITHUB_DYN_HOST));
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn forge_dyn_hosts_parses_convar_and_keeps_github() {
		let hosts =
			ForgeDynHosts::from_sources("gitlab.example.com, github.com forge.internal", None);
		assert!(hosts.contains("gitlab.example.com"));
		assert!(hosts.contains("forge.internal"));
		assert!(hosts.contains(GITHUB_DYN_HOST));
		assert!(hosts.contains(GITLAB_HOST));
		assert_eq!(
			hosts
				.iter()
				.filter(|host| host.eq_ignore_ascii_case(GITHUB_DYN_HOST))
				.count(),
			1
		);
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn forge_dyn_hosts_treats_gitlab_host_env_as_dyn() {
		let hosts = ForgeDynHosts::from_sources("", Some("gitlab.example.com"));
		assert!(hosts.contains("gitlab.example.com"));
		assert!(hosts.contains(GITLAB_HOST));
		assert!(hosts.contains(GITHUB_DYN_HOST));
		let ignored = ForgeDynHosts::from_sources("", Some(""));
		assert!(!ignored.contains("gitlab.example.com"));
		let listed = ForgeDynHosts::from_sources("", Some("github.com"));
		assert!(listed.contains(GITHUB_DYN_HOST));
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn unregistered_host_is_not_sent_to_dyn() {
		let dir = tempfile::tempdir().expect("temp");
		let router = forge_router(
			LiveForgeDeviceClient::default(),
			&default_dyn_hosts(),
			dir.path().to_path_buf(),
			crate::forge_worktree::worktree_root(dir.path()),
		);
		let repo = RepoRef::try_new("gitlab.example", "acme", "widget").unwrap();
		let error = router.view_repo(&repo).await.expect_err("unregistered");
		assert!(
			matches!(error, ForgeError::Unregistered { ref host, op: ForgeOp::RepoView } if host == "gitlab.example"),
			"{error}"
		);
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn gitlab_com_is_registered_dyn_even_when_unbound() {
		let dir = tempfile::tempdir().expect("temp");
		let router = forge_router(
			LiveForgeDeviceClient::default(),
			&default_dyn_hosts(),
			dir.path().to_path_buf(),
			crate::forge_worktree::worktree_root(dir.path()),
		);
		let repo = RepoRef::try_new(GITLAB_HOST, "acme", "widget").unwrap();
		let error = router
			.view_repo(&repo)
			.await
			.expect_err("unbound live client");
		assert!(
			matches!(error, ForgeError::Unavailable { op: ForgeOp::RepoView }),
			"gitlab.com must be registered on the dyn arm: {error}"
		);
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn github_com_is_registered_dyn_even_when_unbound() {
		let dir = tempfile::tempdir().expect("temp");
		let router = forge_router(
			LiveForgeDeviceClient::default(),
			&default_dyn_hosts(),
			dir.path().to_path_buf(),
			crate::forge_worktree::worktree_root(dir.path()),
		);
		let repo = RepoRef::try_new(GITHUB_DYN_HOST, "acme", "widget").unwrap();
		let error = router
			.view_repo(&repo)
			.await
			.expect_err("unbound live client");
		assert!(
			matches!(error, ForgeError::Unavailable { op: ForgeOp::RepoView }),
			"github.com must be registered on the dyn arm: {error}"
		);
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn self_hosted_gitlab_convar_routes_change_request_to_dyn() {
		use std::pin::Pin;

		use parking_lot::Mutex as ParkingMutex;
		use serde_json::{Value, json};

		struct RecordingInvoker {
			calls: Arc<ParkingMutex<Vec<(String, Value)>>>,
		}

		impl ForgeDeviceInvoker for RecordingInvoker {
			fn invoke(
				&self,
				path: &str,
				args: Value,
			) -> Pin<Box<dyn std::future::Future<Output = Result<Value, ForgeError>> + Send + '_>> {
				self.calls.lock().push((path.to_string(), args));
				let cr = json!({
					"repo": { "host": "gitlab.example.com", "owner": "acme", "name": "widget" },
					"number": 7,
					"title": "Fix the widget",
					"body": "details",
					"state": "open",
					"draft": false,
					"base_ref": "main",
					"head_ref": "topic",
					"head_commit": "bbb",
					"base_commit": "aaa",
					"web_url": "https://gitlab.example.com/acme/widget/-/merge_requests/7"
				});
				Box::pin(std::future::ready(Ok(cr)))
			}
		}

		let host = "gitlab.example.com";
		let dyn_hosts = ForgeDynHosts::from_sources(host, None);
		assert!(dyn_hosts.contains(host));

		let devices = LiveForgeDeviceClient::default();
		let calls = Arc::new(ParkingMutex::new(Vec::new()));
		devices.bind(Arc::new(RecordingInvoker { calls: Arc::clone(&calls) }));

		let dir = tempfile::tempdir().expect("temp");
		let router = forge_router(
			devices,
			&dyn_hosts,
			dir.path().to_path_buf(),
			crate::forge_worktree::worktree_root(dir.path()),
		);
		let repo = RepoRef::try_new(host, "acme", "widget").unwrap();
		let cr = router
			.view_change_request(&repo, 7)
			.await
			.expect("self-hosted GitLab must hit the dyn arm");
		assert_eq!(cr.number, 7);
		assert_eq!(cr.title.as_str(), "Fix the widget");
		assert_eq!(cr.repo.host.as_str(), host);

		let recorded = calls.lock().clone();
		assert_eq!(recorded.len(), 1);
		assert_eq!(recorded[0].0, forge_device_paths::CHANGE_REQUEST_VIEW);
		assert_eq!(recorded[0].1["number"], 7);

		let github_repo = RepoRef::try_new(GITHUB_DYN_HOST, "acme", "widget").unwrap();
		let github_cr = router
			.view_change_request(&github_repo, 1)
			.await
			.expect("github.com must hit the dyn arm");
		assert_eq!(github_cr.number, 7);
		assert_eq!(calls.lock().len(), 2, "github.com must hit the dyn client");
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn checkout_host_registers_on_dyn_even_when_unlisted() {
		use std::pin::Pin;

		use serde_json::{Value, json};

		struct CountingInvoker {
			hits: Arc<std::sync::atomic::AtomicUsize>,
		}

		impl ForgeDeviceInvoker for CountingInvoker {
			fn invoke(
				&self,
				_path: &str,
				_args: Value,
			) -> Pin<Box<dyn std::future::Future<Output = Result<Value, ForgeError>> + Send + '_>> {
				self.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
				Box::pin(std::future::ready(Ok(json!({
					"repo": { "host": "ghe.example.com", "owner": "acme", "name": "widget" },
					"number": 1,
					"title": "t",
					"body": "",
					"state": "open",
					"draft": false,
					"base_ref": "main",
					"head_ref": "topic",
					"head_commit": "b",
					"base_commit": "a",
					"web_url": "https://ghe.example.com/acme/widget/pull/1"
				}))))
			}
		}

		let dir = tempfile::tempdir().expect("temp");
		let root = dir.path();
		omp_vcs::git::init(root).expect("init");
		let repo = omp_vcs::git::GitRepo::discover(root)
			.expect("discover")
			.expect("repo");
		repo
			.remote_add("origin", "https://ghe.example.com/acme/widget.git")
			.expect("remote");

		let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let devices = LiveForgeDeviceClient::default();
		devices.bind(Arc::new(CountingInvoker { hits: Arc::clone(&hits) }));

		// Unlisted checkout host still lands on Dyn (honest Unavailable without
		// mounts; here the bound fake answers).
		let router = forge_router(
			devices,
			&default_dyn_hosts(),
			root.to_path_buf(),
			crate::forge_worktree::worktree_root(root),
		);
		let forge_repo = RepoRef::try_new("ghe.example.com", "acme", "widget").unwrap();
		let cr = router
			.view_change_request(&forge_repo, 1)
			.await
			.expect("unlisted checkout host on Dyn");
		assert_eq!(cr.number, 1);
		assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
	}

	#[cfg(feature = "cap-ci")]
	#[tokio::test]
	async fn ci_writes_and_artifacts_are_not_capable() {
		let dir = tempfile::tempdir().expect("temp");
		let client = LiveCiDeviceClient::default();
		client.bind(Arc::new(RecordingCiCapsInvoker));
		let router = super::ci_router(client, dir.path());
		let run = run("42");
		let provider = run.provider.clone();
		for op in [
			CiOp::Cancel,
			CiOp::RerunFailed,
			CiOp::RerunJob,
			CiOp::Trigger,
			CiOp::Artifacts,
			CiOp::PlayManual,
		] {
			let error = match op {
				CiOp::Cancel => router.cancel(&run).await.err(),
				CiOp::RerunFailed => router.rerun_failed(&run).await.err(),
				CiOp::RerunJob => router.rerun_job(&run).await.err(),
				CiOp::Trigger => router
					.trigger(Trigger::try_new(provider.clone(), "ci", None::<(Str, Str)>).unwrap())
					.await
					.err(),
				CiOp::Artifacts => router.artifacts(&run).await.err(),
				CiOp::PlayManual => router.play_manual(&run).await.err(),
				CiOp::Status | CiOp::LogStream => None,
			};
			assert!(
				matches!(error, Some(CiError::NotCapable { op: got, .. }) if got == op),
				"{op} yielded {error:?}"
			);
		}
		let caps = router.capabilities(&provider).await.expect("caps");
		assert!(caps.log_stream);
		assert!(!caps.cancel);
		assert!(!caps.artifacts);
		assert!(caps.allows(CiOp::Status));
	}

	#[cfg(feature = "cap-ci")]
	struct RecordingCiCapsInvoker;

	#[cfg(feature = "cap-ci")]
	impl super::CiDeviceInvoker for RecordingCiCapsInvoker {
		fn invoke(
			&self,
			path: &str,
			_args: serde_json::Value,
		) -> std::pin::Pin<
			Box<dyn std::future::Future<Output = Result<serde_json::Value, CiError>> + Send + '_>,
		> {
			use omp_ci::Capabilities;
			let result = match path {
				"ci/capabilities" => {
					Ok(serde_json::to_value(Capabilities { log_stream: true, ..Capabilities::none() })
						.expect("caps"))
				},
				other => Err(CiError::Unavailable { op: super::ci_op_for_path(other) }),
			};
			Box::pin(std::future::ready(result))
		}
	}

	#[cfg(feature = "cap-ci")]
	#[tokio::test]
	async fn gitlab_com_ci_is_registered_dyn_even_when_unbound() {
		let dir = tempfile::tempdir().expect("temp");
		let router = super::ci_router(LiveCiDeviceClient::default(), dir.path());
		let run = RunRef::try_new(CiProvider::try_new(CI_DYN_HOST).unwrap(), "1", None).unwrap();
		let error = router.status(&run).await.expect_err("unbound live client");
		assert!(
			matches!(error, CiError::Unavailable { op: CiOp::Status }),
			"gitlab.com must be registered on the dyn CI arm: {error}"
		);
	}

	#[cfg(feature = "cap-ci")]
	#[tokio::test]
	async fn github_com_ci_is_registered_dyn_even_when_unbound() {
		let dir = tempfile::tempdir().expect("temp");
		let router = super::ci_router(LiveCiDeviceClient::default(), dir.path());
		let run = RunRef::try_new(CiProvider::try_new(CI_GITHUB_HOST).unwrap(), "1", None).unwrap();
		let error = router.status(&run).await.expect_err("unbound live client");
		assert!(
			matches!(error, CiError::Unavailable { op: CiOp::Status }),
			"github.com must be registered on the dyn CI arm: {error}"
		);
		assert!(
			!matches!(error, CiError::Unregistered { .. }),
			"github.com must not fall off the router: {error}"
		);
	}

	#[cfg(feature = "cap-ci")]
	#[tokio::test]
	async fn github_com_ci_status_and_log_via_bound_client() {
		use std::sync::atomic::{AtomicUsize, Ordering};

		use parking_lot::Mutex;

		struct Recording {
			calls: Arc<Mutex<Vec<String>>>,
			hits:  Arc<AtomicUsize>,
		}

		impl super::CiDeviceInvoker for Recording {
			fn invoke(
				&self,
				path: &str,
				_args: serde_json::Value,
			) -> std::pin::Pin<
				Box<dyn std::future::Future<Output = Result<serde_json::Value, CiError>> + Send + '_>,
			> {
				self.hits.fetch_add(1, Ordering::SeqCst);
				self.calls.lock().push(path.to_owned());
				let host = CI_GITHUB_HOST;
				let result = match path {
					"ci/status" => {
						let pipeline = Pipeline::new(
							CiProvider::try_new(host).unwrap(),
							"42",
							"ci",
							CiStatus::Failure,
							[Job::new("7", "test", CiStatus::Failure)],
						);
						Ok(serde_json::to_value(pipeline).expect("pipeline"))
					},
					"ci/log_stream" => {
						let page = LogPage::new(
							RunRef::try_new(CiProvider::try_new(host).unwrap(), "42", Some(Str::new("7")))
								.unwrap(),
							"boom\n",
							true,
						);
						Ok(serde_json::to_value(page).expect("log"))
					},
					other => Err(CiError::Unavailable { op: super::ci_op_for_path(other) }),
				};
				Box::pin(std::future::ready(result))
			}
		}

		let calls = Arc::new(Mutex::new(Vec::new()));
		let hits = Arc::new(AtomicUsize::new(0));
		let client = LiveCiDeviceClient::default();
		client.bind(Arc::new(Recording { calls: Arc::clone(&calls), hits: Arc::clone(&hits) }));
		let dir = tempfile::tempdir().expect("temp");
		let router = super::ci_router(client, dir.path());
		let run = run("42");
		let pipeline = router.status(&run).await.expect("status via dyn");
		assert_eq!(pipeline.id.as_str(), "42");
		assert_eq!(pipeline.status, CiStatus::Failure);
		let mut scoped = run.clone();
		scoped.job = Some(Str::new("7"));
		let page = router.log_stream(&scoped).await.expect("log via dyn");
		assert_eq!(page.text.as_str(), "boom\n");
		assert_eq!(hits.load(Ordering::SeqCst), 2);
		assert_eq!(calls.lock().as_slice(), ["ci/status", "ci/log_stream"]);
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn forge_checkout_gate_requires_a_dyn_host() {
		let hosts = default_dyn_hosts();

		let dir = tempfile::tempdir().expect("temp");
		let root = dir.path();
		omp_vcs::git::init(root).expect("init");
		assert!(!super::serves_forge_checkout(root, &hosts));

		let ghe = tempfile::tempdir().expect("temp");
		omp_vcs::git::init(ghe.path()).expect("init");
		omp_vcs::git::GitRepo::discover(ghe.path())
			.expect("discover")
			.expect("repo")
			.remote_add("origin", "https://ghe.example/acme/widget.git")
			.expect("ghe remote");
		assert!(!super::serves_forge_checkout(ghe.path(), &hosts));

		let gitlab = tempfile::tempdir().expect("temp");
		omp_vcs::git::init(gitlab.path()).expect("init");
		omp_vcs::git::GitRepo::discover(gitlab.path())
			.expect("discover")
			.expect("repo")
			.remote_add("origin", "https://gitlab.com/acme/widget.git")
			.expect("gitlab remote");
		assert!(super::serves_forge_checkout(gitlab.path(), &hosts));

		let github = tempfile::tempdir().expect("temp");
		omp_vcs::git::init(github.path()).expect("init");
		omp_vcs::git::GitRepo::discover(github.path())
			.expect("discover")
			.expect("repo")
			.remote_add("origin", "https://github.com/acme/widget.git")
			.expect("github remote");
		assert!(super::serves_forge_checkout(github.path(), &hosts));
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn view_branch_returns_none_without_a_dyn_host() {
		let dir = tempfile::tempdir().expect("temp");
		let root = dir.path();
		omp_vcs::git::init(root).expect("init");
		let repo = omp_vcs::git::GitRepo::discover(root)
			.expect("discover")
			.expect("repo");
		repo
			.remote_add("origin", "https://ghe.example/acme/widget.git")
			.expect("remote");
		let forge = super::CheckoutForge::new(
			root.to_path_buf(),
			root.to_path_buf(),
			LiveForgeDeviceClient::default(),
			default_dyn_hosts(),
		);
		assert!(
			forge
				.view_branch("main")
				.await
				.expect("unlisted stays unread")
				.is_none()
		);
	}

	#[cfg(feature = "cap-forge")]
	#[tokio::test]
	async fn view_branch_searches_then_views_single_open_hit() {
		use std::pin::Pin;

		use parking_lot::Mutex as ParkingMutex;
		use serde_json::{Value, json};

		struct BranchInvoker {
			calls: Arc<ParkingMutex<Vec<(String, Value)>>>,
		}

		impl ForgeDeviceInvoker for BranchInvoker {
			fn invoke(
				&self,
				path: &str,
				args: Value,
			) -> Pin<Box<dyn std::future::Future<Output = Result<Value, ForgeError>> + Send + '_>> {
				self.calls.lock().push((path.to_string(), args.clone()));
				let result = match path {
					p if p == forge_device_paths::SEARCH_CHANGE_REQUESTS => {
						let page = SearchPage::new(SearchKind::ChangeRequests, [omp_forge::SearchHit {
							repo:    RepoRef::try_new(GITHUB_DYN_HOST, "acme", "widget").unwrap(),
							title:   Str::new_static("topic"),
							web_url: Str::new_static("https://github.com/acme/widget/pull/9"),
							number:  Some(9),
							commit:  None,
							state:   Str::new_static("open"),
						}]);
						Ok(serde_json::to_value(page).expect("page"))
					},
					p if p == forge_device_paths::CHANGE_REQUEST_VIEW => Ok(json!({
						"repo": { "host": "github.com", "owner": "acme", "name": "widget" },
						"number": 9,
						"title": "topic",
						"body": "",
						"state": "open",
						"draft": false,
						"base_ref": "main",
						"head_ref": "topic",
						"head_commit": "bbb",
						"base_commit": "aaa",
						"web_url": "https://github.com/acme/widget/pull/9"
					})),
					other => Err(ForgeError::Unavailable { op: super::forge_op_for_path(other) }),
				};
				Box::pin(std::future::ready(result))
			}
		}

		let dir = tempfile::tempdir().expect("temp");
		let root = dir.path();
		omp_vcs::git::init(root).expect("init");
		let repo = omp_vcs::git::GitRepo::discover(root)
			.expect("discover")
			.expect("repo");
		repo
			.remote_add("origin", "https://github.com/acme/widget.git")
			.expect("remote");

		let calls = Arc::new(ParkingMutex::new(Vec::new()));
		let devices = LiveForgeDeviceClient::default();
		devices.bind(Arc::new(BranchInvoker { calls: Arc::clone(&calls) }));
		let forge = super::CheckoutForge::new(
			root.to_path_buf(),
			root.to_path_buf(),
			devices,
			default_dyn_hosts(),
		);
		let cr = forge
			.view_branch("topic")
			.await
			.expect("branch")
			.expect("open hit");
		assert_eq!(cr.number, 9);
		assert_eq!(cr.head_ref.as_str(), "topic");
		let recorded = calls.lock().clone();
		assert_eq!(recorded.len(), 2);
		assert_eq!(recorded[0].0, forge_device_paths::SEARCH_CHANGE_REQUESTS);
		let query: SearchQuery =
			serde_json::from_value(recorded[0].1["query"].clone()).expect("query");
		assert_eq!(query.kind, SearchKind::ChangeRequests);
		assert_eq!(query.text.as_str(), "head:topic state:open");
		assert_eq!(recorded[1].0, forge_device_paths::CHANGE_REQUEST_VIEW);
		assert_eq!(recorded[1].1["number"], 9);
	}
}
