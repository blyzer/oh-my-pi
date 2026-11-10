//! Observer-local git facts for the status band: the checked-out branch and
//! whether the worktree is dirty, kept live by watching the repository's head
//! marker with a stat-poll safety net.
//!
//! Nothing here is journaled (ADR 0005): the watcher is a projection input of
//! one observer, exactly like the terminal size.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use flume::{Receiver, Sender};
use notify::{RecursiveMode, Watcher as _};
use omp_core::Str;
use omp_vcs::git::GitRepo;
use tokio::{task::JoinHandle, time};

use crate::{
	overlays::services::Services,
	status_band::{GitStatus, PullRequest, WorktreeLabel},
};

/// Safety-poll cadence when the head watcher is silent or unavailable.
pub const GIT_POLL: Duration = Duration::from_secs(5);

/// Backstop so a forge read cannot stall observer-local status refresh.
///
/// The composed forge host cancels its own read at five seconds.
/// This wait is longer so that result still arrives.
const PR_TIMEOUT: Duration = Duration::from_secs(6);

/// Coalescing window after a filesystem event before the repository is
/// re-probed, so one `git checkout` (HEAD, index, ORIG_HEAD) probes once.
const SETTLE: Duration = Duration::from_millis(100);

/// Repository facts the band paints.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GitFacts {
	/// Checked-out local branch; `None` outside a checkout or on a detached
	/// HEAD.
	pub branch:       Option<Str>,
	/// Exact porcelain counts. `None` only for the cheap synchronous launch
	/// snapshot before the first background probe.
	pub status:       Option<GitStatus>,
	/// Change request for the branch. Refreshed when the branch name changes.
	pub pull_request: Option<PullRequest>,
	/// Linked-worktree identity for the compact path segment.
	pub worktree:     Option<WorktreeLabel>,
}

/// A running head watcher. Dropping it stops the task and releases the
/// filesystem watch.
pub struct GitWatch {
	launch: GitFacts,
	task:   JoinHandle<()>,
}

impl GitWatch {
	/// Starts watching the repository containing `project` with the
	/// production [`GIT_POLL`] cadence. `None` when `project` is not inside a
	/// git checkout or no tokio runtime is current.
	#[must_use]
	pub fn start(project: &Path) -> Option<(Self, Receiver<GitFacts>)> {
		Self::start_with(project, GIT_POLL)
	}

	/// [`Self::start`] with an explicit safety-poll cadence.
	#[must_use]
	pub fn start_with(project: &Path, poll: Duration) -> Option<(Self, Receiver<GitFacts>)> {
		Self::open(project, poll, None)
	}

	/// [`Self::start`] with the composed forge host that reads change requests.
	#[must_use]
	pub fn start_with_services(
		project: &Path,
		services: Arc<dyn Services>,
	) -> Option<(Self, Receiver<GitFacts>)> {
		Self::open(project, GIT_POLL, Some(services))
	}

	fn open(
		project: &Path,
		poll: Duration,
		services: Option<Arc<dyn Services>>,
	) -> Option<(Self, Receiver<GitFacts>)> {
		let handle = tokio::runtime::Handle::try_current().ok()?;
		let repo = Arc::new(GitRepo::discover(project).ok().flatten()?);
		// The branch and linked-worktree identity are metadata reads; porcelain
		// status and the change-request lookup stay off the launch path.
		let launch = GitFacts {
			branch:       branch_of(&repo),
			status:       None,
			pull_request: None,
			worktree:     worktree_label(&repo),
		};
		let (events, changes) = flume::unbounded::<()>();
		let watcher = HeadWatcher::install(&repo, events);
		let (sender, receiver) = flume::unbounded();
		let task =
			handle.spawn(watch_loop(repo, watcher, launch.clone(), changes, sender, poll, services));
		Some((Self { launch, task }, receiver))
	}

	/// Facts probed synchronously at start: the branch is exact, `dirty` is
	/// resolved by the first delivered [`GitFacts`].
	#[must_use]
	pub const fn launch(&self) -> &GitFacts {
		&self.launch
	}
}

impl Drop for GitWatch {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// Platform watch over the head marker (in-place writes) and the directory
/// holding it (atomic replacement by git/jj, which would silence a watch
/// bound to the old inode). `None` when the platform watcher cannot be
/// installed; the poll still runs.
struct HeadWatcher {
	watcher: notify::RecommendedWatcher,
	target:  PathBuf,
}

impl HeadWatcher {
	fn install(repo: &GitRepo, events: Sender<()>) -> Option<Self> {
		let target = repo.head_watch_target();
		let directory = if target.is_dir() {
			target.as_path()
		} else {
			target.parent()?
		};
		let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
			if event.is_ok() {
				let _ = events.send(());
			}
		})
		.ok()?;
		watcher.watch(directory, RecursiveMode::NonRecursive).ok()?;
		let mut watcher = Self { watcher, target };
		watcher.rearm();
		Some(watcher)
	}

	/// Re-binds the marker watch to whatever inode now sits at the target
	/// path; a failure just leaves the directory watch and the poll.
	fn rearm(&mut self) {
		if self.target.is_dir() {
			return;
		}
		let _ = self.watcher.unwatch(&self.target);
		let _ = self
			.watcher
			.watch(&self.target, RecursiveMode::NonRecursive);
	}
}

async fn watch_loop(
	repo: Arc<GitRepo>,
	mut watcher: Option<HeadWatcher>,
	mut last: GitFacts,
	changes: Receiver<()>,
	sender: Sender<GitFacts>,
	poll: Duration,
	services: Option<Arc<dyn Services>>,
) {
	// Deliver the status counts and change request the launch probe skipped.
	// The forge read follows the branch name. A status probe can wake the
	// head watch by touching the git directory; that must not read again.
	// A failed read for one branch waits [`PR_TIMEOUT`] before that same
	// branch is tried again. A settled answer stays until the branch changes.
	let mut due = true;
	let mut looked_up: Option<Option<Str>> = None;
	let mut hold: Option<(Option<Str>, std::time::Instant)> = None;
	loop {
		if !due {
			tokio::select! {
				change = changes.recv_async() => {
					if change.is_ok() {
						time::sleep(SETTLE).await;
						while changes.try_recv().is_ok() {}
						if let Some(watcher) = watcher.as_mut() {
							watcher.rearm();
						}
					}
				},
				() = time::sleep(poll) => {},
			}
		}
		due = false;
		let probe = Arc::clone(&repo);
		let Ok(mut facts) = tokio::task::spawn_blocking(move || probe_facts(&probe)).await else {
			return;
		};
		let waiting = hold.as_ref().is_some_and(|(branch, until)| {
			branch == &facts.branch && std::time::Instant::now() < *until
		});
		if looked_up.as_ref() == Some(&facts.branch) || waiting {
			facts.pull_request.clone_from(&last.pull_request);
		} else {
			match lookup_change_request(services.as_ref(), facts.branch.as_ref()).await {
				BranchRead::Settled(pull) => {
					facts.pull_request = pull;
					looked_up = Some(facts.branch.clone());
					hold = None;
				},
				BranchRead::Again => {
					facts.pull_request.clone_from(&last.pull_request);
					hold = Some((facts.branch.clone(), std::time::Instant::now() + PR_TIMEOUT));
				},
			}
		}
		if facts != last {
			last = facts.clone();
			if sender.send(facts).is_err() {
				return;
			}
		}
	}
}

fn branch_of(repo: &GitRepo) -> Option<Str> {
	repo.current_branch().ok().flatten().map(Str::from)
}

fn probe_facts(repo: &GitRepo) -> GitFacts {
	let status = repo.status_summary().ok().map(|status| GitStatus {
		staged:    status.staged,
		unstaged:  status.unstaged,
		untracked: status.untracked,
	});
	GitFacts { branch: branch_of(repo), status, pull_request: None, worktree: worktree_label(repo) }
}

fn worktree_label(repo: &GitRepo) -> Option<WorktreeLabel> {
	let linked = repo.linked_worktree()?;
	let project = repo
		.primary_root()
		.file_name()
		.and_then(|name| name.to_str())
		.map(Str::new)?;
	let worktree = linked
		.root
		.file_name()
		.and_then(|name| name.to_str())
		.map(Str::new)?;
	Some(WorktreeLabel { project, worktree })
}

/// Outcome of one forge read.
enum BranchRead {
	/// The host answered. `None` means this branch has no open change request.
	Settled(Option<PullRequest>),
	/// The host did not answer. The watcher retries after [`PR_TIMEOUT`].
	Again,
}

/// Bounded change-request lookup. It runs when the checked-out branch
/// changes, including the first probe. A settled answer is not read again
/// on the safety poll.
///
/// The application implements [`Services::branch_change_request`] with the
/// environment's forge `Delegated` router (`view_change_request`).
async fn lookup_change_request(
	services: Option<&Arc<dyn Services>>,
	branch: Option<&Str>,
) -> BranchRead {
	let Some(services) = services else {
		return BranchRead::Settled(None);
	};
	let Some(branch) = branch.filter(|branch| !branch.is_empty()) else {
		return BranchRead::Settled(None);
	};
	let Ok(pending) = services.branch_change_request(branch) else {
		return BranchRead::Again;
	};
	match time::timeout(PR_TIMEOUT, pending.recv_async()).await {
		Ok(Ok(Ok(pull))) => BranchRead::Settled(pull),
		_ => BranchRead::Again,
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	/// Generous bound for a platform watcher delivery plus the settle window.
	const WATCHER: Duration = Duration::from_secs(3);

	fn seeded_repo(root: &Path) -> (GitRepo, Str) {
		omp_vcs::git::init(root).expect("git init");
		fs::write(root.join("note.txt"), "one\n").expect("seed file");
		let repo = GitRepo::discover(root).expect("discover").expect("repo");
		let branch = repo.current_branch().expect("branch").expect("born branch");
		(repo, Str::from(branch))
	}

	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn head_moves_reach_the_receiver_through_the_watcher() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let root = scratch.path();
		let (repo, seeded) = seeded_repo(root);
		// A poll this slow cannot be what delivers below.
		let (watch, facts) = GitWatch::start_with(root, Duration::from_secs(60)).expect("watch");
		assert_eq!(watch.launch().branch, Some(seeded.clone()));
		assert!(watch.launch().status.is_none());

		// The launch probe skips the porcelain walk; the first delivery resolves it.
		let first = facts.recv_timeout(WATCHER).expect("initial status probe");
		assert_eq!(first.branch, Some(seeded));
		assert!(first.status.is_some_and(GitStatus::dirty));

		// In-place rewrite of the marker.
		let head = repo.info().head_path.clone();
		fs::write(&head, "ref: refs/heads/other\n").expect("move HEAD");
		let moved = facts.recv_timeout(WATCHER).expect("in-place HEAD write");
		assert_eq!(moved.branch.as_deref(), Some("other"));

		// git's own checkout path: write a lock file, rename it over HEAD.
		let lock = head.with_extension("lock");
		fs::write(&lock, "ref: refs/heads/third\n").expect("lock");
		fs::rename(&lock, &head).expect("replace HEAD");
		let replaced = facts.recv_timeout(WATCHER).expect("renamed HEAD");
		assert_eq!(replaced.branch.as_deref(), Some("third"));

		// And again in place, proving the watch survived the inode swap.
		fs::write(&head, "ref: refs/heads/fourth\n").expect("move HEAD again");
		let again = facts.recv_timeout(WATCHER).expect("post-rename HEAD write");
		assert_eq!(again.branch.as_deref(), Some("fourth"));
		assert!(again.status.is_some_and(GitStatus::dirty));
		drop(watch);
	}

	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn worktree_edits_reach_the_receiver_through_the_poll() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let root = scratch.path();
		let (_, seeded) = seeded_repo(root);
		let poll = Duration::from_millis(200);
		let (watch, facts) = GitWatch::start_with(root, poll).expect("watch");
		let first = facts.recv_timeout(poll * 10).expect("initial status probe");
		assert_eq!(first.branch, Some(seeded.clone()));
		assert!(first.status.is_some_and(GitStatus::dirty));

		// Worktree files are outside the watched git dir: only the poll sees
		// them.
		fs::remove_file(root.join("note.txt")).expect("clean worktree");
		let clean = facts.recv_timeout(poll * 10).expect("clean probe");
		assert_eq!(clean.branch, Some(seeded.clone()));
		assert!(clean.status.is_some_and(|status| !status.dirty()));

		fs::write(root.join("again.txt"), "two\n").expect("dirty again");
		let dirty = facts.recv_timeout(poll * 10).expect("dirty probe");
		assert_eq!(dirty.branch, Some(seeded));
		assert!(dirty.status.is_some_and(GitStatus::dirty));

		assert!(facts.recv_timeout(poll * 3).is_err(), "unchanged facts are not re-sent");
		drop(watch);
	}

	#[tokio::test]
	async fn outside_a_checkout_there_is_nothing_to_watch() {
		let scratch = tempfile::tempdir().expect("tempdir");
		assert!(GitWatch::start(scratch.path()).is_none());
	}

	struct FixedRequest {
		calls: std::sync::atomic::AtomicUsize,
		pull:  PullRequest,
	}

	impl crate::overlays::services::Services for FixedRequest {
		fn branch_change_request(
			&self,
			_branch: &str,
		) -> crate::overlays::services::ServiceResult<
			crate::overlays::services::Pending<Option<PullRequest>>,
		> {
			self
				.calls
				.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			let (tx, rx) = flume::bounded(1);
			let _ = tx.send(Ok(Some(self.pull.clone())));
			Ok(rx)
		}
	}

	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn change_request_arrives_from_the_forge_lookup_and_not_on_the_poll() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let root = scratch.path();
		let (_, seeded) = seeded_repo(root);
		let pull = PullRequest { number: 7, url: Str::new_static("https://example.test/pull/7") };
		let services = std::sync::Arc::new(FixedRequest {
			calls: std::sync::atomic::AtomicUsize::new(0),
			pull:  pull.clone(),
		});
		let poll = Duration::from_millis(200);
		let (watch, facts) =
			GitWatch::open(root, poll, Some(services.clone() as std::sync::Arc<dyn Services>))
				.expect("watch");
		let first = facts.recv_timeout(WATCHER).expect("initial probe");
		assert_eq!(first.branch, Some(seeded.clone()));
		assert_eq!(first.pull_request, Some(pull));
		assert_eq!(services.calls.load(std::sync::atomic::Ordering::Relaxed), 1);

		fs::write(root.join("again.txt"), "two\n").expect("dirty");
		let dirty = facts.recv_timeout(poll * 10).expect("dirty probe");
		assert!(dirty.status.is_some_and(GitStatus::dirty));
		assert_eq!(dirty.pull_request.as_ref().map(|pull| pull.number), Some(7));
		assert_eq!(services.calls.load(std::sync::atomic::Ordering::Relaxed), 1);

		let head = root.join(".git/HEAD");
		fs::write(&head, "ref: refs/heads/other\n").expect("move HEAD");
		let moved = facts.recv_timeout(WATCHER).expect("branch move");
		assert_eq!(moved.branch.as_deref(), Some("other"));
		assert_eq!(moved.pull_request.as_ref().map(|pull| pull.number), Some(7));
		assert_eq!(services.calls.load(std::sync::atomic::Ordering::Relaxed), 2);
		drop(watch);
	}

	struct FailingLookup {
		calls: std::sync::atomic::AtomicUsize,
	}

	impl crate::overlays::services::Services for FailingLookup {
		fn branch_change_request(
			&self,
			_branch: &str,
		) -> crate::overlays::services::ServiceResult<
			crate::overlays::services::Pending<Option<PullRequest>>,
		> {
			self
				.calls
				.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			Err(crate::overlays::services::ServiceError::Unavailable { feed: "change request" })
		}
	}

	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn a_failed_change_request_read_is_not_retried_on_the_safety_poll() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let root = scratch.path();
		let _ = seeded_repo(root);
		let services =
			std::sync::Arc::new(FailingLookup { calls: std::sync::atomic::AtomicUsize::new(0) });
		let poll = Duration::from_millis(200);
		let (watch, facts) =
			GitWatch::open(root, poll, Some(services.clone() as std::sync::Arc<dyn Services>))
				.expect("watch");
		let first = facts.recv_timeout(WATCHER).expect("initial probe");
		assert!(first.pull_request.is_none());
		assert_eq!(services.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
		tokio::time::sleep(poll * 4).await;
		assert_eq!(services.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
		drop(watch);
	}
}
