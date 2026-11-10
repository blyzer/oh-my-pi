//! Operation vocabulary.
//!
//! [`ForgeOp::mutation`] is the approval flag. It lives on the contract so
//! a host implementor cannot report a write as a read.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use std::{mem::size_of, sync::Arc};

use omp_core::Str;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::RepoRef;

/// Whether an operation reads or writes.
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
pub enum Mutation {
	/// The operation does not change forge or repository state.
	Read,
	/// The operation changes forge or repository state and must be approved.
	Write,
}

/// A forge operation.
///
/// Names are the contract vocabulary. They are not vendor operation names.
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
	VariantArray,
	Serialize,
	Deserialize,
	JsonSchema,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum ForgeOp {
	/// Read repository metadata.
	RepoView,
	/// Read one file.
	FileRead,
	/// Read one change request.
	ChangeRequestView,
	/// Read one issue.
	IssueView,
	/// Read checks on a change request or a commit.
	ListChecks,
	/// Search change requests.
	SearchChangeRequests,
	/// Search issues.
	SearchIssues,
	/// Search code.
	SearchCode,
	/// Search commits.
	SearchCommits,
	/// Search repositories.
	SearchRepos,
	/// Create a change request. Mutates the forge.
	ChangeRequestCreate,
	/// Check a change request out into an isolated worktree. Mutates the
	/// local repository.
	ChangeRequestCheckout,
	/// Push a checked-out change-request branch. Mutates the remote
	/// repository.
	ChangeRequestPush,
	/// Create an issue. Mutates the forge.
	IssueCreate,
}

const _: () = assert!(size_of::<ForgeOp>() <= 1, "ForgeOp must stay a byte");

impl ForgeOp {
	/// The approval class of this operation.
	///
	/// Create, checkout, and push of a change request write, and so does
	/// issue creation. Every other operation reads.
	#[inline]
	#[must_use]
	pub const fn mutation(self) -> Mutation {
		match self {
			Self::RepoView
			| Self::FileRead
			| Self::ChangeRequestView
			| Self::IssueView
			| Self::ListChecks
			| Self::SearchChangeRequests
			| Self::SearchIssues
			| Self::SearchCode
			| Self::SearchCommits
			| Self::SearchRepos => Mutation::Read,
			Self::ChangeRequestCreate
			| Self::ChangeRequestCheckout
			| Self::ChangeRequestPush
			| Self::IssueCreate => Mutation::Write,
		}
	}

	/// Whether core must approval-gate this operation.
	#[inline]
	#[must_use]
	pub const fn mutates(self) -> bool {
		matches!(self.mutation(), Mutation::Write)
	}
}

/// Which family a search reads.
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
pub enum SearchKind {
	/// Change requests.
	ChangeRequests,
	/// Issues.
	Issues,
	/// Source text.
	Code,
	/// Commits.
	Commits,
	/// Repositories.
	Repositories,
}

impl SearchKind {
	/// The read operation this search performs.
	#[inline]
	#[must_use]
	pub const fn op(self) -> ForgeOp {
		match self {
			Self::ChangeRequests => ForgeOp::SearchChangeRequests,
			Self::Issues => ForgeOp::SearchIssues,
			Self::Code => ForgeOp::SearchCode,
			Self::Commits => ForgeOp::SearchCommits,
			Self::Repositories => ForgeOp::SearchRepos,
		}
	}
}

/// Upper bound a host applies to one search page.
///
/// A limit of `0` means the host chooses. A larger limit is clamped to this
/// value by the host. This crate does not perform the search.
pub const MAX_SEARCH_LIMIT: u32 = 100;

/// One search.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchQuery {
	/// What to search.
	pub kind:  SearchKind,
	/// Repository scope. `None` means the host's current repository.
	pub repo:  Option<RepoRef>,
	/// Search text. Empty means the host lists the scope.
	pub text:  Str,
	/// Page size. `0` means the host chooses, and the host clamps to
	/// [`MAX_SEARCH_LIMIT`].
	pub limit: u32,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchHit {
	/// Repository the hit belongs to.
	pub repo:    RepoRef,
	/// Title or commit subject.
	pub title:   Str,
	/// Browser URL. Empty when the forge has none.
	pub web_url: Str,
	/// Change-request or issue number, when the hit has one.
	pub number:  Option<u64>,
	/// Commit identity, when the hit has one.
	pub commit:  Option<Str>,
}

/// One page of search hits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
	/// The search family that produced the page.
	pub kind: SearchKind,
	/// Hits in forge order.
	pub hits: Arc<[SearchHit]>,
}

impl SearchPage {
	/// Collects `hits` into one shared slice.
	#[must_use]
	pub fn new(kind: SearchKind, hits: impl IntoIterator<Item = SearchHit>) -> Self {
		Self { kind, hits: hits.into_iter().collect() }
	}

	/// Every hit, in forge order.
	#[must_use]
	pub fn as_slice(&self) -> &[SearchHit] {
		&self.hits
	}
}

#[cfg(test)]
mod tests {
	use strum::VariantArray;

	use super::*;

	#[test]
	fn mutation_flags_cover_every_operation() {
		for op in ForgeOp::VARIANTS {
			let expected = match op {
				ForgeOp::RepoView
				| ForgeOp::FileRead
				| ForgeOp::ChangeRequestView
				| ForgeOp::IssueView
				| ForgeOp::ListChecks
				| ForgeOp::SearchChangeRequests
				| ForgeOp::SearchIssues
				| ForgeOp::SearchCode
				| ForgeOp::SearchCommits
				| ForgeOp::SearchRepos => false,
				ForgeOp::ChangeRequestCreate
				| ForgeOp::ChangeRequestCheckout
				| ForgeOp::ChangeRequestPush
				| ForgeOp::IssueCreate => true,
			};
			assert_eq!(op.mutates(), expected, "{op}");
			assert_eq!(op.mutates(), op.mutation() == Mutation::Write);
			let name = op.into_str();
			assert_eq!(name.parse::<ForgeOp>().unwrap(), *op);
			assert_eq!(name.to_ascii_uppercase().parse::<ForgeOp>().unwrap(), *op);
		}
	}

	#[test]
	fn search_kinds_are_reads() {
		for kind in [
			SearchKind::ChangeRequests,
			SearchKind::Issues,
			SearchKind::Code,
			SearchKind::Commits,
			SearchKind::Repositories,
		] {
			assert!(!kind.op().mutates(), "{kind}");
		}
	}

	#[test]
	fn operation_names_roundtrip_through_json() {
		let json = serde_json::to_string(&ForgeOp::ChangeRequestPush).unwrap();
		assert_eq!(json, "\"change_request_push\"");
		let parsed: ForgeOp = serde_json::from_str(&json).unwrap();
		assert_eq!(parsed, ForgeOp::ChangeRequestPush);
	}
}
