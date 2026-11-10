//! Private GitHub operation vocabulary used by
//! [`crate::github::GithubService`].
//!
//! These types used to be the model-facing `github@3` contract. The model now
//! calls `forge@1` and `ci@1`. [`crate::github::GithubService::execute`] still
//! dispatches on this vocabulary, and the forge and CI adapters translate
//! into it.

use omp_core::Str;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// GitHub operation.
///
/// The `message` is the human title a transcript card used to paint. The
/// adapters do not show it to the model.
#[derive(
	Clone,
	Copy,
	Debug,
	Deserialize,
	Eq,
	JsonSchema,
	PartialEq,
	Serialize,
	strum::Display,
	strum::EnumMessage,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub(crate) enum Operation {
	/// Read repository metadata.
	#[strum(message = "Repo")]
	RepoView,
	/// Read a repository file.
	#[strum(message = "File")]
	FileRead,
	/// Create a pull request, first pushing a local head branch that the
	/// remote lacks or trails (fast-forward only).
	#[strum(message = "PR Create")]
	PrCreate,
	/// Check out pull request heads into isolated worktrees.
	#[strum(message = "PR Checkout")]
	PrCheckout,
	/// Push a previously checked-out pull request branch.
	#[strum(message = "PR Push")]
	PrPush,
	/// Search issues.
	#[strum(message = "Search Issues")]
	SearchIssues,
	/// Search pull requests.
	#[strum(message = "Search PRs")]
	SearchPrs,
	/// Search code.
	#[strum(message = "Search Code")]
	SearchCode,
	/// Search commits.
	#[strum(message = "Search Commits")]
	SearchCommits,
	/// Search repositories.
	#[strum(message = "Search Repos")]
	SearchRepos,
	/// Watch Actions runs and jobs.
	///
	/// The model-facing replacement is `ci@1` with `watch: true`. This
	/// variant stays so [`crate::github::GithubService::execute`] can still
	/// dispatch the old loop. The adapters call the one-shot snapshot instead.
	#[strum(message = "Run Watch")]
	RunWatch,
}

/// Pull request selector accepted as either one value or a batch.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
pub(crate) enum PrSelector {
	/// One pull request number, URL, or branch name.
	One(Str),
	/// Multiple pull request numbers, URLs, or branch names.
	Many(Vec<Str>),
}

impl PrSelector {
	/// Returns the selectors as a borrowed slice.
	pub(crate) fn as_slice(&self) -> &[Str] {
		match self {
			Self::One(value) => std::slice::from_ref(value),
			Self::Many(values) => values,
		}
	}
}

/// Date field supported by GitHub issue, pull-request, and repository search.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DateField {
	/// Resource creation time.
	Created,
	/// Resource update time; repository search maps this to push time.
	Updated,
}

/// Flat GitHub operation arguments.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Params {
	/// Operation selector.
	pub op:               Operation,
	/// `[host/]owner/repo`; omitted operations resolve the current checkout.
	pub repo:             Option<Str>,
	/// Repository-relative file path.
	pub path:             Option<Str>,
	/// Branch, ref, or watched commit.
	pub branch:           Option<Str>,
	/// Pull request number, URL, or branch; arrays batch checkout.
	pub pr:               Option<PrSelector>,
	/// Search query.
	pub query:            Option<Str>,
	/// Lower date bound.
	pub since:            Option<Str>,
	/// Upper date bound.
	pub until:            Option<Str>,
	/// Search date field.
	pub date_field:       Option<DateField>,
	/// Maximum returned rows.
	pub limit:            Option<u32>,
	/// Pull request title.
	pub title:            Option<Str>,
	/// Pull request body.
	pub body:             Option<Str>,
	/// Pull request base branch.
	pub base:             Option<Str>,
	/// Pull request head branch.
	pub head:             Option<Str>,
	/// Actions run id or URL.
	pub run:              Option<Str>,
	/// Open a draft pull request.
	#[serde(default)]
	pub draft:            bool,
	/// Reset an existing local pull-request branch to the remote head.
	#[serde(default)]
	pub force:            bool,
	/// Force-with-lease a PR push.
	#[serde(default)]
	pub force_with_lease: bool,
	/// Derive the pull request title and body from the head commits; mutually
	/// exclusive with `title` and `body`.
	#[serde(default)]
	pub fill:             bool,
	/// Reviewers to request on the created pull request; `org/team` requests a
	/// team review.
	#[serde(default)]
	pub reviewer:         Vec<Str>,
	/// Users to assign to the created pull request.
	#[serde(default)]
	pub assignee:         Vec<Str>,
	/// Labels to apply to the created pull request.
	#[serde(default)]
	pub label:            Vec<Str>,
	/// Log lines retained per failed Actions job; defaults to 15, capped at 200.
	pub tail:             Option<u32>,
}

/// Durable reference to complete output retained outside the inline projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Artifact {
	/// Canonical content-addressed URI.
	pub uri:        Str,
	/// Exact retained byte count.
	pub size:       u64,
	/// Media type of the retained bytes.
	pub media_type: Str,
}

/// Direct API response plus a bounded human projection and rate-limit receipt.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Payload {
	/// Completed operation.
	pub op:                   Operation,
	/// Structured operation result.
	pub result:               Value,
	/// Human-readable model and transcript projection.
	pub output:               Str,
	/// Complete output retained outside the inline projection, when applicable.
	pub artifact:             Option<Artifact>,
	/// Whether the operation produced no actionable result.
	pub useless:              bool,
	/// Remaining GitHub API requests, when reported.
	pub rate_limit_remaining: Option<u64>,
	/// Rate-limit reset Unix timestamp, when reported.
	pub rate_limit_reset:     Option<u64>,
}

/// GitHub service failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Fault {
	/// Stable failure category.
	pub code:                 Str,
	/// Secret-free diagnostic.
	pub message:              Str,
	/// HTTP status, when the failure came from GitHub.
	#[serde(default)]
	pub status:               Option<u16>,
	/// Remaining requests reported with an HTTP failure.
	#[serde(default)]
	pub rate_limit_remaining: Option<u64>,
	/// Rate-limit reset Unix timestamp reported with an HTTP failure.
	#[serde(default)]
	pub rate_limit_reset:     Option<u64>,
	/// Retry delay reported by GitHub, in seconds.
	#[serde(default)]
	pub retry_after_seconds:  Option<u64>,
}

impl Fault {
	/// Whether GitHub classified this failure as a primary or secondary rate
	/// limit.
	pub(crate) fn is_rate_limited(&self) -> bool {
		self.code == "github_rate_limited"
	}
}

/// Ephemeral Actions-watch state.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Update {
	/// Operation producing the update.
	pub op:     Operation,
	/// Current normalized watch snapshot.
	pub result: Value,
	/// Human-readable current state.
	pub output: Str,
}
