//! Repository and commit identities.
//!
//! A [`RepoRef`] always names a host. A parsed URL that omitted the host
//! stays a [`crate::SchemeRef`] until the caller supplies one.

use std::{fmt, mem::size_of};

use omp_core::{IntoStr, Str};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::component::{self, ComponentError};

/// A repository on a forge.
///
/// `host` is opaque operator data. This type does not name a product.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoRef {
	/// Opaque forge host.
	pub host:  Str,
	/// Owner path. Nested owners use `/` between segments.
	pub owner: Str,
	/// Repository name.
	pub name:  Str,
}

const _: () = assert!(size_of::<RepoRef>() <= 72, "RepoRef must stay three strings");

impl RepoRef {
	/// Builds a ref after checking each component.
	///
	/// # Errors
	///
	/// Returns [`ComponentError`] when the host, owner, or name is empty,
	/// too long, or outside the ASCII charset.
	pub fn try_new(
		host: impl IntoStr,
		owner: impl IntoStr,
		name: impl IntoStr,
	) -> Result<Self, ComponentError> {
		let host = host.into_str();
		let owner = owner.into_str();
		let name = name.into_str();
		component::check_host(&host)?;
		component::check_owner(&owner)?;
		component::check_name(&name)?;
		Ok(Self { host, owner, name })
	}
}

impl fmt::Display for RepoRef {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}/{}/{}", self.host, self.owner, self.name)
	}
}

/// A commit in a repository.
///
/// `id` is an opaque identity, not a branch name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommitRef {
	/// Repository that contains the commit.
	pub repo: RepoRef,
	/// Opaque commit identity.
	pub id:   Str,
}

const _: () = assert!(size_of::<CommitRef>() <= 96, "CommitRef must stay compact");

impl CommitRef {
	/// Builds a commit ref after checking the identity.
	///
	/// # Errors
	///
	/// Returns [`ComponentError`] when `id` is empty, too long, or not ASCII
	/// alphanumeric.
	pub fn try_new(repo: RepoRef, id: impl IntoStr) -> Result<Self, ComponentError> {
		let id = id.into_str();
		component::check_commit(&id)?;
		Ok(Self { repo, id })
	}
}

impl fmt::Display for CommitRef {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}@{}", self.repo, self.id)
	}
}

/// Repository metadata returned by [`crate::ForgeHost::view_repo`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Repository {
	/// The repository that was read.
	pub repo:           RepoRef,
	/// Default branch name, as the forge reports it.
	pub default_branch: Str,
	/// Clone URL. The contract does not fetch it.
	pub clone_url:      Str,
	/// Browser URL. Empty when the forge has none.
	pub web_url:        Str,
	/// Repository description. Empty when the forge has none.
	pub description:    Str,
}

/// A repository-relative file at one revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileQuery {
	/// Repository that holds the file.
	pub repo: RepoRef,
	/// Repository-relative path. Not absolute, and not a `..` traversal.
	pub path: Str,
	/// Revision the file is read at. A branch, a tag, or a commit identity.
	pub rev:  Str,
}

impl FileQuery {
	/// Builds a file query after checking the path and revision.
	///
	/// # Errors
	///
	/// Returns [`PathError`] when the path is absolute, empty, or contains a
	/// relative segment, or when the revision is empty.
	pub fn try_new(repo: RepoRef, path: impl IntoStr, rev: impl IntoStr) -> Result<Self, PathError> {
		let path = path.into_str();
		let rev = rev.into_str();
		check_file_path(&path)?;
		if rev.is_empty() {
			return Err(PathError::EmptyRev);
		}
		Ok(Self { repo, path, rev })
	}
}

/// Bytes of one file read.
///
/// `content` is owned. Cloning it shares the buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBody {
	/// The query that produced the bytes.
	pub query:   FileQuery,
	/// File bytes. The contract does not decode them.
	pub content: bytes::Bytes,
}

const _: () = assert!(size_of::<FileBody>() <= 320, "FileBody must stay compact");

/// A repository-relative path could not be used as a file query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum PathError {
	/// The path was empty.
	#[error("forge file path is empty")]
	Empty,
	/// The revision was empty.
	#[error("forge file revision is empty")]
	EmptyRev,
	/// The path was absolute.
	#[error("forge file path is absolute")]
	Absolute,
	/// The path contained an empty segment, `.`, or `..`.
	#[error("forge file path contains an empty or relative segment")]
	Unsafe,
}

fn check_file_path(path: &str) -> Result<(), PathError> {
	if path.is_empty() {
		return Err(PathError::Empty);
	}
	let bytes = path.as_bytes();
	if path.starts_with('/') || path.starts_with('\\') {
		return Err(PathError::Absolute);
	}
	if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
		return Err(PathError::Absolute);
	}
	if bytes.contains(&0) || bytes.contains(&b'\\') {
		return Err(PathError::Unsafe);
	}
	for segment in path.split('/') {
		if segment.is_empty() || segment == "." || segment == ".." {
			return Err(PathError::Unsafe);
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn repo_ref_accepts_a_nested_owner() {
		let repo = RepoRef::try_new("code.example", "org/team", "widget").unwrap();
		assert_eq!(repo.to_string(), "code.example/org/team/widget");
	}

	#[test]
	fn repo_ref_rejects_an_empty_host_and_a_slash_in_the_name() {
		assert!(matches!(RepoRef::try_new("", "acme", "widget"), Err(ComponentError::Empty { .. })));
		assert!(matches!(
			RepoRef::try_new("code.example", "acme", "a/b"),
			Err(ComponentError::Invalid { .. })
		));
	}

	#[test]
	fn file_query_rejects_traversal() {
		let repo = RepoRef::try_new("code.example", "acme", "widget").unwrap();
		assert!(matches!(
			FileQuery::try_new(repo.clone(), "../secret", "main"),
			Err(PathError::Unsafe)
		));
		assert!(matches!(FileQuery::try_new(repo, "/etc/passwd", "main"), Err(PathError::Absolute)));
	}
}
