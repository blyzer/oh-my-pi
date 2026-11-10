//! `pr://` and `issue://` parsing.
//!
//! A numbered resource is `pr://[host/]owner/repo/number` or the same shape
//! with the `issue` scheme. A host is the first segment only when at least
//! an owner and a repository name follow it. Nested owners are the middle
//! segments (`pr://code.example/org/team/widget/42` has owner `org/team`).
//! The shorthand `pr://acme/widget/42` leaves the host absent. This parser
//! never fills one in.
//!
//! A change request may end in `/diff`, `/diff/all`, or `/diff/<index>`
//! with `<index>` starting at 1. An issue URL rejects those selectors.
//! `comments=0` or `comments=false` records that comments are off;
//! `comments=1` or `comments=true` records that they are on. Any other
//! query key is ignored.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use std::{fmt, mem::size_of};

use omp_core::{Str, StrMut};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use strum::{Display, EnumString, IntoStaticStr};
use thiserror::Error;

use crate::component::{self, ComponentError, MAX_SEGMENTS};

/// Whether a URL named a change request or an issue.
///
/// [`Self::ChangeRequest`] displays as `pr`, which is the scheme spelling.
/// The data name is `change_request`.
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
#[strum(ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
	/// A `pr://` change request.
	///
	/// Display and [`Self::into_str`] use the scheme spelling `pr`.
	/// `change_request` is accepted when parsing the vocabulary name.
	#[strum(to_string = "pr", serialize = "pr", serialize = "change_request")]
	ChangeRequest,
	/// An `issue://` issue.
	#[strum(serialize = "issue")]
	Issue,
}

/// A diff selector on a change-request URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiffSelector {
	/// `/diff`.
	Index,
	/// `/diff/all`.
	All,
	/// `/diff/<index>`, starting at 1.
	Slice(u32),
}

/// A path selector peeled off a resource URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceSelector {
	/// The URL named the resource and nothing under it.
	None,
	/// A change-request diff selector.
	Diff(DiffSelector),
}

/// One numbered `pr://` or `issue://` resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SchemeRef {
	/// Change request or issue.
	pub kind:     ResourceKind,
	/// Host, when the URL included one.
	pub host:     Option<Str>,
	/// Owner path. Nested owners keep their slashes.
	pub owner:    Str,
	/// Repository name.
	pub name:     Str,
	/// Change-request or issue number. Always at least 1.
	pub number:   u64,
	/// Path selector. [`ResourceSelector::None`] when the URL had none.
	pub selector: ResourceSelector,
	/// `comments` query, when the URL set it.
	pub comments: Option<bool>,
}

const _: () = assert!(size_of::<SchemeRef>() <= 192, "SchemeRef must stay compact");

impl SchemeRef {
	/// The host, when the URL named one.
	#[must_use]
	pub fn host(&self) -> Option<&str> {
		self.host.as_deref()
	}

	/// A repository ref, when the URL named a host.
	///
	/// The components were validated by [`parse_resource_url`].
	#[must_use]
	pub fn repo(&self) -> Option<crate::RepoRef> {
		self.host.as_ref().map(|host| crate::RepoRef {
			host:  host.clone(),
			owner: self.owner.clone(),
			name:  self.name.clone(),
		})
	}
}

impl fmt::Display for SchemeRef {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{kind}://", kind = self.kind)?;
		if let Some(host) = &self.host {
			write!(formatter, "{host}/")?;
		}
		write!(formatter, "{}/{}/{}", self.owner, self.name, self.number)?;
		match self.selector {
			ResourceSelector::None => {},
			ResourceSelector::Diff(DiffSelector::Index) => formatter.write_str("/diff")?,
			ResourceSelector::Diff(DiffSelector::All) => formatter.write_str("/diff/all")?,
			ResourceSelector::Diff(DiffSelector::Slice(index)) => {
				write!(formatter, "/diff/{index}")?;
			},
		}
		if let Some(comments) = self.comments {
			write!(formatter, "?comments={}", u8::from(comments))?;
		}
		Ok(())
	}
}

/// A `pr://` or `issue://` URL could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum SchemeError {
	/// The input was empty or only whitespace.
	#[error("forge resource URL is empty")]
	Empty,
	/// The input had no `://` separator.
	#[error("forge resource URL has no scheme")]
	MissingScheme,
	/// The scheme was not `pr` or `issue`.
	#[error("forge resource scheme is not a change request or an issue")]
	UnknownScheme,
	/// The path did not name an owner and a repository.
	#[error("forge resource URL is missing a repository")]
	MissingRepository,
	/// The path did not end in a positive number.
	#[error("forge resource URL is missing a number")]
	MissingNumber,
	/// A numeric token was zero or did not fit in a `u64`.
	#[error("forge resource number is not a positive integer")]
	InvalidNumber,
	/// A `/diff` tail was not `/diff`, `/diff/all`, or `/diff/<index>`.
	#[error("forge resource diff selector is invalid")]
	InvalidDiffSelector,
	/// An issue URL used a diff selector.
	#[error("forge resource selector is only valid on a change request")]
	SelectorNotAllowed,
	/// The `comments` query was present and not a boolean.
	#[error("forge resource comments query is invalid")]
	InvalidComments,
	/// The path had more segments than [`MAX_SEGMENTS`].
	#[error("forge resource path has too many segments")]
	TooManySegments,
	/// A component failed validation.
	#[error(transparent)]
	Component(
		/// The component failure.
		#[from]
		ComponentError,
	),
}

/// Parses one numbered `pr://` or `issue://` URL.
///
/// # Errors
///
/// Returns [`SchemeError`] when the scheme, repository, number, selector,
/// or a path component is not valid. List URLs that name a repository and
/// no number return [`SchemeError::MissingNumber`].
pub fn parse_resource_url(url: &str) -> Result<SchemeRef, SchemeError> {
	let url = url.trim();
	if url.is_empty() {
		return Err(SchemeError::Empty);
	}
	let (scheme, rest) = split_scheme(url)?;
	let kind = scheme
		.parse::<ResourceKind>()
		.map_err(|_| SchemeError::UnknownScheme)?;
	let rest = strip_fragment(rest);
	let (path, query) = split_query(rest);
	let mut parts = decode_path(path)?;
	let selector = peel_selector(kind, &mut parts)?;
	let number = pop_number(&mut parts)?;
	let (host, owner, name) = split_repo(parts)?;
	let comments = parse_comments(query)?;
	Ok(SchemeRef { kind, host, owner, name, number, selector, comments })
}

fn split_scheme(url: &str) -> Result<(&str, &str), SchemeError> {
	let Some((scheme, rest)) = url.split_once("://") else {
		return Err(SchemeError::MissingScheme);
	};
	if scheme.is_empty()
		|| !scheme
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
	{
		return Err(SchemeError::UnknownScheme);
	}
	Ok((scheme, rest))
}

fn strip_fragment(rest: &str) -> &str {
	rest.split_once('#').map_or(rest, |(path, _)| path)
}

fn split_query(rest: &str) -> (&str, &str) {
	rest.split_once('?').unwrap_or((rest, ""))
}

fn decode_path(path: &str) -> Result<SmallVec<Str, 8>, SchemeError> {
	let mut parts = SmallVec::<Str, 8>::new();
	for raw in path.split('/') {
		if raw.is_empty() {
			continue;
		}
		if parts.len() == MAX_SEGMENTS {
			return Err(SchemeError::TooManySegments);
		}
		parts.push(component::decode_segment(raw)?);
	}
	Ok(parts)
}

fn peel_selector(
	kind: ResourceKind,
	parts: &mut SmallVec<Str, 8>,
) -> Result<ResourceSelector, SchemeError> {
	let len = parts.len();
	if len >= 3 && parts[len - 2].as_str() == "diff" && is_digits(parts[len - 3].as_str()) {
		let selector = match parts[len - 1].as_str() {
			"all" => DiffSelector::All,
			text if is_digits(text) => DiffSelector::Slice(parse_index(text)?),
			_ => return Err(selector_error(kind)),
		};
		return accept_selector(kind, selector, parts, 2);
	}
	if len >= 2 && parts[len - 1].as_str() == "diff" && is_digits(parts[len - 2].as_str()) {
		return accept_selector(kind, DiffSelector::Index, parts, 1);
	}
	Ok(ResourceSelector::None)
}

fn accept_selector(
	kind: ResourceKind,
	selector: DiffSelector,
	parts: &mut SmallVec<Str, 8>,
	drop: usize,
) -> Result<ResourceSelector, SchemeError> {
	if kind != ResourceKind::ChangeRequest {
		return Err(SchemeError::SelectorNotAllowed);
	}
	parts.truncate(parts.len() - drop);
	Ok(ResourceSelector::Diff(selector))
}

fn selector_error(kind: ResourceKind) -> SchemeError {
	if kind == ResourceKind::ChangeRequest {
		SchemeError::InvalidDiffSelector
	} else {
		SchemeError::SelectorNotAllowed
	}
}

fn pop_number(parts: &mut SmallVec<Str, 8>) -> Result<u64, SchemeError> {
	let Some(last) = parts.last() else {
		return Err(SchemeError::MissingNumber);
	};
	if !is_digits(last.as_str()) {
		return Err(SchemeError::MissingNumber);
	}
	let number: u64 = last
		.as_str()
		.parse()
		.map_err(|_| SchemeError::InvalidNumber)?;
	if number == 0 {
		return Err(SchemeError::InvalidNumber);
	}
	parts.pop();
	Ok(number)
}

fn split_repo(mut parts: SmallVec<Str, 8>) -> Result<(Option<Str>, Str, Str), SchemeError> {
	let Some(name) = parts.pop() else {
		return Err(SchemeError::MissingRepository);
	};
	if parts.is_empty() {
		return Err(SchemeError::MissingRepository);
	}
	if parts.len() == 1 {
		let owner = parts.pop().ok_or(SchemeError::MissingRepository)?;
		component::check_owner(&owner)?;
		component::check_name(&name)?;
		return Ok((None, owner, name));
	}
	let host = parts.remove(0);
	let owner = join_owner(&parts)?;
	component::check_host(&host)?;
	component::check_name(&name)?;
	Ok((Some(host), owner, name))
}

fn join_owner(parts: &[Str]) -> Result<Str, ComponentError> {
	if parts.is_empty() {
		return Err(ComponentError::Empty { component: crate::PathComponent::Owner });
	}
	if parts.len() == 1 {
		component::check_owner(&parts[0])?;
		return Ok(parts[0].clone());
	}
	let mut len = parts.len() - 1;
	for part in parts {
		component::check_owner(part)?;
		len += part.len();
	}
	if len > crate::MAX_OWNER_LEN {
		return Err(ComponentError::TooLong {
			component: crate::PathComponent::Owner,
			len,
			max: crate::MAX_OWNER_LEN,
		});
	}
	let mut owner = StrMut::with_capacity(len);
	for (index, part) in parts.iter().enumerate() {
		if index > 0 {
			owner.push('/');
		}
		owner.push_str(part);
	}
	Ok(owner.freeze())
}

fn parse_comments(query: &str) -> Result<Option<bool>, SchemeError> {
	if query.is_empty() {
		return Ok(None);
	}
	let mut comments = None;
	for pair in query.split('&') {
		if pair.is_empty() {
			continue;
		}
		let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
		if key != "comments" {
			continue;
		}
		comments = Some(parse_bool(value)?);
	}
	Ok(comments)
}

fn parse_bool(value: &str) -> Result<bool, SchemeError> {
	if value == "1" || value.eq_ignore_ascii_case("true") {
		Ok(true)
	} else if value == "0" || value.eq_ignore_ascii_case("false") {
		Ok(false)
	} else {
		Err(SchemeError::InvalidComments)
	}
}

fn parse_index(text: &str) -> Result<u32, SchemeError> {
	let index: u32 = text.parse().map_err(|_| SchemeError::InvalidDiffSelector)?;
	if index == 0 {
		return Err(SchemeError::InvalidDiffSelector);
	}
	Ok(index)
}

fn is_digits(text: &str) -> bool {
	!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parsed(url: &str) -> SchemeRef {
		parse_resource_url(url).unwrap_or_else(|error| panic!("{url}: {error}"))
	}

	#[test]
	fn parses_a_change_request_with_host_owner_repo_and_number() {
		let resource = parsed("pr://code.example/acme/widget/42");
		assert_eq!(resource.kind, ResourceKind::ChangeRequest);
		assert_eq!(resource.host(), Some("code.example"));
		assert_eq!(resource.owner.as_str(), "acme");
		assert_eq!(resource.name.as_str(), "widget");
		assert_eq!(resource.number, 42);
		assert_eq!(resource.selector, ResourceSelector::None);
		let repo = resource.repo().unwrap();
		assert_eq!(repo.host.as_str(), "code.example");
		assert_eq!(repo.to_string(), "code.example/acme/widget");
	}

	#[test]
	fn shorthand_omits_the_host_and_nested_owners_keep_their_slashes() {
		let shorthand = parsed("pr://acme/widget/7");
		assert!(shorthand.host.is_none());
		assert!(shorthand.repo().is_none());
		assert_eq!(shorthand.owner.as_str(), "acme");
		let nested = parsed("PR://code.example/org/team/widget/42/");
		assert_eq!(nested.host(), Some("code.example"));
		assert_eq!(nested.owner.as_str(), "org/team");
		assert_eq!(nested.name.as_str(), "widget");
	}

	#[test]
	fn diff_selectors_roundtrip_and_issues_reject_them() {
		for (url, selector) in [
			("pr://code.example/acme/widget/42/diff", DiffSelector::Index),
			("pr://code.example/acme/widget/42/diff/all", DiffSelector::All),
			("pr://code.example/acme/widget/42/diff/2", DiffSelector::Slice(2)),
		] {
			let resource = parsed(url);
			assert_eq!(resource.selector, ResourceSelector::Diff(selector));
			assert_eq!(parsed(&resource.to_string()), resource);
		}
		assert_eq!(
			parse_resource_url("issue://code.example/acme/widget/7/diff"),
			Err(SchemeError::SelectorNotAllowed)
		);
		assert_eq!(
			parse_resource_url("pr://code.example/acme/widget/42/diff/0"),
			Err(SchemeError::InvalidDiffSelector)
		);
	}

	#[test]
	fn comments_query_and_percent_decoding() {
		let resource = parsed("issue://code.example/acme/my%2Dwidget/7?comments=false&other=1");
		assert_eq!(resource.kind, ResourceKind::Issue);
		assert_eq!(resource.name.as_str(), "my-widget");
		assert_eq!(resource.comments, Some(false));
		assert_eq!(
			parse_resource_url("pr://code.example/acme/widget/1?comments=maybe"),
			Err(SchemeError::InvalidComments)
		);
		assert_eq!(
			parse_resource_url("pr://code.example/acme/%2e%2e/1"),
			Err(SchemeError::Component(ComponentError::Unsafe))
		);
	}

	#[test]
	fn rejects_incomplete_and_foreign_urls() {
		assert_eq!(parse_resource_url("   "), Err(SchemeError::Empty));
		assert_eq!(parse_resource_url("pr:acme/widget/1"), Err(SchemeError::MissingScheme));
		assert_eq!(
			parse_resource_url("https://code.example/acme/widget/1"),
			Err(SchemeError::UnknownScheme)
		);
		assert_eq!(parse_resource_url("pr://42"), Err(SchemeError::MissingRepository));
		assert_eq!(parse_resource_url("pr://acme/widget"), Err(SchemeError::MissingNumber));
		assert_eq!(
			parse_resource_url("pr://code.example/acme/widget/0"),
			Err(SchemeError::InvalidNumber)
		);
		assert_eq!(
			parse_resource_url("issue://code.example/acme/../1"),
			Err(SchemeError::Component(ComponentError::Unsafe))
		);
	}

	#[test]
	fn scheme_spelling_is_pr_and_the_data_name_parses() {
		assert_eq!(ResourceKind::ChangeRequest.into_str(), "pr");
		assert_eq!("change_request".parse::<ResourceKind>().unwrap(), ResourceKind::ChangeRequest);
		assert_eq!("ISSUE".parse::<ResourceKind>().unwrap(), ResourceKind::Issue);
	}
}
