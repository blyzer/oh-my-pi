//! Forge-neutral change-request worktree checkout and push (ADR 0049 D2.4).
//!
//! Vendor adapters return plan data (clone URL, refs, SHAs). This module
//! performs fetch, worktree creation under the repository mutation lock, and
//! push via `omp-vcs`. It does not speak a forge product name and does not
//! depend on `omp-forge` so both the dyn bridge and residual `GithubService`
//! Operation paths can share one git implementation.

use std::{
	fs,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_core::Str;
use omp_vcs::{PushOptions, git::GitRepo};
use serde::{Deserialize, Serialize};
use tokio::task;
use tokio_util::sync::CancellationToken;

/// Directory name under the envd state dir for isolated change-request
/// worktrees.
pub const WORKTREE_DIR: &str = "forge-worktrees";

/// Absolute worktree root under `state_dir`.
#[must_use]
pub fn worktree_root(state_dir: &Path) -> PathBuf {
	state_dir.join(WORKTREE_DIR)
}

/// Worktree path for one change-request number.
#[must_use]
pub fn worktree_path(worktree_root: &Path, number: u64) -> PathBuf {
	worktree_root.join(format!("cr-{number}"))
}

fn metadata_path(worktree_root: &Path, number: u64) -> PathBuf {
	worktree_root.join(format!("cr-{number}.json"))
}

fn local_branch(number: u64) -> String {
	format!("cr-{number}")
}

fn fetch_branch(number: u64) -> String {
	format!("omp/forge-fetch/cr-{number}")
}

/// Vendor + request facts needed to check a change request out.
#[derive(Debug, Clone)]
pub struct CheckoutInput {
	/// `host/owner/name` identity recorded beside the worktree.
	pub repo_identity: String,
	/// Change-request number.
	pub number:        u64,
	/// HTTPS (or other git) clone URL for the head repository.
	pub clone_url:     String,
	/// Base branch name (returned on the receipt; unused by git).
	pub base_ref:      String,
	/// Head branch name on the forge.
	pub head_ref:      String,
	/// Head commit identity to check out.
	pub head_sha:      String,
	/// Reset an existing local branch to the remote head.
	pub force:         bool,
}

/// Result of a successful core checkout.
#[derive(Debug, Clone)]
pub struct CheckoutOutput {
	/// Absolute worktree path.
	pub worktree: PathBuf,
	/// Head SHA that was planned (and fetched).
	pub head_sha: String,
	/// Base branch name from the plan.
	pub base_ref: String,
	/// Head branch name from the plan.
	pub head_ref: String,
	/// Change-request number.
	pub number:   u64,
}

/// Vendor + request facts needed to push a checked-out change request.
#[derive(Debug, Clone)]
pub struct PushInput {
	/// `host/owner/name` identity that must match checkout metadata.
	pub repo_identity:    String,
	/// Change-request number.
	pub number:           u64,
	/// HTTPS (or other git) clone URL of the head repository.
	pub clone_url:        String,
	/// Remote branch name to update (empty → use checkout metadata).
	pub head_ref:         String,
	/// Refuse the push when the remote moved.
	pub force_with_lease: bool,
}

/// Result of a successful core push.
#[derive(Debug, Clone)]
pub struct PushOutput {
	/// Change-request number.
	pub number:      u64,
	/// Commit identity now at the remote head.
	pub head_commit: Str,
}

/// Failures from forge-neutral worktree I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
	/// Caller cancelled while waiting on the mutation lock or a git worker.
	#[error("forge worktree operation was cancelled")]
	Cancelled,
	/// Checkout metadata is missing or does not match the requested repository.
	#[error("forge worktree checkout metadata is missing or mismatched")]
	CheckoutMissing,
	/// Plan or prep fields required for git were empty or inconsistent.
	#[error("forge worktree plan is incomplete")]
	IncompletePlan,
	/// Local git / filesystem operation failed.
	#[error("forge worktree git operation failed")]
	GitFailed,
}

#[derive(Debug, Deserialize, Serialize)]
struct CheckoutMetadata {
	repo:          String,
	clone_url:     String,
	head:          String,
	#[serde(default)]
	expected_head: String,
}

/// Fetches `input` into an isolated worktree under `worktree_root`.
pub async fn checkout(
	project_root: &Path,
	worktree_root: &Path,
	input: &CheckoutInput,
	cancel: &CancellationToken,
) -> Result<CheckoutOutput, Error> {
	if input.clone_url.is_empty() || input.head_ref.is_empty() || input.head_sha.is_empty() {
		return Err(Error::IncompletePlan);
	}

	fs::create_dir_all(worktree_root).map_err(|_| Error::GitFailed)?;
	let path = worktree_path(worktree_root, input.number);
	tokio::select! {
		result = checkout_git(
			project_root,
			&path,
			input.number,
			&input.clone_url,
			&input.head_ref,
			input.force,
			cancel.clone(),
		) => result?,
		() = cancel.cancelled() => return Err(Error::Cancelled),
	}

	let metadata = CheckoutMetadata {
		repo:          input.repo_identity.clone(),
		clone_url:     input.clone_url.clone(),
		head:          input.head_ref.clone(),
		expected_head: input.head_sha.clone(),
	};
	fs::write(
		metadata_path(worktree_root, input.number),
		serde_json::to_vec(&metadata).expect("checkout metadata serializes"),
	)
	.map_err(|_| Error::GitFailed)?;

	Ok(CheckoutOutput {
		worktree: path,
		head_sha: input.head_sha.clone(),
		base_ref: input.base_ref.clone(),
		head_ref: input.head_ref.clone(),
		number:   input.number,
	})
}

/// Pushes the checked-out change-request worktree.
pub async fn push(
	worktree_root: &Path,
	input: &PushInput,
	cancel: &CancellationToken,
) -> Result<PushOutput, Error> {
	if input.clone_url.is_empty() {
		return Err(Error::IncompletePlan);
	}

	let path = worktree_path(worktree_root, input.number);
	let mut metadata: CheckoutMetadata = serde_json::from_slice(
		&fs::read(metadata_path(worktree_root, input.number)).map_err(|_| Error::CheckoutMissing)?,
	)
	.map_err(|_| Error::CheckoutMissing)?;
	if !metadata.repo.eq_ignore_ascii_case(&input.repo_identity) {
		return Err(Error::CheckoutMissing);
	}

	let remote_branch = if !input.head_ref.is_empty() {
		input.head_ref.as_str()
	} else {
		metadata.head.as_str()
	};
	if remote_branch.is_empty() {
		return Err(Error::IncompletePlan);
	}

	metadata.clone_url = input.clone_url.clone();
	metadata.head = remote_branch.to_owned();

	let pushed_head = tokio::select! {
		result = push_git(path, &metadata, input.force_with_lease, cancel.clone()) => result?,
		() = cancel.cancelled() => return Err(Error::Cancelled),
	};
	metadata.expected_head = pushed_head.to_string();
	fs::write(
		metadata_path(worktree_root, input.number),
		serde_json::to_vec(&metadata).expect("checkout metadata serializes"),
	)
	.map_err(|_| Error::GitFailed)?;

	Ok(PushOutput { number: input.number, head_commit: pushed_head })
}

async fn checkout_git(
	root: &Path,
	path: &Path,
	number: u64,
	remote: &str,
	head: &str,
	force: bool,
	cancel: CancellationToken,
) -> Result<(), Error> {
	let parent = path.parent().ok_or(Error::GitFailed)?;
	fs::create_dir_all(parent).map_err(|_| Error::GitFailed)?;

	let root = root.to_owned();
	let repo = task::spawn_blocking(move || GitRepo::require(&root).map(Arc::new))
		.await
		.map_err(|_| Error::GitFailed)?
		.map_err(|_| Error::GitFailed)?;
	let repository = crate::vcs::git::repo::Repository::from_handle(Arc::clone(&repo));
	let _guard = crate::vcs::git::lock::write(&repository, &cancel)
		.await
		.map_err(|_| Error::Cancelled)?;
	let fetch_name = fetch_branch(number);
	let fetch_ref = format!("refs/heads/{fetch_name}");
	repo
		.fetch(remote, head, &fetch_ref, None, Some(cancel))
		.await
		.map_err(|_| Error::GitFailed)?;

	let path = path.to_owned();
	task::spawn_blocking(move || finish_checkout_git(repo, &path, number, &fetch_name, force))
		.await
		.map_err(|_| Error::GitFailed)?
}

fn finish_checkout_git(
	repo: Arc<GitRepo>,
	path: &Path,
	number: u64,
	fetch_name: &str,
	force: bool,
) -> Result<(), Error> {
	let branch = local_branch(number);
	let fetch_ref = format!("refs/heads/{fetch_name}");
	let fetched = repo
		.resolve_ref(&fetch_ref)
		.map_err(|_| Error::GitFailed)?
		.ok_or(Error::GitFailed)?;
	repo
		.delete_branch(fetch_name, true)
		.map_err(|_| Error::GitFailed)?;

	if path.exists() {
		let active = GitRepo::require(path)
			.map_err(|_| Error::GitFailed)?
			.current_branch()
			.map_err(|_| Error::GitFailed)?;
		if active.as_deref() != Some(branch.as_str()) {
			return Err(Error::GitFailed);
		}
		return Ok(());
	}

	match repo
		.resolve_ref(&format!("refs/heads/{branch}"))
		.map_err(|_| Error::GitFailed)?
	{
		Some(existing) if existing != fetched => {
			if !force {
				return Err(Error::GitFailed);
			}
			repo
				.create_branch(&branch, &fetched, true)
				.map_err(|_| Error::GitFailed)?;
		},
		None => repo
			.create_branch(&branch, &fetched, false)
			.map_err(|_| Error::GitFailed)?,
		_ => {},
	}
	repo
		.worktree_add(path, &branch, false)
		.map_err(|_| Error::GitFailed)
}

async fn push_git(
	path: PathBuf,
	metadata: &CheckoutMetadata,
	force: bool,
	cancel: CancellationToken,
) -> Result<Str, Error> {
	let repo = task::spawn_blocking(move || GitRepo::require(&path).map(Arc::new))
		.await
		.map_err(|_| Error::GitFailed)?
		.map_err(|_| Error::GitFailed)?;
	let repository = crate::vcs::git::repo::Repository::from_handle(Arc::clone(&repo));
	let _guard = crate::vcs::git::lock::write(&repository, &cancel)
		.await
		.map_err(|_| Error::Cancelled)?;
	let pushed_head = repo
		.head_sha()
		.map_err(|_| Error::GitFailed)?
		.ok_or(Error::GitFailed)?;
	repo
		.push(
			&PushOptions {
				remote:           Some(metadata.clone_url.clone()),
				refspec:          Some(format!("HEAD:refs/heads/{}", metadata.head)),
				force_with_lease: if force {
					if metadata.expected_head.is_empty() {
						return Err(Error::CheckoutMissing);
					}
					Some(format!("refs/heads/{}:{}", metadata.head, metadata.expected_head))
				} else {
					None
				},
			},
			Some(cancel),
		)
		.await
		.map_err(|_| Error::GitFailed)?;
	Ok(Str::new(pushed_head))
}

#[cfg(test)]
mod tests {
	use omp_vcs::testing::run as git;

	use super::*;

	fn init_project(root: &Path) {
		git(root, &["init", "-b", "main"]);
		git(root, &["config", "user.name", "omp"]);
		git(root, &["config", "user.email", "omp@example.test"]);
		std::fs::write(root.join("README"), "hello\n").expect("write");
		git(root, &["add", "README"]);
		git(root, &["commit", "-m", "init"]);
	}

	#[tokio::test]
	async fn checkout_creates_worktree_under_forge_root() {
		let dir = tempfile::tempdir().expect("temp");
		let project = dir.path().join("project");
		let remote = dir.path().join("remote.git");
		let trees = dir.path().join("state").join(WORKTREE_DIR);
		std::fs::create_dir_all(&project).expect("project");
		init_project(&project);
		git(&project, &["checkout", "-b", "topic"]);
		std::fs::write(project.join("README"), "topic\n").expect("write");
		git(&project, &["add", "README"]);
		git(&project, &["commit", "-m", "topic"]);
		git(&project, &["checkout", "main"]);

		git(dir.path(), &["init", "--bare", remote.to_str().expect("utf8")]);
		git(&project, &["remote", "add", "origin", remote.to_str().expect("utf8")]);
		git(&project, &["push", "-u", "origin", "main"]);
		git(&project, &["push", "origin", "topic"]);

		let sha = git(&project, &["rev-parse", "topic"]).trim().to_owned();
		let input = CheckoutInput {
			repo_identity: "example.test/acme/widget".to_owned(),
			number:        7,
			clone_url:     remote.to_string_lossy().into_owned(),
			base_ref:      "main".to_owned(),
			head_ref:      "topic".to_owned(),
			head_sha:      sha.clone(),
			force:         false,
		};
		let cancel = CancellationToken::new();
		let out = checkout(&project, &trees, &input, &cancel)
			.await
			.expect("checkout");
		assert_eq!(out.number, 7);
		assert_eq!(out.head_ref, "topic");
		assert_eq!(out.base_ref, "main");
		assert_eq!(out.head_sha, sha);
		let expected = worktree_path(&trees, 7);
		assert_eq!(out.worktree, expected);
		assert!(expected.join("README").is_file());
		assert!(metadata_path(&trees, 7).is_file());
	}

	#[tokio::test]
	async fn push_updates_remote_and_receipt_sha() {
		let dir = tempfile::tempdir().expect("temp");
		let project = dir.path().join("project");
		let remote = dir.path().join("remote.git");
		let trees = dir.path().join("state").join(WORKTREE_DIR);
		std::fs::create_dir_all(&project).expect("project");
		init_project(&project);
		git(&project, &["checkout", "-b", "topic"]);
		std::fs::write(project.join("README"), "topic\n").expect("write");
		git(&project, &["add", "README"]);
		git(&project, &["commit", "-m", "topic"]);
		git(&project, &["checkout", "main"]);
		git(dir.path(), &["init", "--bare", remote.to_str().expect("utf8")]);
		git(&project, &["remote", "add", "origin", remote.to_str().expect("utf8")]);
		git(&project, &["push", "-u", "origin", "main"]);
		git(&project, &["push", "origin", "topic"]);
		let sha = git(&project, &["rev-parse", "topic"]).trim().to_owned();
		let cancel = CancellationToken::new();
		let out = checkout(
			&project,
			&trees,
			&CheckoutInput {
				repo_identity: "example.test/acme/widget".to_owned(),
				number:        9,
				clone_url:     remote.to_string_lossy().into_owned(),
				base_ref:      "main".to_owned(),
				head_ref:      "topic".to_owned(),
				head_sha:      sha,
				force:         false,
			},
			&cancel,
		)
		.await
		.expect("checkout");
		std::fs::write(out.worktree.join("README"), "pushed\n").expect("edit");
		git(&out.worktree, &["add", "README"]);
		git(&out.worktree, &["commit", "-m", "push me"]);
		let new_sha = git(&out.worktree, &["rev-parse", "HEAD"]).trim().to_owned();

		let pushed = push(
			&trees,
			&PushInput {
				repo_identity:    "example.test/acme/widget".to_owned(),
				number:           9,
				clone_url:        remote.to_string_lossy().into_owned(),
				head_ref:         "topic".to_owned(),
				force_with_lease: false,
			},
			&cancel,
		)
		.await
		.expect("push");
		assert_eq!(pushed.number, 9);
		assert_eq!(pushed.head_commit.as_str(), new_sha);
		let remote_sha = git(&remote, &["rev-parse", "topic"]).trim().to_owned();
		assert_eq!(remote_sha, new_sha);
	}
}
