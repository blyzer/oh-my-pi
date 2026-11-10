//! Change requests, issues, and the repository writes that move them.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use std::mem::size_of;

use omp_core::{IntoStr, Str};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

use crate::{CommitRef, RepoRef};

/// Open, closed, or merged state of a change request.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Display,
	EnumString,
	IntoStaticStr,
	Serialize,
	Deserialize,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
	/// The change request is open.
	Open,
	/// The change request is closed and not merged.
	Closed,
	/// The change request is merged.
	Merged,
}

/// Open or closed state of an issue.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Display,
	EnumString,
	IntoStaticStr,
	Serialize,
	Deserialize,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
	/// The issue is open.
	Open,
	/// The issue is closed.
	Closed,
}

/// One change request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangeRequest {
	/// Repository that owns the change request.
	pub repo:        RepoRef,
	/// Change-request number on that repository.
	pub number:      u64,
	/// Title.
	pub title:       Str,
	/// Body. Empty when the forge has none.
	pub body:        Str,
	/// Open, closed, or merged.
	pub state:       RequestState,
	/// Whether the change request is a draft.
	pub draft:       bool,
	/// Base branch name.
	pub base_ref:    Str,
	/// Head branch name.
	pub head_ref:    Str,
	/// Head commit identity.
	pub head_commit: Str,
	/// Base commit identity.
	pub base_commit: Str,
	/// Browser URL. Empty when the forge has none.
	pub web_url:     Str,
}

const _: () = assert!(size_of::<ChangeRequest>() <= 288, "ChangeRequest must stay compact");

/// Fields required to create a change request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangeRequestDraft {
	/// Repository that will own the change request.
	pub repo:  RepoRef,
	/// Title.
	pub title: Str,
	/// Body. Empty when the caller supplies none.
	pub body:  Str,
	/// Base branch name.
	pub base:  Str,
	/// Head branch name.
	pub head:  Str,
	/// Create the change request as a draft.
	pub draft: bool,
}

impl ChangeRequestDraft {
	/// A draft with an empty body that is not marked as a draft change request.
	#[must_use]
	pub fn new(repo: RepoRef, title: impl IntoStr, base: impl IntoStr, head: impl IntoStr) -> Self {
		Self {
			repo,
			title: title.into_str(),
			body: Str::empty(),
			base: base.into_str(),
			head: head.into_str(),
			draft: false,
		}
	}

	/// Sets the body.
	#[must_use]
	pub fn with_body(mut self, body: impl IntoStr) -> Self {
		self.body = body.into_str();
		self
	}

	/// Sets whether the change request is a draft.
	#[must_use]
	pub const fn with_draft(mut self, draft: bool) -> Self {
		self.draft = draft;
		self
	}
}

/// One issue.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Issue {
	/// Repository that owns the issue.
	pub repo:    RepoRef,
	/// Issue number on that repository.
	pub number:  u64,
	/// Title.
	pub title:   Str,
	/// Body. Empty when the forge has none.
	pub body:    Str,
	/// Open or closed.
	pub state:   IssueState,
	/// Browser URL. Empty when the forge has none.
	pub web_url: Str,
}

const _: () = assert!(size_of::<Issue>() <= 192, "Issue must stay compact");

/// One conversation comment on a change request or an issue.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Comment {
	/// Author login. Empty when the forge has none.
	pub author:     Str,
	/// Creation time as the forge reported it. Empty when absent.
	pub created_at: Str,
	/// Comment body. Empty when the forge has none.
	pub body:       Str,
}

const _: () = assert!(size_of::<Comment>() <= 72, "Comment must stay three strings");

/// Fields required to create an issue.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IssueDraft {
	/// Repository that will own the issue.
	pub repo:  RepoRef,
	/// Title.
	pub title: Str,
	/// Body. Empty when the caller supplies none.
	pub body:  Str,
}

impl IssueDraft {
	/// An issue draft with an empty body.
	#[must_use]
	pub fn new(repo: RepoRef, title: impl IntoStr) -> Self {
		Self { repo, title: title.into_str(), body: Str::empty() }
	}

	/// Sets the body.
	#[must_use]
	pub fn with_body(mut self, body: impl IntoStr) -> Self {
		self.body = body.into_str();
		self
	}
}

/// Request to check a change request out into an isolated worktree.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckoutRequest {
	/// Repository that owns the change request.
	pub repo:   RepoRef,
	/// Change-request number.
	pub number: u64,
	/// Reset an existing local branch to the remote head.
	pub force:  bool,
}

impl CheckoutRequest {
	/// A checkout that does not reset an existing local branch.
	#[must_use]
	pub const fn new(repo: RepoRef, number: u64) -> Self {
		Self { repo, number, force: false }
	}
}

/// Result of checking a change request out.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckoutReceipt {
	/// Repository that was checked out.
	pub repo:     RepoRef,
	/// Change-request number.
	pub number:   u64,
	/// Head commit that was checked out.
	pub head:     CommitRef,
	/// Base branch name.
	pub base_ref: Str,
	/// Head branch name.
	pub head_ref: Str,
	/// Worktree path the core composition created.
	pub worktree: Str,
}

/// Request to push a checked-out change-request branch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PushRequest {
	/// Repository that owns the change request.
	pub repo:             RepoRef,
	/// Change-request number.
	pub number:           u64,
	/// Local branch to push.
	pub head_ref:         Str,
	/// Refuse the push when the remote moved.
	pub force_with_lease: bool,
}

/// Result of pushing a change-request branch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PushReceipt {
	/// Repository that was pushed.
	pub repo:        RepoRef,
	/// Change-request number.
	pub number:      u64,
	/// Commit identity the remote now has at the head.
	pub head_commit: Str,
}
