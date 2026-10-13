//! Turns a captured file list into REAPI `Directory` messages in canonical
//! form, bottom-up, and the digest of the root that an `Action` names as its
//! `input_root_digest` (ADR 0040, Part D.3).
//!
//! Canonical form is what makes the digest stable: every directory lists its
//! files, then its sub-directories, each sorted by UTF-8 bytes, with one path
//! segment per node, and carries no mtimes or node properties. Two captures of
//! the same tree therefore produce the same root digest on any machine, which
//! is what lets the server's cache and our own tests treat it as an identity.

use std::collections::BTreeMap;

use omp_core::Hash32;
use omp_exec::Digest;
use prost::Message as _;

use crate::{digest::to_wire, proto::build::bazel::remote::execution::v2 as wire};

/// One captured file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFile {
	/// Path relative to the input root, `/`-separated, no leading slash, no
	/// `.` or `..` segment.
	pub path:       String,
	/// Digest of the file's bytes.
	pub digest:     Digest,
	/// Whether the file is executable.
	pub executable: bool,
}

/// Why a file list cannot form a tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
	/// A path is empty, absolute, or has an empty, `.` or `..` segment.
	#[error("input path {path:?} is not a clean relative path")]
	BadPath {
		/// The offending path.
		path: String,
	},
	/// The same path appears twice.
	#[error("input path {path:?} appears more than once")]
	Duplicate {
		/// The duplicated path.
		path: String,
	},
	/// A path is both a file and a directory (`a` and `a/b`).
	#[error("input path {path:?} is both a file and a directory")]
	FileAndDirectory {
		/// The path used as both.
		path: String,
	},
}

/// The result of building a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
	/// Digest of the root `Directory`.
	pub root:        Digest,
	/// Every `Directory` message, with its digest, root last. Each must be in
	/// the CAS for the server to resolve `input_root_digest`.
	pub directories: Vec<(Digest, wire::Directory)>,
}

#[derive(Default)]
struct Node {
	files:    BTreeMap<String, (Digest, bool)>,
	children: BTreeMap<String, Node>,
}

/// Builds the canonical tree for `files`.
///
/// # Errors
///
/// [`TreeError`] for an unclean, duplicated, or file-and-directory path.
pub fn build(files: &[InputFile]) -> Result<Tree, TreeError> {
	let mut root = Node::default();
	for file in files {
		insert(&mut root, file)?;
	}
	let mut directories = Vec::new();
	let root_digest = emit(&root, &mut directories);
	Ok(Tree { root: root_digest, directories })
}

fn insert(root: &mut Node, file: &InputFile) -> Result<(), TreeError> {
	let bad = || TreeError::BadPath { path: file.path.clone() };
	if file.path.is_empty() || file.path.starts_with('/') {
		return Err(bad());
	}
	let mut segments = file.path.split('/').peekable();
	let mut node = root;
	while let Some(segment) = segments.next() {
		if segment.is_empty() || segment == "." || segment == ".." {
			return Err(bad());
		}
		if segments.peek().is_some() {
			if node.files.contains_key(segment) {
				return Err(TreeError::FileAndDirectory { path: file.path.clone() });
			}
			node = node.children.entry(segment.to_owned()).or_default();
		} else {
			if node.children.contains_key(segment) {
				return Err(TreeError::FileAndDirectory { path: file.path.clone() });
			}
			if node
				.files
				.insert(segment.to_owned(), (file.digest, file.executable))
				.is_some()
			{
				return Err(TreeError::Duplicate { path: file.path.clone() });
			}
		}
	}
	Ok(())
}

/// Emits `node`'s children first, then `node`, and returns its digest.
fn emit(node: &Node, out: &mut Vec<(Digest, wire::Directory)>) -> Digest {
	// BTreeMap iterates in key order, and `String` orders by UTF-8 bytes, which
	// is the order REAPI requires.
	let directories = node
		.children
		.iter()
		.map(|(name, child)| wire::DirectoryNode {
			name:   name.clone(),
			digest: Some(to_wire(emit(child, out))),
		})
		.collect();
	let files = node
		.files
		.iter()
		.map(|(name, (digest, executable))| wire::FileNode {
			name:            name.clone(),
			digest:          Some(to_wire(*digest)),
			is_executable:   *executable,
			node_properties: None,
		})
		.collect();
	let directory =
		wire::Directory { files, directories, symlinks: Vec::new(), node_properties: None };
	let bytes = directory.encode_to_vec();
	let digest = Digest::new(Hash32::sum(&bytes), bytes.len() as u64);
	out.push((digest, directory));
	digest
}

#[cfg(test)]
mod tests {
	use super::*;

	fn file(path: &str, content: &str, executable: bool) -> InputFile {
		InputFile {
			path: path.to_owned(),
			digest: Digest::new(Hash32::sum(content), content.len() as u64),
			executable,
		}
	}

	#[test]
	fn the_root_digest_does_not_depend_on_input_order() {
		let a = [
			file("src/lib.rs", "lib", false),
			file("Cargo.toml", "toml", false),
			file("src/a/b.rs", "b", false),
		];
		let mut b = a.clone();
		b.reverse();
		assert_eq!(build(&a).expect("tree").root, build(&b).expect("tree").root);
	}

	#[test]
	fn a_change_to_one_file_changes_the_root_but_not_a_sibling_directory() {
		let before = build(&[file("a/x", "1", false), file("b/y", "2", false)]).expect("tree");
		let after = build(&[file("a/x", "CHANGED", false), file("b/y", "2", false)]).expect("tree");
		assert_ne!(before.root, after.root);
		let b_digest = |tree: &Tree| {
			tree
				.directories
				.iter()
				.find(|(_, dir)| dir.files.first().is_some_and(|f| f.name == "y"))
				.map(|(digest, _)| *digest)
		};
		assert_eq!(b_digest(&before), b_digest(&after), "untouched subtree keeps its digest");
	}

	#[test]
	fn directories_are_sorted_by_utf8_bytes_with_children_emitted_first() {
		let tree = build(&[
			file("z.txt", "z", false),
			file("B/inner", "i", false),
			file("a/inner", "i", false),
			file("Zed", "z", false),
		])
		.expect("tree");
		let (root_digest, root) = tree.directories.last().expect("root last");
		assert_eq!(*root_digest, tree.root);
		let dirs: Vec<&str> = root.directories.iter().map(|d| d.name.as_str()).collect();
		let files: Vec<&str> = root.files.iter().map(|f| f.name.as_str()).collect();
		assert_eq!(dirs, ["B", "a"], "uppercase sorts before lowercase by byte value");
		assert_eq!(files, ["Zed", "z.txt"]);
	}

	#[test]
	fn executable_bit_is_kept_and_no_mtime_or_properties_are_set() {
		let tree = build(&[file("run.sh", "#!/bin/sh", true)]).expect("tree");
		let node = &tree.directories[0].1.files[0];
		assert!(node.is_executable);
		assert_eq!(node.node_properties, None);
		assert_eq!(tree.directories[0].1.node_properties, None);
	}

	#[test]
	fn identical_subtrees_produce_identical_directory_digests() {
		let tree = build(&[file("x/f", "same", false), file("y/f", "same", false)]).expect("tree");
		let root = &tree.directories.last().expect("root").1;
		assert_eq!(root.directories[0].digest, root.directories[1].digest);
	}

	#[test]
	fn the_empty_input_is_an_empty_root_directory() {
		let tree = build(&[]).expect("tree");
		assert_eq!(tree.directories.len(), 1);
		assert_eq!(tree.root, Digest::new(Hash32::sum(b""), 0));
	}

	#[test]
	fn unclean_duplicate_and_conflicting_paths_are_refused() {
		for path in ["", "/abs", "a//b", "a/./b", "../escape", "a/..", "trailing/"] {
			assert_eq!(
				build(&[file(path, "x", false)]),
				Err(TreeError::BadPath { path: path.to_owned() }),
				"{path:?}"
			);
		}
		assert_eq!(
			build(&[file("a", "1", false), file("a", "2", false)]),
			Err(TreeError::Duplicate { path: "a".to_owned() })
		);
		assert!(matches!(
			build(&[file("a", "1", false), file("a/b", "2", false)]),
			Err(TreeError::FileAndDirectory { .. })
		));
		assert!(matches!(
			build(&[file("a/b", "2", false), file("a", "1", false)]),
			Err(TreeError::FileAndDirectory { .. })
		));
	}
}
