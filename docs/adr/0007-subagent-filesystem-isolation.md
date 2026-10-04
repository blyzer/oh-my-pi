# 0007. Subagents get a copy-on-write view and return a diff

Status: accepted
Date: 2026-09-02
Area: runtime

## Context

The host/sandbox boundary (0006) is usually discussed as host versus VM. The multiplexed-workspace
row of the envelope (0001) raises the same question one layer down: many agents share one
workspace, and a child that writes into the parent's working tree is mutating the parent's authority
directly.

The common answer, `git worktree`, isolates tracked files only. Untracked build output, generated
files, dependency directories, local config, and anything gitignored are either missing from the
child (so its builds fail) or shared with the parent (so its writes collide). The child therefore
starts in a workspace that is neither the parent's nor a faithful copy.

`pi-iso` established the working alternative: give each child a copy-on-write view of the whole
workspace. The backend is whatever the filesystem offers — APFS clonefile, btrfs, ZFS, overlayfs,
ProjFS on Windows — with a plain copy as the fallback. The child diverges freely; when it finishes,
the parent receives a diff.

## Decision

- A subagent that may write MUST receive its own view of the workspace, not of the tracked subset
  only. The implemented view is every tracked file plus every untracked file that is not
  gitignored (hidden files included, `.git` excluded), captured as a content-addressed manifest
  of the live workspace generation. Gitignored paths (build output, dependency directories) are
  not copied; carrying them is an open gap, so a child that needs them must restore them itself.
- The view is created per file. Each file is cloned copy-on-write where the filesystem supports
  it (APFS `clonefile` on macOS, `FICLONE` reflink on Linux) and copied otherwise. The copy
  fallback never shares a writable inode with the parent. This is the only implemented isolation
  mechanism. Other mechanisms (btrfs or ZFS snapshots, overlayfs, ProjFS, block clone, a git
  worktree or recursive-copy backend) are future targets, not current behavior, and no setting
  selects among backends. The child cannot observe which path was taken.
- The child MUST NOT share the parent's mutable authority. It never writes into the parent's tree;
  it writes into its view.
- The child's result MUST be returned as changes — a content-addressed patch or a retained branch —
  which the parent applies or merges under its own policy. Whether to apply is the parent's
  decision, made after the child has settled.
- The isolation is the filesystem form of 0006: the parent is the host, the child's view is the
  sandbox, the diff is the bounded stream crossing back.

## Consequences

- Parallel children can edit overlapping files without corrupting each other; conflicts surface as
  a merge decision at the parent, not as interleaved writes.
- Children see tracked and untracked non-ignored files, so work on new files in progress is
  visible. Gitignored build output and dependency directories are not in the view (see Decision).
- A child that fails or is cancelled leaves nothing in the parent's tree; its view is discarded or
  retained for inspection.
- Prohibited: children spawned with direct write access to the parent workspace when isolation is
  available. Prohibited: worktree-only isolation presented as full isolation.
- Cost accepted: a copy fallback on filesystems without CoW is slow and space-hungry for large
  workspaces. That cost is paid by the fallback, not by weakening the rule; creation refuses an
  untracked tree over the snapshot budget rather than copy it unbounded.
- Cost accepted: the parent must own merge and conflict policy. That is where it belongs.

## Amendment (2026-10-04)

The owner decided to bring this record in line with the code. The decision originally said the
backend is whatever the filesystem offers (APFS, btrfs, ZFS, overlayfs, ProjFS) and that backend
choice is a setting. Only per-file reflink with a copy fallback is implemented. The other
mechanisms stay as possible future targets and are no longer described as selectable. The
`sv_task_isolation_mode` convar and its `TaskIsolationMode` enum
(`crates/driver/src/subagent/settings.rs`) advertise backends that have no implementation, and
nothing selects a backend from them, so their unimplemented options are to be removed in a
follow-up code change; this record does not wait for that change. The rest of the decision stands:
every writing child works in its own view and returns a patch or branch that the parent applies
under its own policy.

## Status in omp

**Status: Partially implemented.** Every subagent gets a copy-on-write workspace and returns a patch or branch, with per-file reflink and a copy fallback as the only backend. Open: gitignored files are not copied, and the inert `sv_task_isolation_mode` convar still lists unimplemented backends until the follow-up code change. (Verified 2026-10-04 against `omp2` at `9b2d91fe9d`.)

- Isolation and return path: `create_isolation`/`finish_isolation`/`discard_isolation` in `crates/driver/src/subagent/spawn.rs` call `CreateWorktree`/`MergeWorktree`. `run_child` creates an isolated root for every composed child unconditionally (comment at `spawn.rs:874`). The result is an `artifact://sha256/...` patch or a retained branch, applied only when `isolation.apply` allows.
- Environment side: `crates/envd/src/workspace/operations.rs::create_worktree` snapshots the live workspace, then clones each manifest entry with `clone_file_cow`: `clonefile` (macOS) or `FICLONE` (Linux), falling back on `ENOTSUP`/`EXDEV` (macOS) or `EOPNOTSUPP`/`EXDEV`/`ENOTTY`/`EINVAL` (Linux) to `hardlink_copy_fallback`, which probes a hard link and immediately replaces it with a copy, or plain `fs::copy`; other platforms always take the copy path. It refuses an oversize untracked tree (`IsolationBaselineTooLargeError`). Proof: `crates/e2e/tests/p9_isolation.rs`.
- Gap, ignored files: the snapshot walk in `snapshot_at` runs with `.gitignore(true)` and `.skip_git(true)` (`operations.rs`, around line 779), so gitignored files are not in the manifest and are not copied. Untracked non-ignored files and hidden files are included. This replaces the earlier note that the question was unverified.
- Gap, inert convar: `TaskIsolationMode` declares `none`, `auto`, `apfs`, `btrfs`, `zfs`, `reflink`, `overlayfs`, `projfs`, `block-clone` and `rcopy`. `CreateWorktree` carries no backend field (`crates/proto/proto/omp/env/v1/env.proto`), and the mode is read in one place, `subagent_spec` in `spawn.rs`, where `none` only sets the hook payload's `worktree` flag to false. Isolation still happens. The convar therefore does not select a backend and `none` does not disable isolation. Follow-up code change: remove the unimplemented variants and their `ui.option.*` metadata, and reword the `task.isolation.mode: none` advice in `IsolationBaselineTooLargeError`.

## References

- The Harness Playbook, "The runtime" — "Subagents cross the same boundary"
- `pi-iso` (prior art: CoW workspace views for pi subagents)
- 0006 (host/sandbox rule), 0010 (subagents as jobs), 0001 (multiplexed-workspace row)
- `crates/driver/src/subagent/settings.rs`, `crates/envd/src/workspace/operations.rs`,
  `crates/envd/src/lib.rs` (`isolated`), `crates/driver/src/subagent/spawn.rs`
  (child-kernel spawn; the retained-run-state file `crates/agent/src/subagent.rs`
  was removed by `d98ed242f5`), `crates/e2e/tests/p9_isolation.rs`
