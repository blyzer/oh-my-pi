//! Patch application, hunk staging, stash, and cherry-pick for
//! [`GitRepo`](super::GitRepo).
//!
//! Patch joins deliberately preserve binary-patch terminators (issue #8899).
//! Cherry-picks are fail-clean: unlike `git cherry-pick`, conflicts do not put
//! markers or unmerged entries in the checkout. Stash pop is likewise
//! preflighted so a rejected restore leaves no trace (issue #4175).

use std::{
	collections::{BTreeMap, BTreeSet},
	fs,
	path::{Component, Path, PathBuf},
};

use gix::{
	bstr::{BStr, ByteSlice},
	index::entry::{Flags, Mode, Stat},
	merge::tree::TreatAsUnresolved,
	objs::tree::EntryKind,
	refs::transaction::PreviousValue,
};

use super::{GitRepo, mutate::update_reference};
use crate::{
	error::{Error, Result},
	types::{ApplyOptions, DiffOptions, HunkSelection, HunkSelectionError, HunkSpec},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileEntry {
	id:            gix::ObjectId,
	mode:          Mode,
	/// Index entry carries git's intent-to-add flag (`git add -N`): the path
	/// is promised but has no staged content yet.
	intent_to_add: bool,
}
impl FileEntry {
	const fn new(id: gix::ObjectId, mode: Mode) -> Self {
		Self { id, mode, intent_to_add: false }
	}
}

#[derive(Clone, Debug)]
struct FilePatch {
	old_path: Option<String>,
	new_path: Option<String>,
	old_mode: Option<Mode>,
	new_mode: Option<Mode>,
	old_oid:  Option<String>,
	new_oid:  Option<String>,
	hunks:    Vec<Hunk>,
	binary:   Vec<BinaryBlock>,
	raw:      String,
}

#[derive(Clone, Debug)]
struct Hunk {
	old_start: usize,
	old_count: usize,
	new_start: usize,
	new_count: usize,
	lines:     Vec<HunkLine>,
	raw:       String,
}

#[derive(Clone, Debug)]
struct HunkLine {
	kind:       u8,
	data:       Vec<u8>,
	no_newline: bool,
}

#[derive(Clone, Debug)]
struct BinaryBlock {
	kind:    BinaryKind,
	size:    usize,
	encoded: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum BinaryKind {
	Literal,
	Delta,
}

#[derive(Debug)]
enum ApplyFailure {
	Context(String),
	Invalid(String),
}

impl ApplyFailure {
	fn into_error(self) -> Error {
		let message = match self {
			Self::Context(message) | Self::Invalid(message) => message,
		};
		Error::PatchFailed { message }
	}
}

impl GitRepo {
	/// Apply a git-format patch to the worktree or index.
	pub fn apply_patch(&self, patch_text: &str, options: &ApplyOptions) -> Result<()> {
		if patch_text.trim().is_empty() {
			return Ok(());
		}
		let patches = parse_patch(patch_text).map_err(ApplyFailure::into_error)?;
		let repo = self.gix()?;
		// Containment is checked for EVERY side before anything is read or
		// written. Two distinct reasons to do it first:
		//
		//   - Reading. `augment_patch_sources` opens each source to fill the state map.
		//     A source beneath an escaping symlink would be read from outside the
		//     worktree before any later check could refuse it, and a device or FIFO
		//     target — `link/dev/zero`, a named pipe — turns that read into unbounded
		//     memory growth or a hang.
		//   - Writing. Validating per path as each is reached leaves a patch whose
		//     early files are legitimate and whose later target escapes half-applied,
		//     and a rename would already have deleted its source.
		//
		// Rejecting up front keeps application all-or-nothing with respect to
		// path validity.
		if !options.cached {
			assert_patch_paths_contained(self, &patches, options.reverse)?;
		}
		let mut state = if options.cached {
			index_map_at(&repo, options.index_path.as_deref())?
		} else {
			worktree_map(self, &repo)?
		};
		if !options.cached {
			augment_patch_sources(self, &repo, &mut state, &patches, options.reverse)?;
		}
		apply_patches_to_map(&repo, &mut state, &patches, options)?;
		if options.cached {
			write_index_map_at(&repo, &state, options.index_path.as_deref())?;
		} else {
			write_patch_worktree(self, &patches, options.reverse, &state)?;
		}
		Ok(())
	}

	/// Check whether a patch applies without changing the index or worktree.
	pub fn can_apply_patch(&self, patch_text: &str, options: &ApplyOptions) -> Result<bool> {
		if patch_text.trim().is_empty() {
			return Ok(true);
		}
		let Ok(patches) = parse_patch(patch_text) else {
			return Ok(false);
		};
		let repo = self.gix()?.with_object_memory();
		// Refused before any source is opened, for the same two reasons as
		// `apply_patch`: a probe must not be usable to read outside the
		// worktree, and a patch that cannot apply without escaping does not
		// apply. Without this the predicate answers `true` for a patch
		// `apply_patch` then refuses, and a caller that gates on it would treat
		// an attack as a viable change.
		if !options.cached && assert_patch_paths_contained(self, &patches, options.reverse).is_err() {
			return Ok(false);
		}
		let mut state = if options.cached {
			index_map_at(&repo, options.index_path.as_deref())?
		} else {
			worktree_map(self, &repo)?
		};
		if !options.cached {
			augment_patch_sources(self, &repo, &mut state, &patches, options.reverse)?;
		}
		match apply_patches_to_map(&repo, &mut state, &patches, options) {
			Ok(()) => Ok(true),
			Err(Error::PatchFailed { .. } | Error::Conflict { .. }) => Ok(false),
			Err(err) => Err(err),
		}
	}

	/// Stage selected hunks from a supplied or freshly generated worktree diff.
	pub fn stage_hunks(&self, selections: &[HunkSelection], raw_diff: Option<&str>) -> Result<()> {
		if selections.is_empty() {
			return Ok(());
		}
		let owned;
		let raw_diff = if let Some(raw_diff) = raw_diff {
			raw_diff
		} else {
			owned = self.diff_text(&DiffOptions::default())?;
			&owned
		};
		let files = parse_patch(raw_diff).map_err(ApplyFailure::into_error)?;
		let mut by_path = BTreeMap::new();
		for file in &files {
			if let Some(path) = file.new_path.as_ref().or(file.old_path.as_ref()) {
				by_path.insert(path.as_str(), file);
			}
		}
		let mut parts = Vec::with_capacity(selections.len());
		for selection in selections {
			let Some(file) = by_path.get(selection.path.as_str()) else {
				return Err(Error::PatchFailed {
					message: format!("No diff found for {}", selection.path),
				});
			};
			if !file.binary.is_empty() {
				if !matches!(selection.hunks, HunkSpec::All) {
					return Err(Error::PatchFailed {
						message: format!("Cannot select hunks for binary file {}", selection.path),
					});
				}
				parts.push(file.raw.clone());
				continue;
			}
			if matches!(selection.hunks, HunkSpec::All) {
				parts.push(file.raw.clone());
				continue;
			}
			let selected = select_hunks(file, &selection.hunks);
			if selected.is_empty() {
				return Err(Error::PatchFailed {
					message: format!("No hunks selected for {}", selection.path),
				});
			}
			let header = extract_file_header(&file.raw);
			let mut part = header.to_owned();
			for hunk in selected {
				if !part.ends_with('\n') {
					part.push('\n');
				}
				part.push_str(&hunk.raw);
			}
			parts.push(part);
		}
		let patch = join_patches(&parts);
		self.apply_patch(&patch, &ApplyOptions {
			cached:     true,
			index_path: None,
			reverse:    false,
			three_way:  false,
		})
	}

	/// Cherry-pick one commit with a fail-clean three-way tree merge.
	pub fn cherry_pick(&self, rev: &str) -> Result<()> {
		let repo = self.gix()?;
		let picked_id = repo
			.rev_parse_single(rev)
			.map_err(|err| Error::backend("git cherry-pick resolve", err))?
			.detach();
		let picked = repo
			.find_commit(picked_id)
			.map_err(|err| Error::backend("git cherry-pick commit", err))?;
		let parent_id = picked.parent_ids().next().map(|id| id.detach());
		let head = repo
			.head_commit()
			.map_err(|err| Error::backend("git cherry-pick HEAD", err))?;
		let head_id = head.id().detach();
		let head_tree = head
			.tree_id()
			.map_err(|err| Error::backend("git cherry-pick HEAD tree", err))?;
		let parent_tree = if let Some(parent_id) = parent_id {
			repo
				.find_commit(parent_id)
				.map_err(|err| Error::backend("git cherry-pick parent", err))?
				.tree_id()
				.map_err(|err| Error::backend("git cherry-pick parent tree", err))?
				.detach()
		} else {
			repo.empty_tree().id().detach()
		};
		let picked_tree = picked
			.tree_id()
			.map_err(|err| Error::backend("git cherry-pick picked tree", err))?;
		let options = repo
			.tree_merge_options()
			.map_err(|err| Error::backend("git cherry-pick options", err))?;
		let labels = gix::merge::blob::builtin_driver::text::Labels {
			ancestor: Some("base".into()),
			current:  Some("HEAD".into()),
			other:    Some(rev.into()),
		};
		let mut outcome = repo
			.merge_trees(parent_tree, head_tree, picked_tree, labels, options)
			.map_err(|err| Error::backend("git cherry-pick merge", err))?;
		if outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
			return Err(Error::Conflict { paths: Vec::new() });
		}
		let merged_tree = outcome
			.tree
			.write()
			.map_err(|err| Error::backend("git cherry-pick write tree", err))?
			.detach();
		if merged_tree == head_tree.detach() {
			return Err(Error::EmptyCherryPick { sha: picked_id.to_string() });
		}
		let author = picked
			.author()
			.map_err(|err| Error::backend("git cherry-pick author", err))?;
		let committer = repo
			.committer()
			.ok_or_else(|| Error::backend("git cherry-pick", "committer identity is not configured"))?
			.map_err(|err| Error::backend("git cherry-pick committer", err))?;
		let message = picked
			.message_raw()
			.map_err(|err| Error::backend("git cherry-pick message", err))?
			.to_str_lossy();
		let merged = tree_map(&repo, merged_tree)?;
		let previous = index_map(&repo)?;
		// Refuse before HEAD moves. Discovering an escaping path after the
		// commit would leave HEAD ahead of a worktree and index that never
		// received it — a fail-clean pick must fail before it commits.
		assert_worktree_map_contained(self, &repo, &previous, &merged)?;
		repo
			.commit_as(committer, author, "HEAD", message.as_ref(), merged_tree, [head_id])
			.map_err(|err| Error::backend("git cherry-pick commit", err))?;
		write_worktree_map(self, &previous, &merged)?;
		write_index_map(&repo, &merged)
	}

	/// Clear cherry-pick state; fail-clean single-commit picks create none.
	pub const fn cherry_pick_abort(&self) -> Result<()> {
		Ok(())
	}

	/// Skip cherry-pick state; single-commit picks have no sequencer.
	pub const fn cherry_pick_skip(&self) -> Result<()> {
		Ok(())
	}

	/// Stash index, tracked worktree changes, and untracked files.
	pub fn stash_push(&self, message: Option<&str>) -> Result<bool> {
		let repo = self.gix()?;
		let head = repo
			.head_commit()
			.map_err(|err| Error::backend("git stash HEAD", err))?;
		let head_id = head.id().detach();
		let head_tree = head
			.tree_id()
			.map_err(|err| Error::backend("git stash HEAD tree", err))?
			.detach();
		let head_map = tree_map(&repo, head_tree)?;
		let index = index_map(&repo)?;
		let tracked_worktree = tracked_worktree_map(self, &repo, &index)?;
		let untracked = untracked_worktree_map(self, &repo, &index)?;
		if index == head_map && tracked_worktree == index && untracked.is_empty() {
			return Ok(false);
		}
		let index_tree = write_tree_map(&repo, &index)?;
		let worktree_tree = write_tree_map(&repo, &tracked_worktree)?;
		let untracked_tree = write_tree_map(&repo, &untracked)?;
		let label = message.unwrap_or("WIP");
		let index_commit = repo
			.new_commit(format!("index on HEAD: {label}"), index_tree, [head_id])
			.map_err(|err| Error::backend("git stash index commit", err))?;
		let untracked_commit = repo
			.new_commit("untracked files on HEAD", untracked_tree, std::iter::empty::<gix::ObjectId>())
			.map_err(|err| Error::backend("git stash untracked commit", err))?;
		let stash_commit = repo
			.new_commit(label, worktree_tree, [
				head_id,
				index_commit.id().detach(),
				untracked_commit.id().detach(),
			])
			.map_err(|err| Error::backend("git stash commit", err))?;
		update_stash_ref(
			&repo,
			stash_commit.id().detach(),
			PreviousValue::Any,
			format!("On HEAD: {label}"),
			true,
		)?;
		write_worktree_map(self, &tracked_worktree, &head_map)?;
		write_index_map(&repo, &head_map)?;
		for path in untracked.keys() {
			remove_worktree_path(self, path)?;
		}
		Ok(true)
	}

	/// Try to pop the top stash without leaving partial conflict state.
	pub fn stash_try_pop(&self, reinstate_index: bool) -> Result<bool> {
		let repo = self.gix()?;
		let Some(stash_ref) = repo
			.try_find_reference("refs/stash")
			.map_err(|err| Error::backend("git stash resolve", err))?
		else {
			return Ok(false);
		};
		let stash_id = stash_ref.id().detach();
		let stash_log = fs::read(self.info().common_dir.join("logs/refs/stash")).ok();
		let stash = repo
			.find_commit(stash_id)
			.map_err(|err| Error::backend("git stash commit", err))?;
		let parents: Vec<_> = stash.parent_ids().map(|id| id.detach()).collect();
		if parents.len() < 2 {
			return Err(Error::backend("git stash pop", "stash commit has fewer than two parents"));
		}
		let base = repo
			.find_commit(parents[0])
			.map_err(|err| Error::backend("git stash base", err))?;
		let base_tree = base
			.tree_id()
			.map_err(|err| Error::backend("git stash base tree", err))?
			.detach();
		let stash_tree = stash
			.tree_id()
			.map_err(|err| Error::backend("git stash tree", err))?
			.detach();
		let current_index = index_map(&repo)?;
		let current_worktree = tracked_worktree_map(self, &repo, &current_index)?;
		let current_tree = write_tree_map(&repo, &current_worktree)?;
		let Some(merged_worktree) = merge_tree_maps(&repo, base_tree, current_tree, stash_tree)?
		else {
			return Ok(false);
		};
		let merged_index = if reinstate_index {
			let stash_index = repo
				.find_commit(parents[1])
				.map_err(|err| Error::backend("git stash index", err))?;
			let stash_index_tree = stash_index
				.tree_id()
				.map_err(|err| Error::backend("git stash index tree", err))?
				.detach();
			let current_index_tree = write_tree_map(&repo, &current_index)?;
			let Some(merged) =
				merge_tree_maps(&repo, base_tree, current_index_tree, stash_index_tree)?
			else {
				return Ok(false);
			};
			Some(merged)
		} else {
			None
		};
		let untracked = if let Some(parent) = parents.get(2) {
			let untracked_commit = repo
				.find_commit(*parent)
				.map_err(|err| Error::backend("git stash untracked", err))?;
			let tree = untracked_commit
				.tree_id()
				.map_err(|err| Error::backend("git stash untracked tree", err))?;
			tree_map(&repo, tree.detach())?
		} else {
			BTreeMap::new()
		};
		for path in untracked.keys() {
			if self.root().join(path).symlink_metadata().is_ok() {
				return Ok(false);
			}
		}
		write_worktree_map(self, &current_worktree, &merged_worktree)?;
		for (path, entry) in &untracked {
			write_worktree_entry(self, path, entry, &repo)?;
		}
		if let Some(merged_index) = merged_index {
			write_index_map(&repo, &merged_index)?;
		}
		drop_stash(self, &repo, &stash_ref, stash_id, stash_log.as_deref())?;
		Ok(true)
	}
}

/// Join patch parts verbatim, adding one final newline only when absent.
pub fn join_patches(parts: &[String]) -> String {
	let capacity = parts
		.iter()
		.map(|part| part.len() + usize::from(!part.ends_with('\n')))
		.sum();
	let mut joined = String::with_capacity(capacity);
	for part in parts {
		joined.push_str(part);
		if !part.ends_with('\n') {
			joined.push('\n');
		}
	}
	joined
}

/// Validate hunk selections against a raw git diff.
pub fn validate_hunk_selections(
	raw_diff: &str,
	selections: &[HunkSelection],
) -> Vec<HunkSelectionError> {
	let Ok(files) = parse_patch(raw_diff) else {
		return Vec::new();
	};
	let mut by_path = BTreeMap::new();
	for file in &files {
		if let Some(path) = file.new_path.as_ref().or(file.old_path.as_ref()) {
			by_path.insert(path.as_str(), file);
		}
	}
	let mut errors = Vec::new();
	for selection in selections {
		let Some(file) = by_path.get(selection.path.as_str()) else {
			continue;
		};
		if matches!(selection.hunks, HunkSpec::All) {
			continue;
		}
		if !file.binary.is_empty() {
			errors.push(HunkSelectionError {
				path:    selection.path.clone(),
				message: format!("Cannot select hunks for binary file {}", selection.path),
			});
		} else if select_hunks(file, &selection.hunks).is_empty() {
			errors.push(HunkSelectionError {
				path:    selection.path.clone(),
				message: format!("No hunks selected for {}", selection.path),
			});
		}
	}
	errors
}

fn parse_patch(text: &str) -> std::result::Result<Vec<FilePatch>, ApplyFailure> {
	let starts: Vec<_> = text
		.match_indices("diff --git ")
		.filter(|(idx, _)| *idx == 0 || text.as_bytes()[idx - 1] == b'\n')
		.map(|(idx, _)| idx)
		.collect();
	if starts.is_empty() {
		return Err(ApplyFailure::Invalid("patch has no diff --git header".into()));
	}
	let mut files = Vec::with_capacity(starts.len());
	for (position, start) in starts.iter().copied().enumerate() {
		let end = starts.get(position + 1).copied().unwrap_or(text.len());
		files.push(parse_file_patch(&text[start..end])?);
	}
	Ok(files)
}

fn parse_file_patch(raw: &str) -> std::result::Result<FilePatch, ApplyFailure> {
	let lines: Vec<&str> = raw.split_inclusive('\n').collect();
	let first = lines
		.first()
		.map(|line| line.trim_end_matches('\n'))
		.unwrap_or_default();
	let paths = first
		.strip_prefix("diff --git ")
		.ok_or_else(|| ApplyFailure::Invalid("invalid diff header".into()))?;
	let (old_token, new_token) = split_diff_paths(paths)?;
	let mut patch = FilePatch {
		old_path: Some(strip_side_path(old_token, 'a')),
		new_path: Some(strip_side_path(new_token, 'b')),
		old_mode: None,
		new_mode: None,
		old_oid:  None,
		new_oid:  None,
		hunks:    Vec::new(),
		binary:   Vec::new(),
		raw:      raw.to_owned(),
	};
	let mut index = 1;
	while index < lines.len() {
		let line = lines[index].trim_end_matches('\n');
		if let Some(value) = line.strip_prefix("old mode ") {
			patch.old_mode = parse_mode(value);
		} else if let Some(value) = line.strip_prefix("new mode ") {
			patch.new_mode = parse_mode(value);
		} else if let Some(value) = line.strip_prefix("new file mode ") {
			patch.old_path = None;
			patch.new_mode = parse_mode(value);
		} else if let Some(value) = line.strip_prefix("deleted file mode ") {
			patch.new_path = None;
			patch.old_mode = parse_mode(value);
		} else if let Some(value) = line.strip_prefix("rename from ") {
			patch.old_path = Some(unquote_path(value));
		} else if let Some(value) = line.strip_prefix("rename to ") {
			patch.new_path = Some(unquote_path(value));
		} else if let Some(value) = line.strip_prefix("index ") {
			let ids = value.split_whitespace().next().unwrap_or_default();
			if let Some((old, new)) = ids.split_once("..") {
				patch.old_oid = Some(old.to_owned());
				patch.new_oid = Some(new.to_owned());
			}
			if let Some(mode) = value.split_whitespace().nth(1).and_then(parse_mode) {
				patch.old_mode.get_or_insert(mode);
				patch.new_mode.get_or_insert(mode);
			}
		} else if let Some(value) = line.strip_prefix("--- ") {
			patch.old_path = parse_marker_path(value, 'a');
		} else if let Some(value) = line.strip_prefix("+++ ") {
			patch.new_path = parse_marker_path(value, 'b');
		} else if line.starts_with("@@") {
			let (hunk, next) = parse_hunk(&lines, index)?;
			patch.hunks.push(hunk);
			index = next;
			continue;
		} else if line == "GIT binary patch" {
			index += 1;
			while index < lines.len() {
				let header = lines[index].trim_end_matches('\n');
				let Some((kind, size)) = parse_binary_header(header) else {
					break;
				};
				index += 1;
				let mut encoded = Vec::new();
				while index < lines.len() {
					let data = lines[index].trim_end_matches('\n');
					if data.is_empty() {
						index += 1;
						break;
					}
					if parse_binary_header(data).is_some() {
						break;
					}
					encoded.push(data.to_owned());
					index += 1;
				}
				patch.binary.push(BinaryBlock { kind, size, encoded });
			}
			continue;
		}
		index += 1;
	}
	if patch.hunks.is_empty()
		&& patch.binary.is_empty()
		&& patch.old_path == patch.new_path
		&& patch.old_mode == patch.new_mode
	{
		return Err(ApplyFailure::Invalid("patch contains no change".into()));
	}
	Ok(patch)
}

fn split_diff_paths(paths: &str) -> std::result::Result<(&str, &str), ApplyFailure> {
	if paths.starts_with('"') {
		return Err(ApplyFailure::Invalid("quoted paths in diff headers are unsupported".into()));
	}
	paths
		.split_once(' ')
		.ok_or_else(|| ApplyFailure::Invalid("invalid diff paths".into()))
}

fn unquote_path(path: &str) -> String {
	path.trim_matches('"').to_owned()
}

fn strip_side_path(token: &str, side: char) -> String {
	let prefix = format!("{side}/");
	unquote_path(token.strip_prefix(&prefix).unwrap_or(token))
}

fn parse_marker_path(value: &str, side: char) -> Option<String> {
	let path = value.split('\t').next().unwrap_or(value);
	if path == "/dev/null" {
		None
	} else {
		Some(strip_side_path(path, side))
	}
}

fn parse_mode(value: &str) -> Option<Mode> {
	let bits = u32::from_str_radix(value.trim(), 8).ok()?;
	Mode::from_bits(bits)
}

fn parse_hunk(lines: &[&str], start: usize) -> std::result::Result<(Hunk, usize), ApplyFailure> {
	let header = lines[start].trim_end_matches('\n');
	let (old_start, old_count, new_start, new_count) = parse_hunk_header(header)?;
	let mut body: Vec<HunkLine> = Vec::new();
	let mut raw = String::from(lines[start]);
	let mut index = start + 1;
	while index < lines.len() {
		let line = lines[index];
		let bare = line.trim_end_matches('\n');
		if bare.starts_with("@@") || bare.starts_with("diff --git ") || bare == "GIT binary patch" {
			break;
		}
		let Some(kind @ (b' ' | b'+' | b'-')) = bare.as_bytes().first().copied() else {
			if bare == "\\ No newline at end of file" {
				let Some(previous) = body.last_mut() else {
					return Err(ApplyFailure::Invalid("orphan no-newline marker".into()));
				};
				previous.no_newline = true;
				raw.push_str(line);
				index += 1;
				continue;
			}
			break;
		};
		body.push(HunkLine { kind, data: bare.as_bytes()[1..].to_vec(), no_newline: false });
		raw.push_str(line);
		index += 1;
	}
	let actual_old = body.iter().filter(|line| line.kind != b'+').count();
	let actual_new = body.iter().filter(|line| line.kind != b'-').count();
	if actual_old != old_count || actual_new != new_count {
		return Err(ApplyFailure::Invalid(format!("hunk count mismatch in {header}")));
	}
	Ok((Hunk { old_start, old_count, new_start, new_count, lines: body, raw }, index))
}

fn parse_hunk_header(
	header: &str,
) -> std::result::Result<(usize, usize, usize, usize), ApplyFailure> {
	let body = header
		.strip_prefix("@@ -")
		.and_then(|value| value.split_once(" @@").map(|pair| pair.0))
		.ok_or_else(|| ApplyFailure::Invalid(format!("invalid hunk header: {header}")))?;
	let (old, new) = body
		.split_once(" +")
		.ok_or_else(|| ApplyFailure::Invalid(format!("invalid hunk header: {header}")))?;
	let parse_range = |value: &str| -> Option<(usize, usize)> {
		let (start, count) = value.split_once(',').unwrap_or((value, "1"));
		Some((start.parse().ok()?, count.parse().ok()?))
	};
	let (old_start, old_count) = parse_range(old)
		.ok_or_else(|| ApplyFailure::Invalid(format!("invalid hunk range: {header}")))?;
	let (new_start, new_count) = parse_range(new)
		.ok_or_else(|| ApplyFailure::Invalid(format!("invalid hunk range: {header}")))?;
	Ok((old_start, old_count, new_start, new_count))
}

fn parse_binary_header(line: &str) -> Option<(BinaryKind, usize)> {
	let (kind, size) = line.split_once(' ')?;
	let kind = match kind {
		"literal" => BinaryKind::Literal,
		"delta" => BinaryKind::Delta,
		_ => return None,
	};
	Some((kind, size.parse().ok()?))
}

fn select_hunks<'a>(file: &'a FilePatch, spec: &HunkSpec) -> Vec<&'a Hunk> {
	match spec {
		HunkSpec::All => file.hunks.iter().collect(),
		HunkSpec::Indices(indices) => {
			let wanted: BTreeSet<_> = indices
				.iter()
				.map(|index| (*index).max(1) as usize)
				.collect();
			file
				.hunks
				.iter()
				.enumerate()
				.filter(|(index, _)| wanted.contains(&(index + 1)))
				.map(|(_, hunk)| hunk)
				.collect()
		},
		HunkSpec::Lines { start, end } => file
			.hunks
			.iter()
			.filter(|hunk| {
				let first = hunk.new_start as u32;
				let last = first
					.saturating_add(hunk.new_count as u32)
					.saturating_sub(1);
				first <= *end && last >= *start
			})
			.collect(),
	}
}

fn extract_file_header(raw: &str) -> &str {
	raw.find("\n@@").map_or(raw, |position| &raw[..=position])
}

fn apply_patches_to_map(
	repo: &gix::Repository,
	state: &mut BTreeMap<String, FileEntry>,
	patches: &[FilePatch],
	options: &ApplyOptions,
) -> Result<()> {
	for patch in patches {
		let (source_path, target_path, source_mode, target_mode) =
			patch_sides(patch, options.reverse);
		// A create patch may land on an intent-to-add entry: git treats the
		// promised path as absent and stages the real content over it.
		if source_path.is_none()
			&& target_path
				.and_then(|path| state.get(path))
				.is_some_and(|entry| !entry.intent_to_add)
		{
			return Err(Error::PatchFailed {
				message: format!("{} already exists", target_path.unwrap_or_default()),
			});
		}
		let source = source_path.and_then(|path| state.get(path).cloned());
		if source_path.is_some() && source.is_none() {
			return Err(Error::PatchFailed {
				message: format!("{} does not exist", source_path.unwrap_or_default()),
			});
		}
		if source_mode.is_some_and(|mode| source.as_ref().is_some_and(|entry| entry.mode != mode)) {
			return Err(Error::PatchFailed {
				message: format!("mode does not match for {}", source_path.unwrap_or_default()),
			});
		}
		let source_bytes = match source.as_ref() {
			Some(entry) => blob_bytes(repo, entry.id)?,
			None => Vec::new(),
		};
		let direct = apply_file_bytes(patch, &source_bytes, options.reverse);
		let bytes = match direct {
			Ok(bytes) => bytes,
			Err(ApplyFailure::Context(_)) if options.three_way => {
				merge_patch_bytes(repo, patch, source.as_ref(), options.reverse)?
			},
			Err(err) => return Err(err.into_error()),
		};
		if let Some(path) = source_path
			&& target_path != Some(path)
		{
			state.remove(path);
		}
		if let Some(path) = target_path {
			validate_repo_path(path).map_err(ApplyFailure::into_error)?;
			let id = repo
				.write_blob(&bytes)
				.map_err(|err| Error::backend("git apply write blob", err))?
				.detach();
			let mode = target_mode
				.or_else(|| source.as_ref().map(|entry| entry.mode))
				.or(source_mode)
				.unwrap_or(Mode::FILE);
			state.insert(path.to_owned(), FileEntry::new(id, mode));
		}
	}
	Ok(())
}

fn patch_sides(
	patch: &FilePatch,
	reverse: bool,
) -> (Option<&str>, Option<&str>, Option<Mode>, Option<Mode>) {
	if reverse {
		(patch.new_path.as_deref(), patch.old_path.as_deref(), patch.new_mode, patch.old_mode)
	} else {
		(patch.old_path.as_deref(), patch.new_path.as_deref(), patch.old_mode, patch.new_mode)
	}
}

fn apply_file_bytes(
	patch: &FilePatch,
	source: &[u8],
	reverse: bool,
) -> std::result::Result<Vec<u8>, ApplyFailure> {
	if !patch.binary.is_empty() {
		let block = if reverse {
			patch.binary.get(1).or_else(|| patch.binary.first())
		} else {
			patch.binary.first()
		}
		.ok_or_else(|| ApplyFailure::Invalid("binary patch has no data block".into()))?;
		return decode_binary_block(block, source);
	}
	if patch.hunks.is_empty() {
		return Ok(source.to_vec());
	}
	let mut lines = split_lines(source);
	let mut offset: isize = 0;
	for hunk in &patch.hunks {
		let (start, count, replacement, expected) = hunk_sides(hunk, reverse);
		let position = if count == 0 {
			start as isize
		} else {
			start.saturating_sub(1) as isize
		} + offset;
		if position < 0 {
			return Err(ApplyFailure::Context("hunk position precedes file".into()));
		}
		let position = position as usize;
		if position.saturating_add(expected.len()) > lines.len()
			|| lines[position..position + expected.len()] != expected
		{
			return Err(ApplyFailure::Context(format!("hunk at line {start} does not apply")));
		}
		lines.splice(position..position + expected.len(), replacement.clone());
		offset += replacement.len() as isize - expected.len() as isize;
	}
	Ok(lines.concat())
}

fn hunk_sides(hunk: &Hunk, reverse: bool) -> (usize, usize, Vec<Vec<u8>>, Vec<Vec<u8>>) {
	let mut old = Vec::new();
	let mut new = Vec::new();
	for line in &hunk.lines {
		let mut content = line.data.clone();
		if !line.no_newline {
			content.push(b'\n');
		}
		if line.kind != b'+' {
			old.push(content.clone());
		}
		if line.kind != b'-' {
			new.push(content);
		}
	}
	if reverse {
		(hunk.new_start, hunk.new_count, old, new)
	} else {
		(hunk.old_start, hunk.old_count, new, old)
	}
}

fn split_lines(bytes: &[u8]) -> Vec<Vec<u8>> {
	let mut lines = Vec::new();
	let mut start = 0;
	for (index, byte) in bytes.iter().enumerate() {
		if *byte == b'\n' {
			lines.push(bytes[start..=index].to_vec());
			start = index + 1;
		}
	}
	if start < bytes.len() {
		lines.push(bytes[start..].to_vec());
	}
	lines
}

fn decode_binary_block(
	block: &BinaryBlock,
	base: &[u8],
) -> std::result::Result<Vec<u8>, ApplyFailure> {
	let mut compressed = Vec::new();
	for line in &block.encoded {
		let bytes = line.as_bytes();
		let Some(prefix) = bytes.first().copied() else {
			return Err(ApplyFailure::Invalid("empty binary data line".into()));
		};
		let decoded_len = match prefix {
			b'A'..=b'Z' => usize::from(prefix - b'A' + 1),
			b'a'..=b'z' => usize::from(prefix - b'a' + 27),
			_ => return Err(ApplyFailure::Invalid("invalid binary line length".into())),
		};
		let mut decoded = Vec::with_capacity((bytes.len().saturating_sub(1) / 5) * 4);
		for chunk in bytes[1..].chunks(5) {
			if chunk.len() != 5 {
				return Err(ApplyFailure::Invalid("truncated base85 group".into()));
			}
			let mut value = 0_u32;
			for byte in chunk {
				let digit = u32::from(decode_base85(*byte)?);
				value = value
					.checked_mul(85)
					.and_then(|value| value.checked_add(digit))
					.ok_or_else(|| ApplyFailure::Invalid("base85 overflow".into()))?;
			}
			decoded.extend_from_slice(&value.to_be_bytes());
		}
		if decoded_len > decoded.len() {
			return Err(ApplyFailure::Invalid("binary line length exceeds payload".into()));
		}
		compressed.extend_from_slice(&decoded[..decoded_len]);
	}
	let inflate_size = block.size;
	let mut inflated = vec![0; inflate_size];
	let mut decoder = gix::features::zlib::Inflate::default();
	let (status, consumed, written) = decoder
		.once(&compressed, &mut inflated)
		.map_err(|err| ApplyFailure::Invalid(format!("invalid zlib stream: {err}")))?;
	if status != gix::features::zlib::Status::StreamEnd || consumed != compressed.len() {
		return Err(ApplyFailure::Invalid("incomplete zlib stream".into()));
	}
	inflated.truncate(written);
	if inflated.len() != block.size {
		return Err(ApplyFailure::Invalid(format!(
			"binary payload size {} != {}",
			inflated.len(),
			block.size
		)));
	}
	match block.kind {
		BinaryKind::Literal => Ok(inflated),
		BinaryKind::Delta => apply_git_delta(base, &inflated),
	}
}

fn decode_base85(byte: u8) -> std::result::Result<u8, ApplyFailure> {
	const ALPHABET: &[u8; 85] =
		b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";
	ALPHABET
		.iter()
		.position(|candidate| *candidate == byte)
		.map(|position| position as u8)
		.ok_or_else(|| ApplyFailure::Invalid(format!("invalid base85 byte {byte}")))
}

fn apply_git_delta(base: &[u8], delta: &[u8]) -> std::result::Result<Vec<u8>, ApplyFailure> {
	let mut cursor = 0;
	let base_size = read_delta_varint(delta, &mut cursor)?;
	let result_size = read_delta_varint(delta, &mut cursor)?;
	if base_size != base.len() {
		return Err(ApplyFailure::Context("binary delta base size mismatch".into()));
	}
	let mut result = Vec::with_capacity(result_size);
	while cursor < delta.len() {
		let command = delta[cursor];
		cursor += 1;
		if command & 0x80 != 0 {
			let mut offset = 0_usize;
			let mut size = 0_usize;
			for bit in 0..4 {
				if command & (1 << bit) != 0 {
					offset |= usize::from(
						*delta
							.get(cursor)
							.ok_or_else(|| ApplyFailure::Invalid("truncated delta copy offset".into()))?,
					) << (8 * bit);
					cursor += 1;
				}
			}
			for bit in 0..3 {
				if command & (1 << (4 + bit)) != 0 {
					size |= usize::from(
						*delta
							.get(cursor)
							.ok_or_else(|| ApplyFailure::Invalid("truncated delta copy size".into()))?,
					) << (8 * bit);
					cursor += 1;
				}
			}
			if size == 0 {
				size = 0x1_0000;
			}
			let end = offset
				.checked_add(size)
				.ok_or_else(|| ApplyFailure::Invalid("delta copy overflow".into()))?;
			let slice = base
				.get(offset..end)
				.ok_or_else(|| ApplyFailure::Invalid("delta copy exceeds base".into()))?;
			result.extend_from_slice(slice);
		} else if command != 0 {
			let count = usize::from(command);
			let end = cursor
				.checked_add(count)
				.ok_or_else(|| ApplyFailure::Invalid("delta insert overflow".into()))?;
			result.extend_from_slice(
				delta
					.get(cursor..end)
					.ok_or_else(|| ApplyFailure::Invalid("truncated delta insert".into()))?,
			);
			cursor = end;
		} else {
			return Err(ApplyFailure::Invalid("invalid zero delta opcode".into()));
		}
	}
	if result.len() != result_size {
		return Err(ApplyFailure::Invalid("delta result length mismatch".into()));
	}
	Ok(result)
}

fn read_delta_varint(data: &[u8], cursor: &mut usize) -> std::result::Result<usize, ApplyFailure> {
	let mut value = 0_usize;
	let mut shift = 0;
	loop {
		let byte = *data
			.get(*cursor)
			.ok_or_else(|| ApplyFailure::Invalid("truncated delta header".into()))?;
		*cursor += 1;
		value |= usize::from(byte & 0x7f)
			.checked_shl(shift)
			.ok_or_else(|| ApplyFailure::Invalid("delta varint overflow".into()))?;
		if byte & 0x80 == 0 {
			return Ok(value);
		}
		shift += 7;
		if shift >= usize::BITS {
			return Err(ApplyFailure::Invalid("delta varint overflow".into()));
		}
	}
}

fn merge_patch_bytes(
	repo: &gix::Repository,
	patch: &FilePatch,
	current: Option<&FileEntry>,
	reverse: bool,
) -> Result<Vec<u8>> {
	let old_oid = if reverse {
		patch.new_oid.as_deref()
	} else {
		patch.old_oid.as_deref()
	}
	.ok_or_else(|| Error::PatchFailed {
		message: "3-way patch lacks an index base object".into(),
	})?;
	let base_id = resolve_object(repo, old_oid)?;
	let base = blob_bytes(repo, base_id)?;
	let theirs = apply_file_bytes(patch, &base, reverse).map_err(ApplyFailure::into_error)?;
	let ours = current.map_or_else(|| Ok(Vec::new()), |entry| blob_bytes(repo, entry.id))?;
	if ours == base {
		return Ok(theirs);
	}
	if theirs == base || ours == theirs {
		return Ok(ours);
	}
	let mode = current.map_or(Mode::FILE, |entry| entry.mode);
	let base_blob = repo
		.write_blob(&base)
		.map_err(|err| Error::backend("git apply 3-way base", err))?
		.detach();
	let ours_blob = repo
		.write_blob(&ours)
		.map_err(|err| Error::backend("git apply 3-way ours", err))?
		.detach();
	let theirs_blob = repo
		.write_blob(&theirs)
		.map_err(|err| Error::backend("git apply 3-way theirs", err))?
		.detach();
	let path = patch
		.new_path
		.as_deref()
		.or(patch.old_path.as_deref())
		.unwrap_or("file");
	let base_tree =
		write_tree_map(repo, &BTreeMap::from([(path.to_owned(), FileEntry::new(base_blob, mode))]))?;
	let ours_tree =
		write_tree_map(repo, &BTreeMap::from([(path.to_owned(), FileEntry::new(ours_blob, mode))]))?;
	let theirs_tree = write_tree_map(
		repo,
		&BTreeMap::from([(path.to_owned(), FileEntry::new(theirs_blob, mode))]),
	)?;
	let Some(merged) = merge_tree_maps(repo, base_tree, ours_tree, theirs_tree)? else {
		return Err(Error::Conflict { paths: vec![path.to_owned()] });
	};
	let entry = merged
		.get(path)
		.ok_or_else(|| Error::Conflict { paths: vec![path.to_owned()] })?;
	blob_bytes(repo, entry.id)
}

fn resolve_object(repo: &gix::Repository, spec: &str) -> Result<gix::ObjectId> {
	repo
		.rev_parse_single(spec)
		.map(|id| id.detach())
		.map_err(|err| Error::backend("git apply resolve base", err))
}

fn merge_tree_maps(
	repo: &gix::Repository,
	base: gix::ObjectId,
	ours: gix::ObjectId,
	theirs: gix::ObjectId,
) -> Result<Option<BTreeMap<String, FileEntry>>> {
	let options = repo
		.tree_merge_options()
		.map_err(|err| Error::backend("git merge options", err))?;
	let labels = gix::merge::blob::builtin_driver::text::Labels {
		ancestor: Some("base".into()),
		current:  Some("current".into()),
		other:    Some("stashed".into()),
	};
	let mut outcome = repo
		.merge_trees(base, ours, theirs, labels, options)
		.map_err(|err| Error::backend("git tree merge", err))?;
	if outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
		return Ok(None);
	}
	let tree = outcome
		.tree
		.write()
		.map_err(|err| Error::backend("git merge write tree", err))?
		.detach();
	Ok(Some(tree_map(repo, tree)?))
}
fn drop_stash(
	repo: &GitRepo,
	gix_repo: &gix::Repository,
	stash_ref: &gix::Reference<'_>,
	stash_id: gix::ObjectId,
	log: Option<&[u8]>,
) -> Result<()> {
	let Some((previous, prior_log)) = log.and_then(previous_stash_from_log) else {
		return stash_ref
			.delete()
			.map_err(|err| Error::backend("git stash drop", err));
	};
	if previous.is_null() {
		return stash_ref
			.delete()
			.map_err(|err| Error::backend("git stash drop", err));
	}
	update_reference(
		gix_repo,
		"git stash drop",
		"refs/stash",
		previous,
		PreviousValue::MustExistAndMatch(gix::refs::Target::Object(stash_id)),
		"stash: drop",
		false,
	)?;
	let log_path = repo.info().common_dir.join("logs/refs/stash");
	fs::write(log_path, prior_log)?;
	Ok(())
}
fn update_stash_ref(
	repo: &gix::Repository,
	id: gix::ObjectId,
	expected: PreviousValue,
	message: String,
	force_create_reflog: bool,
) -> Result<()> {
	update_reference(
		repo,
		"git stash ref",
		"refs/stash",
		id,
		expected,
		&message,
		force_create_reflog,
	)
}

fn previous_stash_from_log(log: &[u8]) -> Option<(gix::ObjectId, &[u8])> {
	let end = log.iter().rposition(|byte| *byte != b'\n')? + 1;
	let start = log[..end]
		.iter()
		.rposition(|byte| *byte == b'\n')
		.map_or(0, |position| position + 1);
	let old_hex = log.get(start..end)?.split(|byte| *byte == b' ').next()?;
	let previous = gix::ObjectId::from_hex(old_hex).ok()?;
	Some((previous, &log[..start]))
}

fn blob_bytes(repo: &gix::Repository, id: gix::ObjectId) -> Result<Vec<u8>> {
	repo
		.find_blob(id)
		.map(|blob| blob.data.clone())
		.map_err(|err| Error::backend("git read blob", err))
}

fn index_map(repo: &gix::Repository) -> Result<BTreeMap<String, FileEntry>> {
	let index = repo
		.index_or_load_from_head_or_empty()
		.map_err(|err| Error::backend("git read index", err))?;
	Ok(index_state_map(&index))
}

fn index_map_at(
	repo: &gix::Repository,
	index_path: Option<&Path>,
) -> Result<BTreeMap<String, FileEntry>> {
	let Some(path) = index_path else {
		return index_map(repo);
	};
	match fs::metadata(path) {
		Ok(_) => {
			let index = gix::index::File::at(
				path,
				repo.object_hash(),
				false,
				gix::index::decode::Options::default(),
			)
			.map_err(|err| Error::backend("git read alternate index", err))?;
			Ok(index_state_map(&index))
		},
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
		Err(err) => Err(err.into()),
	}
}

fn index_state_map(index: &gix::index::State) -> BTreeMap<String, FileEntry> {
	let mut map = BTreeMap::new();
	for entry in index.entries() {
		if entry.stage() == gix::index::entry::Stage::Unconflicted {
			let path = entry.path(index);
			map.insert(path.to_str_lossy().into_owned(), FileEntry {
				id:            entry.id,
				mode:          entry.mode,
				intent_to_add: entry.flags.contains(Flags::INTENT_TO_ADD),
			});
		}
	}
	map
}

fn tree_map(repo: &gix::Repository, tree: gix::ObjectId) -> Result<BTreeMap<String, FileEntry>> {
	let index = repo
		.index_from_tree(&tree)
		.map_err(|err| Error::backend("git read tree", err))?;
	let mut map = BTreeMap::new();
	for entry in index.entries() {
		let path = entry.path(&index);
		map.insert(path.to_str_lossy().into_owned(), FileEntry::new(entry.id, entry.mode));
	}
	Ok(map)
}

fn worktree_map(repo: &GitRepo, gix_repo: &gix::Repository) -> Result<BTreeMap<String, FileEntry>> {
	let index = index_map(gix_repo)?;
	tracked_worktree_map(repo, gix_repo, &index)
}
fn augment_patch_sources(
	repo: &GitRepo,
	gix_repo: &gix::Repository,
	state: &mut BTreeMap<String, FileEntry>,
	patches: &[FilePatch],
	reverse: bool,
) -> Result<()> {
	for patch in patches {
		let (source, target, source_mode, target_mode) = patch_sides(patch, reverse);
		let path = if let Some(path) = source {
			path
		} else {
			let Some(target) = target else {
				continue;
			};
			target
		};
		if state.contains_key(path) {
			continue;
		}
		let mode = source_mode.or(target_mode).unwrap_or(Mode::FILE);
		if let Some((bytes, mode)) = read_worktree_entry(&repo.root().join(path), mode)? {
			let id = gix_repo
				.write_blob(bytes)
				.map_err(|err| Error::backend("git hash patch source", err))?
				.detach();
			state.insert(path.to_owned(), FileEntry::new(id, mode));
		}
	}
	Ok(())
}

fn tracked_worktree_map(
	repo: &GitRepo,
	gix_repo: &gix::Repository,
	index: &BTreeMap<String, FileEntry>,
) -> Result<BTreeMap<String, FileEntry>> {
	let mut map = BTreeMap::new();
	for (path, entry) in index {
		let absolute = repo.root().join(path);
		if let Some((bytes, mode)) = read_worktree_entry(&absolute, entry.mode)? {
			let id = gix_repo
				.write_blob(bytes)
				.map_err(|err| Error::backend("git hash worktree blob", err))?
				.detach();
			map.insert(path.clone(), FileEntry::new(id, mode));
		}
	}
	Ok(map)
}

fn untracked_worktree_map(
	repo: &GitRepo,
	gix_repo: &gix::Repository,
	index: &BTreeMap<String, FileEntry>,
) -> Result<BTreeMap<String, FileEntry>> {
	let mut walk_index = gix_repo
		.index_or_load_from_head_or_empty()
		.map_err(|err| Error::backend("git read index for untracked files", err))?
		.into_owned();
	for entry in walk_index.entries_mut() {
		entry.flags.insert(Flags::UPTODATE);
	}
	let options = gix_repo
		.dirwalk_options()
		.map_err(|err| Error::backend("git untracked options", err))?;
	let walk = gix_repo
		.dirwalk_iter(
			walk_index,
			std::iter::empty::<gix::bstr::BString>(),
			Default::default(),
			options,
		)
		.map_err(|err| Error::backend("git untracked walk", err))?;
	let mut map = BTreeMap::new();
	for item in walk {
		let item = item.map_err(|err| Error::backend("git untracked walk", err))?;
		if item.entry.status != gix::dir::entry::Status::Untracked
			|| !matches!(
				item.entry.disk_kind,
				Some(gix::dir::entry::Kind::File | gix::dir::entry::Kind::Symlink)
			) {
			continue;
		}
		let path = item.entry.rela_path.to_str_lossy().into_owned();
		if index.contains_key(&path) {
			continue;
		}
		let absolute = repo.root().join(&path);
		if let Some((bytes, mode)) = read_worktree_entry(&absolute, Mode::FILE)? {
			let id = gix_repo
				.write_blob(bytes)
				.map_err(|err| Error::backend("git hash untracked blob", err))?
				.detach();
			map.insert(path, FileEntry::new(id, mode));
		}
	}
	Ok(map)
}

fn read_worktree_entry(path: &Path, index_mode: Mode) -> Result<Option<(Vec<u8>, Mode)>> {
	let metadata = match fs::symlink_metadata(path) {
		Ok(metadata) => metadata,
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
		Err(err) => return Err(err.into()),
	};
	if metadata.file_type().is_symlink() {
		let target = fs::read_link(path)?;
		#[cfg(unix)]
		let bytes = {
			use std::os::unix::ffi::OsStrExt;
			target.as_os_str().as_bytes().to_vec()
		};
		#[cfg(not(unix))]
		let bytes = target.to_string_lossy().as_bytes().to_vec();
		return Ok(Some((bytes, Mode::SYMLINK)));
	}
	if !metadata.is_file() {
		return Ok(None);
	}
	let mode = worktree_file_mode(&metadata, index_mode);
	Ok(Some((fs::read(path)?, mode)))
}

#[cfg(unix)]
fn worktree_file_mode(metadata: &fs::Metadata, _index_mode: Mode) -> Mode {
	use std::os::unix::fs::PermissionsExt;
	if metadata.permissions().mode() & 0o111 != 0 {
		Mode::FILE_EXECUTABLE
	} else {
		Mode::FILE
	}
}

#[cfg(not(unix))]
fn worktree_file_mode(_metadata: &fs::Metadata, index_mode: Mode) -> Mode {
	index_mode
}

fn write_index_map(repo: &gix::Repository, map: &BTreeMap<String, FileEntry>) -> Result<()> {
	write_index_map_at(repo, map, None)
}

fn write_index_map_at(
	repo: &gix::Repository,
	map: &BTreeMap<String, FileEntry>,
	index_path: Option<&Path>,
) -> Result<()> {
	let mut state = gix::index::State::new(repo.object_hash());
	for (path, entry) in map {
		validate_repo_path(path).map_err(ApplyFailure::into_error)?;
		// INTENT_TO_ADD lives in the extended flag word; losing it here would
		// silently stage promised paths as empty files.
		let flags = if entry.intent_to_add {
			Flags::EXTENDED | Flags::INTENT_TO_ADD
		} else {
			Flags::empty()
		};
		state.dangerously_push_entry(
			Stat::default(),
			entry.id,
			flags,
			entry.mode,
			BStr::new(path.as_bytes()),
		);
	}
	state.sort_entries();
	let mut index = gix::index::File::from_state(
		state,
		index_path.map_or_else(|| repo.index_path(), Path::to_owned),
	);
	index.remove_tree();
	index
		.write(gix::index::write::Options::default())
		.map_err(|err| Error::backend("git write index", err))
}

fn write_tree_map(
	repo: &gix::Repository,
	map: &BTreeMap<String, FileEntry>,
) -> Result<gix::ObjectId> {
	let empty = repo.empty_tree();
	let mut editor = empty
		.edit()
		.map_err(|err| Error::backend("git edit tree", err))?;
	for (path, entry) in map {
		validate_repo_path(path).map_err(ApplyFailure::into_error)?;
		editor
			.upsert(path.as_str(), entry_kind(entry.mode), entry.id)
			.map_err(|err| Error::backend("git edit tree", err))?;
	}
	editor
		.write()
		.map(|id| id.detach())
		.map_err(|err| Error::backend("git write tree", err))
}

fn entry_kind(mode: Mode) -> EntryKind {
	if mode == Mode::FILE_EXECUTABLE {
		EntryKind::BlobExecutable
	} else if mode == Mode::SYMLINK {
		EntryKind::Link
	} else if mode == Mode::COMMIT {
		EntryKind::Commit
	} else {
		EntryKind::Blob
	}
}

fn write_patch_worktree(
	repo: &GitRepo,
	patches: &[FilePatch],
	reverse: bool,
	state: &BTreeMap<String, FileEntry>,
) -> Result<()> {
	for patch in patches {
		let (source, target, ..) = patch_sides(patch, reverse);
		if let Some(source) = source
			&& target != Some(source)
		{
			remove_worktree_path(repo, source)?;
		}
		if let Some(target) = target {
			let entry = state.get(target).ok_or_else(|| Error::PatchFailed {
				message: format!("missing applied path {target}"),
			})?;
			write_worktree_entry(repo, target, entry, &repo.gix()?)?;
		}
	}
	Ok(())
}

fn write_worktree_map(
	repo: &GitRepo,
	previous: &BTreeMap<String, FileEntry>,
	next: &BTreeMap<String, FileEntry>,
) -> Result<()> {
	let gix_repo = repo.gix()?;
	// Every removal and every write is validated before the first one happens,
	// so a refusal cannot leave the worktree half-written. Callers that mutate
	// repository state first must call `assert_worktree_map_contained` BEFORE
	// they commit, not rely on this one.
	assert_worktree_map_contained(repo, &gix_repo, previous, next)?;
	for path in previous.keys() {
		if !next.contains_key(path) {
			remove_worktree_path(repo, path)?;
		}
	}
	for (path, entry) in next {
		if previous.get(path) != Some(entry) || !repo.root().join(path).exists() {
			write_worktree_entry(repo, path, entry, &gix_repo)?;
		}
	}
	Ok(())
}

fn write_worktree_entry(
	repo: &GitRepo,
	path: &str,
	entry: &FileEntry,
	gix_repo: &gix::Repository,
) -> Result<()> {
	validate_repo_path(path).map_err(ApplyFailure::into_error)?;
	// A symlink is unlinked and recreated, never opened through, so its leaf is
	// not resolved; resolving it would reject repositories that legitimately
	// track a link pointing outside the worktree.
	if entry.mode == Mode::SYMLINK {
		assert_prefix_within_root(repo.root(), path)?;
	} else {
		assert_within_root(repo.root(), path)?;
	}
	let absolute = repo.root().join(path);
	if let Some(parent) = absolute.parent() {
		fs::create_dir_all(parent)?;
	}
	let bytes = blob_bytes(gix_repo, entry.id)?;
	if entry.mode == Mode::SYMLINK {
		let _ = fs::remove_file(&absolute);
		#[cfg(unix)]
		{
			use std::os::unix::{ffi::OsStrExt, fs::symlink};
			symlink(std::ffi::OsStr::from_bytes(&bytes), &absolute)?;
		}
		#[cfg(not(unix))]
		fs::write(&absolute, bytes)?;
		return Ok(());
	}
	fs::write(&absolute, bytes)?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let permissions = fs::Permissions::from_mode(if entry.mode == Mode::FILE_EXECUTABLE {
			0o755
		} else {
			0o644
		});
		fs::set_permissions(&absolute, permissions)?;
	}
	Ok(())
}

fn remove_worktree_path(repo: &GitRepo, path: &str) -> Result<()> {
	validate_repo_path(path).map_err(ApplyFailure::into_error)?;
	// Only the PREFIX is resolved: `remove_file` unlinks a directory entry
	// without following it, so a tracked symlink pointing outside the worktree
	// is safe to delete and must stay deletable.
	assert_prefix_within_root(repo.root(), path)?;
	let absolute = repo.root().join(path);
	match fs::remove_file(&absolute) {
		Ok(()) => {},
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(err) => return Err(err.into()),
	}
	let mut directory = absolute.parent();
	while let Some(current) = directory {
		if current == repo.root() {
			break;
		}
		match fs::remove_dir(current) {
			Ok(()) => directory = current.parent(),
			Err(_) => break,
		}
	}
	Ok(())
}

fn validate_repo_path(path: &str) -> std::result::Result<(), ApplyFailure> {
	let candidate = PathBuf::from(path);
	if candidate.is_absolute()
		|| candidate.components().any(|component| {
			matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
		}) || path.is_empty()
	{
		return Err(ApplyFailure::Invalid(format!("unsafe patch path: {path}")));
	}
	// A patch must never touch `.git/` — doing so could overwrite hooks, the
	// index, or the objects store, giving the patch author arbitrary code
	// execution on the host. Checked per-component so `sub/.git/…` (a nested
	// repository) is rejected just like a leading `.git`.
	if candidate
		.components()
		.any(|component| is_git_store_alias(component.as_os_str()))
	{
		return Err(ApplyFailure::Invalid(format!(
			"patch path must not touch the git store: {path}"
		)));
	}
	Ok(())
}
/// Validate every path a patch would touch, before any of them is touched.
///
/// Mirrors exactly what [`write_patch_worktree`] will do per side: a source
/// that is only unlinked is checked with the prefix guard, a target that is
/// opened for writing with the full guard. Keeping the two in step is what
/// makes the preflight meaningful — a rule enforced here but not there would
/// reject a valid patch, and one enforced there but not here would reopen the
/// partial-write window this exists to close.
fn assert_patch_paths_contained(
	repo: &GitRepo,
	patches: &[FilePatch],
	reverse: bool,
) -> Result<()> {
	let gix_repo = repo.gix()?;
	for patch in patches {
		let (source, target, ..) = patch_sides(patch, reverse);
		if let Some(source) = source
			&& target != Some(source)
		{
			validate_repo_path(source).map_err(ApplyFailure::into_error)?;
			assert_prefix_within_root(repo.root(), source)?;
			assert_outside_git_store(repo, &gix_repo, source)?;
		}
		if let Some(target) = target {
			validate_repo_path(target).map_err(ApplyFailure::into_error)?;
			assert_within_root(repo.root(), target)?;
			assert_outside_git_store(repo, &gix_repo, target)?;
		}
	}
	Ok(())
}

/// Refuse a path that lands inside the repository's ACTUAL Git store.
///
/// [`validate_repo_path`] rejects components *named* `.git` under any spelling
/// a filesystem accepts, which covers the ordinary layout and nested
/// repositories. It cannot cover a store that does not carry that name: after
/// `git init --separate-git-dir=meta .` the worktree holds a `.git` FILE
/// pointing at `meta/`, and `meta/hooks/pre-commit` is both a real hook and an
/// unremarkable-looking path inside the root. Worktrees created by
/// `git worktree add` have the same shape, with `common_dir` naming the shared
/// store the linked one borrows objects and hooks from.
///
/// So the store is refused by LOCATION as well as by name — canonicalized, so
/// a symlinked or relative spelling cannot dodge the comparison.
fn assert_outside_git_store(repo: &GitRepo, gix_repo: &gix::Repository, rel: &str) -> Result<()> {
	let candidate = repo.root().join(rel);
	// The candidate usually does not exist yet, so resolve the deepest existing
	// ancestor: a store lives in directories that do exist.
	let mut probe = candidate.as_path();
	let resolved = loop {
		if let Ok(canonical) = std::fs::canonicalize(probe) {
			// Re-attach the unresolved tail so `meta/hooks/pre-commit` is still
			// compared as a path under `meta`, not just as `meta`.
			let tail = candidate.strip_prefix(probe).unwrap_or(Path::new(""));
			break canonical.join(tail);
		}
		match probe.parent() {
			Some(parent) if parent != probe => probe = parent,
			_ => return Ok(()),
		}
	};
	for store in [gix_repo.git_dir(), gix_repo.common_dir()] {
		let store = std::fs::canonicalize(&store).unwrap_or(store.to_path_buf());
		if resolved.starts_with(&store) {
			return Err(Error::backend(
				"apply worktree path",
				format!("patch path must not touch the git store: {rel}"),
			));
		}
	}
	Ok(())
}

/// Validate every path a worktree-map write would remove or create.
///
/// Split out of [`write_worktree_map`] because some callers advance repository
/// state BEFORE writing: `cherry_pick` runs `commit_as`, moving HEAD, and only
/// then writes the worktree. A refusal discovered during the write would leave
/// HEAD ahead of an index and worktree that never received the change —
/// exactly the fail-clean contract those operations document. Such callers run
/// this first, while the refusal is still free.
fn assert_worktree_map_contained(
	repo: &GitRepo,
	gix_repo: &gix::Repository,
	previous: &BTreeMap<String, FileEntry>,
	next: &BTreeMap<String, FileEntry>,
) -> Result<()> {
	for path in previous.keys() {
		if !next.contains_key(path) {
			validate_repo_path(path).map_err(ApplyFailure::into_error)?;
			assert_prefix_within_root(repo.root(), path)?;
			assert_outside_git_store(repo, gix_repo, path)?;
		}
	}
	for (path, entry) in next {
		validate_repo_path(path).map_err(ApplyFailure::into_error)?;
		// A symlink entry is written by unlinking whatever is there and calling
		// `symlink` — neither follows the old leaf, and the new one is created,
		// not opened. Resolving it would reject every operation on a repository
		// that legitimately tracks a link pointing outside the worktree:
		// `stash_push`, `stash_try_pop` and `cherry_pick` would all fail even
		// when the link is untouched, and a patch updating the link itself
		// would be refused. Only the prefix has to stay inside the root.
		if entry.mode == Mode::SYMLINK {
			assert_prefix_within_root(repo.root(), path)?;
		} else {
			assert_within_root(repo.root(), path)?;
		}
		assert_outside_git_store(repo, gix_repo, path)?;
	}
	Ok(())
}

/// Whether a single path component names the Git store under any spelling a
/// filesystem may accept for it.
///
/// Git itself refuses these same aliases when checking out a tree (see
/// `is_ntfs_dotgit` and the `core.protectNTFS`/`core.protectHFS` defaults);
/// this mirrors that rule for patch application.
fn is_git_store_alias(component: &std::ffi::OsStr) -> bool {
	let Some(name) = component.to_str() else {
		// A non-UTF-8 component cannot spell `.git` in any of the forms
		// below, and `validate_repo_path` has already rejected traversal.
		return false;
	};
	// HFS+ treats a set of zero-width and formatting codepoints as invisible
	// when comparing names, so `.\u{200c}git` opens the real `.git`. Git refuses
	// the same set under `core.protectHFS`; drop them before any comparison so
	// every rule below sees the name the filesystem will actually resolve.
	let folded: String = name.chars().filter(|c| !is_hfs_ignorable(*c)).collect();
	let name = folded.as_str();
	if name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case("git~1") {
		return true;
	}
	// NTFS and HFS+ ignore a trailing dot or space when opening a file, so
	// `.git.` and `.git ` reach the store too. Strip them before comparing.
	let trimmed = name.trim_end_matches(['.', ' ']);
	trimmed.eq_ignore_ascii_case(".git")
}

/// Whether HFS+ ignores this codepoint when comparing filenames.
///
/// The set git itself folds away under `core.protectHFS` (see `is_hfs_dotgit`
/// in `path.c`): zero-width joiners and non-joiners, directional and
/// deprecated formatting marks, the zero-width no-break space, and the
/// interlinear annotation and object-replacement characters.
const fn is_hfs_ignorable(c: char) -> bool {
	matches!(
		c,
		'\u{200c}' | '\u{200d}' | '\u{200e}' | '\u{200f}'
			| '\u{202a}'..='\u{202e}'
			| '\u{206a}'..='\u{206f}'
			| '\u{feff}'
	)
}

/// Refuse a path that would leave the worktree once symlinks are resolved.
///
/// The leaf is resolved too, so this is the guard for anything that OPENS the
/// path — writing a file, creating a directory. To delete an entry, use
/// [`assert_prefix_within_root`]: unlinking does not follow the leaf, and
/// resolving it would reject legitimately tracked symlinks.
///
/// THREAT MODEL. The check is not atomic with the write that follows it: a
/// symlink planted into the prefix between the two would still be followed.
/// Closing that window needs `openat`/`O_NOFOLLOW` descriptor-relative writes,
/// which the surrounding code does not use. It is accepted here because a
/// worktree is operated by a single agent at a time — an attacker able to
/// plant directories mid-apply already has write access to the workspace and
/// does not need a patch to exercise it. What this guard does stop is the
/// untrusted PATCH TEXT, which is the actual attacker-controlled input.
fn assert_within_root(root: &Path, rel: &str) -> Result<()> {
	assert_contained(root, rel, root.join(rel).as_path())
}

/// Refuse a path whose PREFIX would leave the worktree, ignoring the leaf.
///
/// For deletion: `remove_file` unlinks a directory entry without following it,
/// so a tracked symlink whose target is outside the worktree (or dangling) is
/// both safe to remove and must stay removable.
fn assert_prefix_within_root(root: &Path, rel: &str) -> Result<()> {
	let absolute = root.join(rel);
	// No parent means the join produced the root itself, which is contained by
	// definition; `validate_repo_path` has already refused empty and absolute
	// paths, so this cannot be an escape.
	let Some(parent) = absolute.parent() else {
		return Ok(());
	};
	assert_contained(root, rel, parent)
}

/// Walk `probe` up to the deepest existing ancestor and require it to resolve
/// inside `root`. `rel` names the original path for the error message.
fn assert_contained(root: &Path, rel: &str, probe: &Path) -> Result<()> {
	// Resolve the deepest existing ancestor against a canonicalized root so a
	// symlinked directory (macOS /var -> /private/var, or a hostile symlink
	// planted inside the root) is resolved consistently — a nonexistent leaf
	// must not be compared unresolved against a resolved root, which would
	// false-positive on symlinked prefixes.
	let root_canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
	let escaped =
		|| Error::backend("apply worktree path", format!("path escapes workspace root: {rel}"));
	let mut probe = probe;
	loop {
		if probe == root {
			return Ok(());
		}
		match std::fs::canonicalize(probe) {
			Ok(canonical) if canonical.starts_with(&root_canonical) => return Ok(()),
			Ok(_) => return Err(escaped()),
			// `canonicalize` fails both for a path that does not exist and for a
			// DANGLING symlink. Only the first may walk up: a dangling link is a
			// real directory entry, and the write below would follow it and land
			// at its target, outside the root.
			Err(_) => {
				if std::fs::symlink_metadata(probe).is_ok() {
					return Err(escaped());
				}
				match probe.parent() {
					Some(parent) if parent != probe => probe = parent,
					_ => return Ok(()),
				}
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use std::process::Command;

	use tempfile::TempDir;

	use super::*;

	fn git(cwd: &Path, args: &[&str]) -> String {
		let output = Command::new("git")
			.current_dir(cwd)
			.args(args)
			.output()
			.expect("run git");
		assert!(
			output.status.success(),
			"git {} failed: {}",
			args.join(" "),
			String::from_utf8_lossy(&output.stderr)
		);
		String::from_utf8(output.stdout).expect("git output is UTF-8")
	}
	fn git_with_index(cwd: &Path, index: &Path, args: &[&str]) -> String {
		let output = Command::new("git")
			.current_dir(cwd)
			.env("GIT_INDEX_FILE", index)
			.args(args)
			.output()
			.expect("run git with alternate index");
		assert!(
			output.status.success(),
			"git {} failed: {}",
			args.join(" "),
			String::from_utf8_lossy(&output.stderr)
		);
		String::from_utf8(output.stdout).expect("git output is UTF-8")
	}

	fn init(files: &[(&str, &[u8])]) -> TempDir {
		let temp = tempfile::tempdir().expect("tempdir");
		git(temp.path(), &["init", "-q"]);
		git(temp.path(), &["config", "user.name", "Patch Test"]);
		git(temp.path(), &["config", "user.email", "patch@example.com"]);
		for (path, bytes) in files {
			let absolute = temp.path().join(path);
			if let Some(parent) = absolute.parent() {
				fs::create_dir_all(parent).expect("create parent");
			}
			fs::write(absolute, bytes).expect("write fixture");
		}
		git(temp.path(), &["add", "-A"]);
		git(temp.path(), &["commit", "-qm", "base"]);
		temp
	}

	fn repo(path: &Path) -> GitRepo {
		GitRepo::require(path).expect("discover repo")
	}

	fn reset(path: &Path) {
		git(path, &["reset", "--hard", "-q", "HEAD"]);
		git(path, &["clean", "-fdq"]);
	}

	#[test]
	fn patch_stage_hunks_stages_intent_to_add_and_preserves_unrelated_promise() {
		let temp = init(&[("base.txt", b"one\ntwo\n")]);
		fs::write(temp.path().join("picked.txt"), b"picked\n").expect("write picked");
		fs::write(temp.path().join("promised.txt"), b"promised\n").expect("write promised");
		git(temp.path(), &["add", "-N", "picked.txt", "promised.txt"]);
		fs::write(temp.path().join("base.txt"), b"one changed\ntwo\n").expect("edit base");
		let repo = repo(temp.path());
		let raw = repo.diff_text(&DiffOptions::default()).expect("diff");
		repo
			.stage_hunks(
				&[
					HunkSelection { path: "picked.txt".into(), hunks: HunkSpec::Indices(vec![1]) },
					HunkSelection { path: "base.txt".into(), hunks: HunkSpec::All },
				],
				Some(&raw),
			)
			.expect("stage hunks over intent-to-add");
		// git is the oracle: the create patch lands on the promised entry and
		// the unrelated intent-to-add flag survives the index rewrite.
		let status = git(temp.path(), &["status", "--porcelain"]);
		assert!(status.contains("A  picked.txt"), "picked.txt staged: {status}");
		assert!(status.contains("M  base.txt"), "base.txt staged: {status}");
		assert!(status.contains(" A promised.txt"), "promised.txt keeps intent-to-add: {status}");
	}

	#[test]
	fn patch_join_and_validation_preserve_binary_terminators() {
		assert_eq!(join_patches(&["one\n\n".into(), "two".into(), String::new()]), "one\n\ntwo\n\n");
		let binary = "diff --git a/a.bin b/a.bin\nindex 1111111..2222222 100644\nGIT binary \
		              patch\nliteral 1\nIc${O300000\n\n";
		let errors = validate_hunk_selections(binary, &[
			HunkSelection { path: "missing".into(), hunks: HunkSpec::Indices(vec![1]) },
			HunkSelection { path: "a.bin".into(), hunks: HunkSpec::Indices(vec![1]) },
		]);
		assert_eq!(errors.len(), 1);
		assert_eq!(errors[0].path, "a.bin");
	}

	#[test]
	fn patch_apply_matches_git_for_text_binary_mode_rename_and_no_eof() {
		let binary: Vec<u8> = (0_u8..=255).cycle().take(4096).collect();
		let files: Vec<(&str, &[u8])> = vec![
			("text.txt", b"alpha\nbeta\ngamma\n"),
			("old.txt", b"rename body\nsecond\n"),
			("noeof.txt", b"before"),
			("script.sh", b"#!/bin/sh\necho hi\n"),
			("data.bin", &binary),
			("deleted.txt", b"delete this unique file\n"),
		];
		let ours = init(&files);
		let oracle = init(&files);
		fs::write(ours.path().join("text.txt"), b"alpha\nBETA\ngamma\n").expect("edit text");
		fs::rename(ours.path().join("old.txt"), ours.path().join("new.txt")).expect("rename");
		fs::write(ours.path().join("new.txt"), b"rename body\nchanged\n").expect("edit rename");
		git(ours.path(), &["add", "-N", "new.txt"]);
		fs::remove_file(ours.path().join("deleted.txt")).expect("delete file");
		fs::write(ours.path().join("created.txt"), b"new unique file\n").expect("create file");
		git(ours.path(), &["add", "-N", "created.txt"]);
		fs::write(ours.path().join("noeof.txt"), b"after").expect("edit no-eof");
		let mut changed_binary = binary;
		changed_binary[7] ^= 0xff;
		changed_binary.extend_from_slice(b"\0tail");
		fs::write(ours.path().join("data.bin"), &changed_binary).expect("edit binary");
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			fs::set_permissions(ours.path().join("script.sh"), fs::Permissions::from_mode(0o755))
				.expect("chmod");
		}
		let patch = git(ours.path(), &["diff", "--binary", "--find-renames"]);
		reset(ours.path());
		assert!(
			repo(ours.path())
				.can_apply_patch(&patch, &ApplyOptions::default())
				.expect("check patch")
		);
		repo(ours.path())
			.apply_patch(&patch, &ApplyOptions::default())
			.expect("apply worktree");
		let patch_file = oracle.path().join("change.patch");
		fs::write(&patch_file, &patch).expect("write patch");
		git(oracle.path(), &["apply", "--binary", "change.patch"]);
		fs::remove_file(patch_file).expect("remove patch");
		assert_eq!(
			git(ours.path(), &["status", "--porcelain"]),
			git(oracle.path(), &["status", "--porcelain"])
		);
		for path in ["text.txt", "new.txt", "noeof.txt", "script.sh", "data.bin", "created.txt"] {
			assert_eq!(
				fs::read(ours.path().join(path))
					.unwrap_or_else(|err| panic!("read ours {path}: {err}")),
				fs::read(oracle.path().join(path)).expect("read oracle"),
				"{path}"
			);
		}
		assert!(!ours.path().join("deleted.txt").exists());
		assert!(!oracle.path().join("deleted.txt").exists());
		repo(ours.path())
			.apply_patch(&patch, &ApplyOptions {
				cached:     false,
				index_path: None,
				reverse:    true,
				three_way:  false,
			})
			.expect("reverse worktree patch");
		let patch_file = oracle.path().join("change.patch");
		fs::write(&patch_file, &patch).expect("write reverse patch");
		git(oracle.path(), &["apply", "--reverse", "--binary", "change.patch"]);
		fs::remove_file(patch_file).expect("remove reverse patch");
		assert_eq!(
			git(ours.path(), &["status", "--porcelain"]),
			git(oracle.path(), &["status", "--porcelain"])
		);

		reset(ours.path());
		reset(oracle.path());
		repo(ours.path())
			.apply_patch(&patch, &ApplyOptions {
				cached:     true,
				index_path: None,
				reverse:    false,
				three_way:  false,
			})
			.expect("apply cached");
		let patch_file = oracle.path().join("change.patch");
		fs::write(&patch_file, &patch).expect("write patch");
		git(oracle.path(), &["apply", "--cached", "--binary", "change.patch"]);
		fs::remove_file(patch_file).expect("remove patch");
		assert_eq!(git(ours.path(), &["write-tree"]), git(oracle.path(), &["write-tree"]));
		assert_eq!(
			git(ours.path(), &["status", "--porcelain"]),
			git(oracle.path(), &["status", "--porcelain"])
		);
	}

	#[test]
	fn patch_stage_hunks_selects_indices_and_lines() {
		let original = (1..=20).fold(String::new(), |mut out, line| {
			use std::fmt::Write as _;
			let _ = writeln!(out, "line {line}");
			out
		});
		let temp = init(&[("file.txt", original.as_bytes())]);
		let changed = original
			.replace("line 2\n", "LINE TWO\n")
			.replace("line 18\n", "LINE EIGHTEEN\n");
		fs::write(temp.path().join("file.txt"), changed).expect("edit");
		let diff = git(temp.path(), &["diff", "--unified=1"]);
		let repository = repo(temp.path());
		repository
			.stage_hunks(
				&[HunkSelection { path: "file.txt".into(), hunks: HunkSpec::Indices(vec![1]) }],
				Some(&diff),
			)
			.expect("stage first hunk");
		let staged = git(temp.path(), &["show", ":file.txt"]);
		assert!(staged.contains("LINE TWO"));
		assert!(staged.contains("line 18"));
		assert!(!staged.contains("LINE EIGHTEEN"));
	}
	#[test]
	fn patch_cached_mixed_creation_uses_dev_null_with_alternate_index() {
		let temp = init(&[("tracked.txt", b"base\n")]);
		fs::create_dir_all(temp.path().join("src")).expect("create src");
		fs::write(temp.path().join("src/new.py"), b"WIP header\nunchanged\n")
			.expect("write WIP file");
		let repository = repo(temp.path());
		let created = repository
			.diff_no_index(Path::new("/dev/null"), Path::new("src/new.py"), true)
			.expect("creation patch");
		assert!(created.contains("--- /dev/null"));

		git(temp.path(), &["add", "src/new.py"]);
		fs::write(temp.path().join("tracked.txt"), b"changed\n").expect("edit tracked");
		fs::write(temp.path().join("src/new.py"), b"WIP header\nagent-edit\n")
			.expect("edit WIP file");
		let modified = repository
			.diff_text(&DiffOptions { binary: true, ..DiffOptions::default() })
			.expect("mixed tracked patch");
		assert!(modified.contains("--- a/src/new.py"));

		let ours_index = temp.path().join("ours-mixed.index");
		let oracle_index = temp.path().join("oracle-mixed.index");
		repository
			.read_tree("HEAD", Some(&ours_index))
			.expect("seed ours index");
		git_with_index(temp.path(), &oracle_index, &["read-tree", "HEAD"]);
		let options = ApplyOptions {
			cached:     true,
			index_path: Some(ours_index.clone()),
			reverse:    false,
			three_way:  false,
		};
		repository
			.apply_patch(&created, &options)
			.expect("apply creation patch");
		repository
			.apply_patch(&modified, &options)
			.expect("apply following modification patch");

		let creation_file = temp.path().join("creation.patch");
		let modified_file = temp.path().join("modified.patch");
		fs::write(&creation_file, &created).expect("write creation patch");
		fs::write(&modified_file, &modified).expect("write modified patch");
		git_with_index(temp.path(), &oracle_index, &[
			"apply",
			"--cached",
			"--binary",
			"creation.patch",
		]);
		git_with_index(temp.path(), &oracle_index, &[
			"apply",
			"--cached",
			"--binary",
			"modified.patch",
		]);
		let ours_tree = repository
			.write_tree(Some(&ours_index))
			.expect("write ours mixed tree");
		let oracle_tree = git_with_index(temp.path(), &oracle_index, &["write-tree"]);
		assert_eq!(ours_tree, oracle_tree.trim());
	}

	#[test]
	fn patch_cached_alternate_index_matches_git_and_preserves_real_index() {
		let temp = init(&[("file.txt", b"base\n")]);
		fs::write(temp.path().join("file.txt"), b"patched\n").expect("edit");
		let patch = git(temp.path(), &["diff", "--full-index"]);
		reset(temp.path());
		let repository = repo(temp.path());
		let ours_index = temp.path().join("ours.index");
		let oracle_index = temp.path().join("oracle.index");
		repository
			.read_tree("HEAD", Some(&ours_index))
			.expect("seed ours index");
		git_with_index(temp.path(), &oracle_index, &["read-tree", "HEAD"]);
		let real_index_before = fs::read(temp.path().join(".git/index")).expect("real index");
		let options = ApplyOptions {
			cached:     true,
			index_path: Some(ours_index.clone()),
			reverse:    false,
			three_way:  false,
		};
		let ours_before_check = fs::read(&ours_index).expect("ours index before check");
		assert!(
			repository
				.can_apply_patch(&patch, &options)
				.expect("alternate index check")
		);
		assert_eq!(fs::read(&ours_index).expect("ours index after check"), ours_before_check);
		repository
			.apply_patch(&patch, &options)
			.expect("apply alternate index");
		let patch_file = temp.path().join("alternate.patch");
		fs::write(&patch_file, &patch).expect("write patch");
		git_with_index(temp.path(), &oracle_index, &[
			"apply",
			"--cached",
			"--binary",
			"alternate.patch",
		]);
		fs::remove_file(patch_file).expect("remove patch");
		let ours_tree = repository
			.write_tree(Some(&ours_index))
			.expect("write ours tree");
		let oracle_tree = git_with_index(temp.path(), &oracle_index, &["write-tree"]);
		assert_eq!(ours_tree, oracle_tree.trim());
		assert_eq!(
			fs::read(temp.path().join(".git/index")).expect("real index after"),
			real_index_before
		);
	}

	#[test]
	fn patch_three_way_check_merges_drift_and_rejects_conflict() {
		let temp = init(&[("file.txt", b"one\ntwo\nthree\n")]);
		fs::write(temp.path().join("file.txt"), b"one\nTWO\nthree\n").expect("patch edit");
		let patch = git(temp.path(), &["diff", "--full-index"]);
		reset(temp.path());
		fs::write(temp.path().join("file.txt"), b"ONE\ntwo\nthree\n").expect("drift");
		let repository = repo(temp.path());
		assert!(
			!repository
				.can_apply_patch(&patch, &ApplyOptions::default())
				.expect("direct check")
		);
		let three_way =
			ApplyOptions { cached: false, index_path: None, reverse: false, three_way: true };
		assert!(
			repository
				.can_apply_patch(&patch, &three_way)
				.expect("three-way check")
		);
		repository
			.apply_patch(&patch, &three_way)
			.expect("three-way apply");
		assert_eq!(
			fs::read(temp.path().join("file.txt")).expect("merged file"),
			b"ONE\nTWO\nthree\n"
		);

		reset(temp.path());
		fs::write(temp.path().join("file.txt"), b"one\nOTHER\nthree\n").expect("conflict");
		assert!(
			!repository
				.can_apply_patch(&patch, &three_way)
				.expect("conflict check")
		);
	}

	#[test]
	fn patch_cherry_pick_and_stash_are_fail_clean() {
		let temp = init(&[("file.txt", b"base\n"), (".gitignore", b"ignored.txt\n")]);
		let base_branch = git(temp.path(), &["branch", "--show-current"])
			.trim()
			.to_owned();
		let base_sha = git(temp.path(), &["rev-parse", "HEAD"]).trim().to_owned();
		git(temp.path(), &["checkout", "-qb", "topic"]);
		fs::write(temp.path().join("file.txt"), b"base\ntopic\n").expect("topic edit");
		git(temp.path(), &["commit", "-qam", "topic change"]);
		let topic = git(temp.path(), &["rev-parse", "HEAD"]).trim().to_owned();
		fs::write(temp.path().join("remaining.txt"), b"remaining\n").expect("remaining edit");
		git(temp.path(), &["add", "remaining.txt"]);
		git(temp.path(), &["commit", "-qm", "remaining change"]);
		let topic_tip = git(temp.path(), &["rev-parse", "HEAD"]).trim().to_owned();
		git(temp.path(), &["checkout", "-q", &base_branch]);
		let repository = repo(temp.path());
		repository.cherry_pick(&topic).expect("clean cherry-pick");
		assert_eq!(fs::read(temp.path().join("file.txt")).expect("read"), b"base\ntopic\n");
		assert!(matches!(repository.cherry_pick(&topic), Err(Error::EmptyCherryPick { .. })));
		for commit in repository
			.rev_list_range(&base_sha, &topic_tip)
			.expect("topic range")
		{
			match repository.cherry_pick(&commit) {
				Ok(()) | Err(Error::EmptyCherryPick { .. }) => {},
				Err(error) => panic!("range cherry-pick failed: {error}"),
			}
		}
		assert_eq!(
			fs::read(temp.path().join("remaining.txt")).expect("remaining landed"),
			b"remaining\n"
		);
		let conflict = init(&[("file.txt", b"base\n")]);
		let conflict_base = git(conflict.path(), &["branch", "--show-current"])
			.trim()
			.to_owned();
		git(conflict.path(), &["checkout", "-qb", "other"]);
		fs::write(conflict.path().join("file.txt"), b"other\n").expect("other edit");
		git(conflict.path(), &["commit", "-qam", "other"]);
		let other = git(conflict.path(), &["rev-parse", "HEAD"])
			.trim()
			.to_owned();
		git(conflict.path(), &["checkout", "-q", &conflict_base]);
		fs::write(conflict.path().join("file.txt"), b"current\n").expect("current edit");
		git(conflict.path(), &["commit", "-qam", "current"]);
		let before_head = git(conflict.path(), &["rev-parse", "HEAD"]);
		let before_index = fs::read(conflict.path().join(".git/index")).expect("conflict index");
		assert!(matches!(repo(conflict.path()).cherry_pick(&other), Err(Error::Conflict { .. })));
		assert_eq!(git(conflict.path(), &["rev-parse", "HEAD"]), before_head);
		assert_eq!(
			fs::read(conflict.path().join(".git/index")).expect("conflict index after"),
			before_index
		);
		assert_eq!(fs::read(conflict.path().join("file.txt")).expect("conflict file"), b"current\n");

		fs::write(temp.path().join("file.txt"), b"base\ntopic\nstaged\n").expect("staged edit");
		git(temp.path(), &["add", "file.txt"]);
		fs::write(temp.path().join("file.txt"), b"base\ntopic\nstaged\nworktree\n")
			.expect("worktree edit");
		fs::write(temp.path().join("untracked.txt"), b"untracked\n").expect("untracked");
		fs::write(temp.path().join("ignored.txt"), b"ignored\n").expect("ignored");
		assert!(
			repository
				.stash_push(Some("roundtrip"))
				.expect("stash push")
		);
		assert!(!temp.path().join("untracked.txt").exists());
		assert!(temp.path().join("ignored.txt").exists());
		assert!(repository.stash_try_pop(true).expect("stash pop"));
		assert_eq!(
			fs::read(temp.path().join("file.txt")).expect("restored"),
			b"base\ntopic\nstaged\nworktree\n"
		);
		assert_eq!(
			fs::read(temp.path().join("untracked.txt")).expect("restored untracked"),
			b"untracked\n"
		);
		assert_eq!(fs::read(temp.path().join("ignored.txt")).expect("ignored remains"), b"ignored\n");

		reset(temp.path());
		fs::write(temp.path().join("file.txt"), b"stashed\n").expect("stash conflict edit");
		assert!(repository.stash_push(None).expect("stash conflict"));
		fs::write(temp.path().join("file.txt"), b"current\n").expect("current conflict edit");
		let before_file = fs::read(temp.path().join("file.txt")).expect("before file");
		let before_index = fs::read(temp.path().join(".git/index")).expect("before index");
		let before_stash = git(temp.path(), &["rev-parse", "refs/stash"]);
		assert!(!repository.stash_try_pop(false).expect("conflicting pop"));
		assert_eq!(fs::read(temp.path().join("file.txt")).expect("after file"), before_file);
		assert_eq!(fs::read(temp.path().join(".git/index")).expect("after index"), before_index);
		assert_eq!(git(temp.path(), &["rev-parse", "refs/stash"]), before_stash);
	}
	#[test]
	fn patch_stash_pop_preserves_older_stack_entry() {
		let temp = init(&[("file.txt", b"base\n")]);
		let repository = repo(temp.path());
		fs::write(temp.path().join("file.txt"), b"first\n").expect("first stash");
		assert!(repository.stash_push(Some("first")).expect("push first"));
		let first = git(temp.path(), &["rev-parse", "refs/stash"]);
		fs::write(temp.path().join("file.txt"), b"second\n").expect("second stash");
		assert!(repository.stash_push(Some("second")).expect("push second"));
		assert_ne!(git(temp.path(), &["rev-parse", "refs/stash"]), first);
		assert!(repository.stash_try_pop(false).expect("pop second"));
		assert_eq!(git(temp.path(), &["rev-parse", "refs/stash"]), first);
		assert_eq!(fs::read(temp.path().join("file.txt")).expect("second restored"), b"second\n");
	}

	#[test]
	fn validate_repo_path_rejects_git_store_and_escapes() {
		for path in [
			".git",
			".git/config",
			".git/hooks/pre-commit",
			"sub/.git/objects",
			"../outside",
			"/abs/path",
			"",
		] {
			assert!(validate_repo_path(path).is_err(), "expected {path:?} to be rejected");
		}
		// Ordinary relative paths still apply.
		assert!(validate_repo_path("src/main.rs").is_ok());
		assert!(validate_repo_path("a/b/c.txt").is_ok());
	}

	#[test]
	fn validate_repo_path_rejects_case_insensitive_git_aliases() {
		// macOS and Windows default to case-insensitive filesystems, where each
		// of these opens the real `.git`. The byte-exact check they replaced
		// admitted every one of them.
		for path in [
			".GIT/hooks/pre-commit",
			".Git/config",
			".gIt/objects/pack",
			"sub/.GIT/config",
			"GIT~1/hooks/pre-commit",
			"git~1/config",
			".git./config",
			".git /config",
		] {
			assert!(validate_repo_path(path).is_err(), "expected {path:?} to be rejected");
		}
		// Names that merely start with or contain `git` are ordinary files.
		assert!(validate_repo_path("gitignore").is_ok());
		assert!(validate_repo_path(".gitignore").is_ok());
		assert!(validate_repo_path("src/git/patch.rs").is_ok());
		assert!(validate_repo_path("digits/x.txt").is_ok());
	}

	#[test]
	fn validate_repo_path_rejects_hfs_ignorable_git_spellings() {
		// HFS+ folds these codepoints away when comparing names, so each of
		// these opens the real `.git`. Git refuses the same set under
		// `core.protectHFS`; a nested repository under `sub/` is reachable the
		// same way, and `assert_outside_git_store` cannot help there — it only
		// knows the OUTER repository's store.
		for path in [
			".\u{200c}git/config",
			".g\u{200d}it/hooks/pre-commit",
			".gi\u{feff}t/objects",
			".git\u{202a}/config",
			"sub/.\u{200c}git/hooks/pre-commit",
			"sub/.g\u{206f}it/config",
		] {
			assert!(validate_repo_path(path).is_err(), "expected {path:?} to be rejected");
		}
		// Ordinary names containing no ignorable characters still apply.
		assert!(validate_repo_path("sub/gitignore").is_ok());
		assert!(validate_repo_path("digit/x.txt").is_ok());
	}

	#[test]
	fn apply_patch_refuses_to_write_through_symlink_outside_root() {
		let temp = init(&[("keep.txt", b"base\n")]);
		let outside = tempfile::tempdir().expect("outside tempdir");
		// A symlinked directory inside the worktree aliases an untracked path
		// that would otherwise be written into the repo root. The patch must
		// not follow the link out of the worktree.
		#[cfg(unix)]
		{
			use std::os::unix::fs::symlink;
			symlink(outside.path(), temp.path().join("link")).expect("create symlink");
		}
		#[cfg(not(unix))]
		{
			let _ = &outside;
		}
		let repository = repo(temp.path());
		// Craft a minimal "new file" patch targeting the symlinked prefix.
		let patch = concat!(
			"diff --git a/link/sneaky.txt b/link/sneaky.txt\n",
			"new file mode 100644\n",
			"index 0000000..3b18e51\n",
			"--- /dev/null\n",
			"+++ b/link/sneaky.txt\n",
			"@@ -0,0 +1 @@\n",
			"+pwned\n",
		);
		let result = repository.apply_patch(patch, &ApplyOptions::default());
		#[cfg(unix)]
		{
			assert!(result.is_err(), "patch must refuse symlink traversal");
			assert!(
				!outside.path().join("sneaky.txt").exists(),
				"file written through symlink escape"
			);
		}
		#[cfg(not(unix))]
		{
			let _ = result;
		}
	}

	#[test]
	fn apply_patch_refuses_to_write_through_dangling_symlink_outside_root() {
		let temp = init(&[("keep.txt", b"base\n")]);
		let outside = tempfile::tempdir().expect("outside tempdir");
		// The link's target does not exist yet. `canonicalize` fails for a
		// dangling link exactly as it does for an absent path, so a guard that
		// walks up on any error admits it — and `create_dir_all` then
		// materialises the target while following the link.
		let ghost = outside.path().join("ghost");
		#[cfg(unix)]
		{
			use std::os::unix::fs::symlink;
			symlink(&ghost, temp.path().join("link")).expect("create dangling symlink");
		}
		#[cfg(not(unix))]
		{
			let _ = &outside;
		}
		let repository = repo(temp.path());
		let patch = concat!(
			"diff --git a/link/sneaky.txt b/link/sneaky.txt\n",
			"new file mode 100644\n",
			"index 0000000..3b18e51\n",
			"--- /dev/null\n",
			"+++ b/link/sneaky.txt\n",
			"@@ -0,0 +1 @@\n",
			"+pwned\n",
		);
		let result = repository.apply_patch(patch, &ApplyOptions::default());
		#[cfg(unix)]
		{
			assert!(result.is_err(), "patch must refuse dangling symlink traversal");
			assert!(!ghost.exists(), "patch materialised the link target outside the root");
		}
		#[cfg(not(unix))]
		{
			let _ = result;
			let _ = &ghost;
		}
	}

	#[test]
	fn apply_patch_leaves_worktree_untouched_when_a_later_path_escapes() {
		let temp = init(&[("keep.txt", b"base\n"), ("doomed.txt", b"victim\n")]);
		let outside = tempfile::tempdir().expect("outside tempdir");
		#[cfg(unix)]
		{
			use std::os::unix::fs::symlink;
			symlink(outside.path(), temp.path().join("link")).expect("create symlink");
		}
		#[cfg(not(unix))]
		{
			let _ = &outside;
		}
		let repository = repo(temp.path());
		// Three sides in order: a legitimate edit, a legitimate deletion, then
		// a target beneath the escaping symlink. Validating per-side as each is
		// reached would commit the first two before refusing the third.
		let patch = concat!(
			"diff --git a/keep.txt b/keep.txt\n",
			"--- a/keep.txt\n",
			"+++ b/keep.txt\n",
			"@@ -1 +1 @@\n",
			"-base\n",
			"+edited\n",
			"diff --git a/doomed.txt b/doomed.txt\n",
			"deleted file mode 100644\n",
			"--- a/doomed.txt\n",
			"+++ /dev/null\n",
			"@@ -1 +0,0 @@\n",
			"-victim\n",
			"diff --git a/link/sneaky.txt b/link/sneaky.txt\n",
			"new file mode 100644\n",
			"--- /dev/null\n",
			"+++ b/link/sneaky.txt\n",
			"@@ -0,0 +1 @@\n",
			"+pwned\n",
		);
		let result = repository.apply_patch(patch, &ApplyOptions::default());
		#[cfg(unix)]
		{
			assert!(result.is_err(), "patch must refuse the escaping target");
			assert_eq!(
				fs::read(temp.path().join("keep.txt")).expect("keep.txt"),
				b"base\n",
				"earlier edit was committed before the escape was caught"
			);
			assert!(
				temp.path().join("doomed.txt").exists(),
				"earlier deletion was committed before the escape was caught"
			);
			assert!(!outside.path().join("sneaky.txt").exists(), "escape was written");
		}
		#[cfg(not(unix))]
		{
			let _ = result;
		}
	}

	#[test]
	fn can_apply_patch_reports_false_for_an_escaping_patch() {
		let temp = init(&[("keep.txt", b"base\n")]);
		let outside = tempfile::tempdir().expect("outside tempdir");
		#[cfg(unix)]
		{
			use std::os::unix::fs::symlink;
			symlink(outside.path(), temp.path().join("link")).expect("create symlink");
		}
		#[cfg(not(unix))]
		{
			let _ = &outside;
		}
		let repository = repo(temp.path());
		let patch = concat!(
			"diff --git a/link/sneaky.txt b/link/sneaky.txt\n",
			"new file mode 100644\n",
			"--- /dev/null\n",
			"+++ b/link/sneaky.txt\n",
			"@@ -0,0 +1 @@\n",
			"+pwned\n",
		);
		#[cfg(unix)]
		{
			// The predicate must agree with `apply_patch`: a caller gating on it
			// would otherwise treat an attack as a viable change.
			assert!(
				!repository
					.can_apply_patch(patch, &ApplyOptions::default())
					.expect("probe"),
				"can_apply_patch claimed an escaping patch applies"
			);
			assert!(
				repository
					.apply_patch(patch, &ApplyOptions::default())
					.is_err()
			);
		}
		#[cfg(not(unix))]
		{
			let _ = (&repository, patch);
		}
	}

	#[test]
	#[cfg(unix)]
	fn apply_patch_deletes_a_tracked_symlink_pointing_outside_the_root() {
		use std::os::unix::fs::symlink;

		// A repository may legitimately track a symlink whose target lies
		// outside the worktree. Unlinking it does not follow it, so deletion
		// must keep working — resolving the leaf would break ordinary patch
		// deletion, cherry-pick, and stash cleanup.
		let temp = init(&[("keep.txt", b"base\n")]);
		let outside = tempfile::tempdir().expect("outside tempdir");
		let link = temp.path().join("outbound");
		symlink(outside.path().join("target"), &link).expect("create dangling symlink");
		git(temp.path(), &["add", "outbound"]);
		git(temp.path(), &["commit", "-m", "track symlink"]);

		let repository = repo(temp.path());
		let target_id = git(temp.path(), &["rev-parse", "HEAD:outbound"]);
		let patch = format!(
			concat!(
				"diff --git a/outbound b/outbound\n",
				"deleted file mode 120000\n",
				"index {}..0000000\n",
				"--- a/outbound\n",
				"+++ /dev/null\n",
				"@@ -1 +0,0 @@\n",
				"-{}\n",
				"\\ No newline at end of file\n",
			),
			&target_id[..7],
			outside.path().join("target").display(),
		);
		repository
			.apply_patch(&patch, &ApplyOptions::default())
			.expect("delete tracked symlink");
		assert!(
			link.symlink_metadata().is_err(),
			"tracked symlink pointing outside the root was not deleted"
		);
	}

	#[test]
	#[cfg(unix)]
	fn apply_patch_refuses_to_read_a_source_through_an_escaping_symlink() {
		use std::os::unix::fs::symlink;

		// The source side is READ to build the state map. Before the preflight
		// moved ahead of that read, a patch could name a source beneath an
		// escaping symlink and have it slurped from outside the worktree — and
		// pointing it at a device or FIFO turned that read into unbounded
		// memory growth or a hang.
		let temp = init(&[("keep.txt", b"base\n")]);
		let outside = tempfile::tempdir().expect("outside tempdir");
		let secret = outside.path().join("secret.txt");
		fs::write(&secret, b"classified\n").expect("write secret");
		symlink(outside.path(), temp.path().join("link")).expect("create symlink");

		let repository = repo(temp.path());
		let objects_before = loose_object_count(temp.path());
		let patch = concat!(
			"diff --git a/link/secret.txt b/link/secret.txt\n",
			"deleted file mode 100644\n",
			"--- a/link/secret.txt\n",
			"+++ /dev/null\n",
			"@@ -1 +0,0 @@\n",
			"-classified\n",
		);
		assert!(
			repository
				.apply_patch(patch, &ApplyOptions::default())
				.is_err(),
			"patch must refuse a source outside the worktree"
		);
		assert!(
			!repository
				.can_apply_patch(patch, &ApplyOptions::default())
				.expect("probe"),
			"probe must refuse it too — it reads the same sources"
		);
		assert!(secret.exists(), "external file was consumed by the patch");
		// Refusal alone proves nothing: the patch is refused either way, just
		// later. What separates the two orderings is whether the source was
		// READ — `augment_patch_sources` writes every source it reads into the
		// object store as a blob, so the store growing is the observable
		// evidence that external content was slurped before the refusal.
		assert_eq!(
			loose_object_count(temp.path()),
			objects_before,
			"external file was read into the object store before the refusal"
		);
	}

	#[test]
	fn apply_patch_refuses_a_separate_git_dir_store() {
		// `git init --separate-git-dir=meta .` leaves a `.git` FILE pointing at
		// `meta/`, so the real store carries a name the component check cannot
		// recognise. `meta/hooks/pre-commit` is a genuine hook and an
		// unremarkable-looking path inside the root.
		let temp = TempDir::new().expect("tempdir");
		git(temp.path(), &["init", "--separate-git-dir=meta", "."]);
		git(temp.path(), &["config", "user.email", "test@example.com"]);
		git(temp.path(), &["config", "user.name", "Test"]);
		fs::write(temp.path().join("keep.txt"), b"base\n").expect("seed");
		git(temp.path(), &["add", "keep.txt"]);
		git(temp.path(), &["commit", "-m", "seed"]);

		let repository = repo(temp.path());
		let patch = concat!(
			"diff --git a/meta/hooks/pre-commit b/meta/hooks/pre-commit\n",
			"new file mode 100755\n",
			"--- /dev/null\n",
			"+++ b/meta/hooks/pre-commit\n",
			"@@ -0,0 +1 @@\n",
			"+#!/bin/sh\n",
		);
		assert!(
			repository
				.apply_patch(patch, &ApplyOptions::default())
				.is_err(),
			"patch installed a hook into the separate git dir"
		);
		assert!(
			!temp.path().join("meta/hooks/pre-commit").exists(),
			"hook was written into the real git store"
		);
		// A path that merely shares the prefix is not the store.
		assert!(validate_repo_path("metadata/notes.txt").is_ok());
	}

	#[test]
	#[cfg(unix)]
	fn stash_roundtrip_preserves_a_tracked_outbound_symlink() {
		use std::os::unix::fs::symlink;

		// A repository may track a symlink whose target lies outside the
		// worktree. Writing such an entry unlinks and recreates the link
		// without following it, so stash and pop must keep working — resolving
		// the leaf would fail every operation that merely carries the entry
		// along, even when the link itself is untouched.
		let temp = init(&[("keep.txt", b"base\n")]);
		let outside = tempfile::tempdir().expect("outside tempdir");
		let link = temp.path().join("outbound");
		symlink(outside.path().join("target"), &link).expect("create symlink");
		git(temp.path(), &["add", "outbound"]);
		git(temp.path(), &["commit", "-m", "track outbound symlink"]);

		let repository = repo(temp.path());
		fs::write(temp.path().join("keep.txt"), b"dirty\n").expect("dirty the tree");
		assert!(
			repository.stash_push(Some("wip")).expect("stash"),
			"stash must not choke on the link"
		);
		assert!(link.symlink_metadata().is_ok(), "stash removed the tracked symlink");
		assert!(repository.stash_try_pop(false).expect("pop"), "pop must restore the change");
		assert_eq!(fs::read(temp.path().join("keep.txt")).expect("keep.txt"), b"dirty\n");
		assert!(link.symlink_metadata().is_ok(), "pop lost the tracked symlink");
	}

	#[test]
	#[cfg(unix)]
	fn cherry_pick_refuses_before_advancing_head_when_a_path_escapes() {
		use std::os::unix::fs::symlink;

		// `cherry_pick` merges trees, commits with `commit_as` — moving HEAD —
		// and only THEN writes the worktree. A path refused during that write
		// leaves HEAD ahead of a worktree that never received the change.
		//
		// The escape has to be invisible to the tree merge, so it is planted in
		// the worktree only: `nested/` is a real tracked directory in both
		// trees, replaced on disk by a symlink pointing outside the root.
		let temp = init(&[("keep.txt", b"base\n")]);
		fs::create_dir_all(temp.path().join("nested")).expect("mkdir");
		fs::write(temp.path().join("nested/tracked.txt"), b"one\n").expect("write");
		git(temp.path(), &["add", "nested/tracked.txt"]);
		git(temp.path(), &["commit", "-m", "track nested"]);

		git(temp.path(), &["checkout", "-q", "-b", "side"]);
		fs::write(temp.path().join("nested/tracked.txt"), b"two\n").expect("write");
		git(temp.path(), &["commit", "-aqm", "edit nested"]);
		let side = git(temp.path(), &["rev-parse", "HEAD"]).trim().to_string();
		git(temp.path(), &["checkout", "-q", "-"]);

		// Swap the real directory for a link out of the worktree. The trees
		// still merge cleanly — only the write is an escape.
		let outside = tempfile::tempdir().expect("outside tempdir");
		fs::remove_dir_all(temp.path().join("nested")).expect("drop real dir");
		symlink(outside.path(), temp.path().join("nested")).expect("create symlink");

		let repository = repo(temp.path());
		let head_before = git(temp.path(), &["rev-parse", "HEAD"]);
		assert!(repository.cherry_pick(&side).is_err(), "cherry-pick must refuse the escaping path");
		assert_eq!(
			git(temp.path(), &["rev-parse", "HEAD"]),
			head_before,
			"HEAD advanced even though the worktree write was refused"
		);
		assert!(!outside.path().join("tracked.txt").exists(), "file written outside the worktree");
	}

	/// Number of loose objects in a repository's store.
	fn loose_object_count(cwd: &Path) -> usize {
		let output = git(cwd, &["count-objects", "-v"]);
		output
			.lines()
			.find_map(|line| line.strip_prefix("count: "))
			.and_then(|count| count.trim().parse().ok())
			.expect("count-objects reports a count")
	}
}
