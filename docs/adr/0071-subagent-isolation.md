# 0071. New `/wt` worktrees may carry allowlisted ignored cache directories by APFS clone; subagent isolation keeps its per-file view; ovnfs is not adopted

Status: Proposed 2026-11-14; EGM round 2 resolved, round 3 pending
Date: 2026-11-13 (revised 2026-11-14 after EGM rounds 1 and 2)
Area: runtime

Research: [docs/research/subagent-isolation.md](../research/subagent-isolation.md), Gap 1 of the upstream gap closure. Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations: `upstream:` is the OMPv1 pin; `ovnfs@c43f05a:` is `stencil-hq/ovnfs` at `c43f05a517`, re-read from a fresh clone on 2026-11-14; bare paths are omp2. Everything below comes from reading, except one `git ls-files` run noted in D2. Nothing was built or timed. Claims that rest on inference are marked [INFERENCE].

Terms. The gap closure is a mission to close functional gaps between this repo and upstream OMPv1. Its brief (`docs/research/upstream-gap-closure-brief.md`, not yet on `omp2`) sorts each gap into one class: **a** already covered by an omp2 equivalent; **b** not needed (with what a user does instead; for ovnfs the brief adds "if it only helps on a filesystem this project's users do not have, classify it b"); **c** port upstream's behaviour in omp2's shape; **d** propose a design better suited to omp2. An **EGM round** (Elephant-Goldfish Model) is one review by four fresh-context reviewer agents and a synthesizer that see only this file and what it cites; the author verifies every finding against source before accepting or rejecting it. A **convar** is a typed setting declared in code (ADR 0012); `sv_*` names host or environment policy; `flags: archive` saves a non-default value to the user's config (`crates/con/src/spec.rs:24-25`); `PROJECT` would let a repository's own cfg set it (`crates/con/src/spec.rs:34-36`); `legacy.path` names the OMPv1 setting the importer maps onto it. **W7** and **P8** are the non-gating performance workload and recorder; **P9** is the gated end-to-end proof of isolated subagent worktrees (`crates/e2e/tests/p9_isolation.rs`).

## Context

### The omp2 problem

`/wt [branch]` is a chat command that forks the current checkout into a new git worktree on a new branch and moves the session there. It runs `git stash create`, `git worktree add -b <branch> <path> HEAD`, replays the stash, then copies untracked files that git does not ignore (`crates/app/src/chat_services/workspace.rs:26-92`; message `crates/chat/src/commands/run.rs:307-326`). Ignored dependency and build directories, such as `node_modules/` and `target/`, are not carried, so the first install or build in the new worktree starts from nothing. No omp2 user report about this is recorded, and no timing exists for `/wt` (`crates/e2e/src/baseline/workloads.rs:795-830` measures subagent slots, not `/wt`). Upstream OMPv1 carries those directories (below); this record decides whether and how omp2 should.

### What OMPv1 does

- `pi-iso` is an isolation layer with eight backends behind one trait: APFS `clonefile`, btrfs snapshot, ZFS clone, Linux `FICLONE` tree, overlayfs with a `fuse-overlayfs` fallback, Windows block clone, ProjFS and a recursive copy (`upstream:crates/pi-iso/src/lib.rs:8-13`, `:51-71`, `:136-148`, `:267-299`). On APFS one `clonefile(2)` call clones a whole directory tree, sharing blocks until either side writes (`upstream:crates/pi-iso/src/apfs.rs:1-8`).
- Isolated subagents get a copy-on-write view through it, chosen by `isolation.backend` (`upstream:packages/coding-agent/src/task/settings.ts:41-44`, `upstream:packages/coding-agent/src/task/worktree.ts:629-674`).
- New worktrees (`/wt`, `github pr_checkout`, `git worktree add` in bash) are copy-on-write clones of the whole source checkout "so ignored build artifacts (node_modules, target) carry over", on by default (`upstream:packages/coding-agent/src/task/settings.ts:83-94`, `upstream:crates/pi-shell/src/git/mod.rs:17-22`). The clone copies every file except `.git`, then reconciles the copied index and working tree with the target commit, and prunes directories holding `pyvenv.cfg` or `CMakeCache.txt`, which record absolute paths and break when moved (`upstream:crates/pi-vcs/src/git/mutate.rs:620-860`, `:1842-1872`).
- ovnfs is not a crate. It is the external Go daemon `stencil-hq/ovnfs` (`ovnfs@c43f05a:go.mod:1-3`). With its binary on PATH and `isolation.backend` on `auto`, isolated subagents run in ovnfs checkouts (`upstream:packages/coding-agent/src/task/structured-subagent.ts:907-911`, `upstream:packages/coding-agent/src/task/ovnfs.ts:1-3`, `:260-264`), through four TypeScript files (1,014 lines).
- `pi-vfs` is not ovnfs and not isolation: it is the embedded shell's injectable filesystem, with host paths plus `scheme://` providers, and it never mentions ovnfs (`upstream:crates/pi-vfs/src/lib.rs:1-17`).

### What omp2 has

- Subagent isolation (ADR 0007; "Partially implemented", `docs/adr/STATUS.md:19`). Every writing child gets a manifest-driven per-file view: `clonefile` on macOS, `FICLONE` on Linux, a copy fallback otherwise (`crates/envd/src/workspace/operations.rs:673-746`, `:2041-2128`). The walk keeps files git does not ignore (`:1036-1038`, `:1053`). ADR 0007's Decision calls carrying ignored paths "an open gap" (`docs/adr/0007-subagent-filesystem-isolation.md:29-30`); its owner decision of 2026-11-01 made it a rule: "no gitignored path is safe to copy" (`:89`, `:93`).
- The clone FFI and its "cannot clone" errno set are private functions in `operations.rs` (`try_clone_file_cow`, `cow_is_unsupported`, `crates/envd/src/workspace/operations.rs:2051-2108`). `omp-sandbox` has a second copy and cannot share it, because it does not depend on `omp-envd` (`crates/sandbox/src/runtime/macos.rs:178-196`, `crates/sandbox/Cargo.toml:15-21`). `omp-app` already depends on `omp-envd` (`crates/app/Cargo.toml:87`) and reads its convars (`crates/app/src/chat_services/workspace.rs:111-118`).
- Forge checkout fetches a change request's head from its `clone_url` and checks it out under `<envd state>/forge-worktrees/cr-<n>` (`crates/envd/src/forge_worktree.rs:21-35`, `:248-249`, `:304-306`). That code comes from the forge, possibly a fork.
- Sandbox read denials: `sv_sandbox_read_deny` holds absolute paths the kernel sandbox makes unreadable (`crates/envd/src/exec_settings/sandbox.rs:297-302`, applied at `crates/envd/src/exec_sandbox.rs:1337-1342`). The static is not re-exported from `omp_envd::exec_settings` (`crates/envd/src/exec_settings/mod.rs:3-14`).
- `omp-exec` already holds omp2's credential-name list for remote uploads (`crates/exec/src/deny.rs:15-38`).
- CI runs `ubuntu-24.04` and macOS, with no Windows job; no Linux job has a reflink-capable filesystem [INFERENCE: runner filesystems not observed] (`.github/workflows/ci.yml:128`, `:457`, `:654`).
- ovnfs: no code, extension, script or CI file mentions it; ADR 0007's amendment records it as a proposal (`docs/adr/0007-subagent-filesystem-isolation.md:82-84`).

### Rules that bind

- Additive only: no existing tool, skill, convar, hook, URL scheme or CLI command changes behaviour, name, schema or revision.
- ADR 0007 decides subagent views (`docs/adr/0007-subagent-filesystem-isolation.md:1`, `:26`); whether its ignored-file rule also covers user worktrees is O1, still open.
- ADR 0035: `thiserror` errors with typed sources; no formatted error strings in libraries (`docs/adr/0035-rust-for-the-engine.md:55-56`).
- ADR 0049 D1: vendor tools are extensions (`docs/adr/0049-tiered-integration-architecture.md:41-54`); ADR 0036: extensions are Python (`docs/adr/0036-python-for-extensions.md:40-41`); ADR 0024: nothing here touches the model-facing roster (`docs/adr/0024-small-permanent-roster.md:33-44`).
- PHILOSOPHY: refuse splits that carry no weight; "a surface nobody has consumed is a guess" (`PHILOSOPHY.md:120-124`, `:140-141`). AGENTS: pre-release code is moved, not copied (`AGENTS.md:133-134`).

## Decision

Part (a), copy-on-write backends, is class **d**: omp2 adopts upstream's user-visible benefit (new worktrees keep caches) in a narrower shape, not upstream's clone-first worktree code. Part (b), ovnfs, is class **b**.

**D1. Subagent isolation keeps its per-file view, with no new backend and no selector.** A whole-tree clone, snapshot or overlay exposes ignored files, which ADR 0007 forbids for a subagent view (`upstream:crates/pi-iso/src/apfs.rs:3-8`, `upstream:crates/pi-iso/src/btrfs.rs:1-6`, `upstream:crates/pi-iso/src/overlayfs.rs:1-6`; `docs/adr/0007-subagent-filesystem-isolation.md:93`). btrfs, ZFS, overlayfs, ProjFS, block clone and the recursive copy are not ported; neither is `isolation.backend`, which omp2 deleted on purpose (`docs/adr/0007-subagent-filesystem-isolation.md:94`).

**D2. With `sv_worktree_clone` on, `/wt` adds one step after today's flow: it carries allowlisted ignored cache directories by clone.** The stash, `git worktree add` and untracked copy run exactly as today, so tracked, staged, unstaged and untracked state have one implementation (`crates/app/src/chat_services/workspace.rs:35-92`). Then:

1. **List.** `git ls-files --others --ignored --exclude-standard --directory -z` in the source root, through the same `git` helper. Run on 2026-11-14 with git 2.43.0 against a scratch repository, it listed `node_modules/`, `packages/p/node_modules/` and `target/` as single directory entries, and nothing from inside a populated submodule.
2. **Select.** An entry is a candidate when it ends in `/`, and either its final component is in `CACHE_DIR_NAMES` (`node_modules`, `.next`, `.nuxt`, `.turbo`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`) or it directly holds a `CACHEDIR.TAG` file (the Cache Directory Tagging convention, ovnfs's `cachedir` predicate, `ovnfs@c43f05a:docs/configuration.md:102`; Cargo is expected to tag `target/` [INFERENCE: `~/.cargo/registry/CACHEDIR.TAG` exists on this machine, a `target/` was not observed]). The names are ovnfs's `cache` directory list (`ovnfs@c43f05a:docs/configuration.md:77`) minus `.venv`, `.tox`, `__pycache__` and `.gradle`: virtual environments hold `pyvenv.cfg`, `__pycache__` is revalidated against source mtimes, and `.gradle` was not examined. A candidate is dropped (`SourceBound`) when it directly holds `pyvenv.cfg` or `CMakeCache.txt`, which record absolute paths. It is dropped (`ReadDenied`) when its canonical path contains, or lies below, the canonical path of an existing `sv_sandbox_read_deny` entry; both sides go through `fs::canonicalize` (so `/var` and `/private/var` compare equal on macOS), and entries that do not exist are ignored. Names compare case-sensitively on the final component.
3. **Clone.** Each candidate is cloned to the same relative path in the new worktree by `omp_envd::cow::clone_dir` (D3). The first error that `is_unsupported` accepts (another volume, a non-APFS volume) ends the step, and the whole carry is reported as `Unsupported`. Any other error skips that candidate (`CloneFailed { kind: io::ErrorKind }`), removes only its partial destination, and continues.
4. **Report.** `WorktreeInfo` (`crates/chat/src/overlays/services.rs:949-956`) gains `carry: Option<CacheCarry>`, `None` when the convar is off. `CacheCarry` is `Unsupported` or `Carried { carried: u32, skipped: Vec<SkippedCache> }`, where `SkippedCache { path, reason }` holds a path relative to the source root and a `CarrySkipReason` (`SourceBound`, `ReadDenied`, `CloneFailed { kind }`). These are plain data types in `omp-chat`, rendered once by the `/wt` notice (`crates/chat/src/commands/run.rs:317-322`); with `carry: None` the notice text is unchanged.

The allowlist fails closed: an ignored file is carried only inside a directory a build or package tool regenerates. What remains exposed is stated, not hidden: a secret a user places inside an allowlisted cache directory is carried, and tracked or untracked-unignored files are carried as today, whatever `sv_sandbox_read_deny` says (O13). Glob denials are not applied (O12).

Contract: like the rest of `/wt` today, the step runs synchronously in the chat service, is not cancellable and has no size budget; `/wt` already refuses to run during a turn (`crates/chat/src/commands/run.rs:308-310`). A cache directory that another process is writing may be cloned mid-write; its tool rebuilds it. Its cost is measured before S2 merges (S2 acceptance).

**D3. The clone FFI moves into a public `omp_envd::cow` module; nothing is copied.** `crates/envd/src/cow.rs` (`pub mod cow` in `crates/envd/src/lib.rs`) receives `try_clone_file_cow` and `cow_is_unsupported` from `operations.rs`, unchanged in behaviour, as `cow::clone_file` and `cow::is_unsupported`; `operations.rs` calls them, and its copy fallback stays where it is. It adds `cow::clone_dir(source, destination) -> io::Result<()>`: on macOS, `clonefile(source, destination, CLONE_NOFOLLOW)` after creating the destination's parent; elsewhere, `ErrorKind::Unsupported` without a syscall. The policy (what to carry) stays in `/wt` beside today's flow in `crates/app`; the FFI and the deny list stay in `omp-envd`, their owner. No new crate is created: the module has one new caller, `/wt`, besides envd itself.

**D4. Forge checkout and Linux do not carry.**
- Forge checkout is unchanged. Carrying the user's ignored files into a checkout of change-request code would let that code's scripts read them (Context). Upstream does carry them (`upstream:packages/coding-agent/src/task/settings.ts:91-92`); omp2 does not without the owner (O11).
- Linux gets no tree clone: `clone_dir` returns `Unsupported` there and `/wt` says so. A `FICLONE` tree walk is not ported, because no CI job could prove its success path and Linux desktops are not a primary platform. Trigger: a Linux runner with a reflink filesystem, or a user report.

**D5. The convar.** Beside `SV_WORKTREE_BASE` (`crates/envd/src/host_settings.rs:138-148`):
`sv_worktree_clone: bool`, default `false`, `flags: archive` and not `PROJECT`, meta `ui.tab: "tasks"`, `ui.group: "Isolation"`, `ui.label: "Carry Caches into New Worktrees"`, `legacy.path: "worktree.clone"`. Help text: "Carry ignored dependency and build-cache directories into new `/wt` worktrees by APFS clone. Does not affect forge checkout or subagents." The v1 key also governed PR checkout and bash `git worktree add` upstream; the help text says what it governs here. `SV_SANDBOX_READ_DENY` gets an additive `pub use` in `crates/envd/src/exec_settings/mod.rs`.

**D6. ovnfs is not adopted.** ovnfs keeps its own copy-on-write layer behind an NFSv3 (macOS) or FUSE (Linux) mount (`ovnfs@c43f05a:README.md:12-14`, `:96-97`) and uses host block cloning only to capture files over 1 MiB (`ovnfs@c43f05a:docs/internals.md:143-146`), so it is not tied to a filesystem the users lack and the brief's filesystem test does not decide it. It is class b on two grounds:
- Its defaults break ADR 0007's rule. Built-in rules make credential names `local` and ignored paths `cache` or `local` (`ovnfs@c43f05a:docs/configuration.md:75-81`, `:103`), and both show "the folder, live until the checkout writes the path" (`:47-52`), so a subagent in a checkout reads every ignored file and secret.
- It needs a second daemon and a mount, with documented edge cases: an undocumented `mount_nfs` socket form on macOS, `fusermount3` and mount-location limits on Linux, forced stops that fail in some containers, client-local NFSv3 locks and up to 60 s of stale cache (`ovnfs@c43f05a:docs/platforms.md:15-20`, `:52-54`; `ovnfs@c43f05a:docs/configuration.md:10-11`; `ovnfs@c43f05a:docs/limitations.md:28-30`, `:40-43`). It is not a sandbox (`ovnfs@c43f05a:docs/limitations.md:3`), and core has no seam for it (`crates/proto/proto/omp/env/v1/env.proto:1154-1160`).

What the user does instead: install `ovnfs`, copy its agent skill into `.omp/skills` or `<config root>/agent/skills` (`ovnfs@c43f05a:README.md:114`; `crates/driver/src/discovery/skills.rs:1-12`), and run `ovnfs new`, `diff` and `apply` through the bash tool. Re-entry triggers: a recorded subagent creation or merge time over a budget the owner sets; an owner decision that subagents may see ignored caches read-only; cross-machine isolation on the roadmap. If re-opened, the shape is a Python extension over a neutral isolation-provider seam with `hidden` rules for credentials and ignored paths, after a macOS spike (O9).

**D7. Out of this record.** Clone-first worktrees (upstream's `worktree_add` clone path); forge checkout carry (O11); bash `git worktree add` through clones (`upstream:crates/pi-shell/src/git/mod.rs:3-22`); moving the sandbox copy; a standalone clone crate (O5); Windows (O7); `pi-vfs`, declined by Gap 6 (`docs/adr/0054-shell-builtins.md:19`, `:51`).

## Rejected options

| Option | Why rejected |
|---|---|
| Do nothing | The cost is unmeasured; S2's measurement gate decides it. If carry is not faster, S2 is withdrawn and part (a) becomes class b. |
| Clone-first worktrees: port upstream's `worktree_add` clone path into `omp-vcs` (round-1 design) | `/wt` would get a second implementation of tracked and staged carry; the API needs a new-branch start point it lacked (round 2, B1); the port needs helpers omp2 lacks and a reconciliation of the copied index (`upstream:crates/pi-vcs/src/git/mutate.rs:759-860`); submodule `.git` files and index extensions are copied byte for byte [INFERENCE]. |
| Carry every ignored path, minus a credential-name denylist (round-1 design) | Fails open for any secret the list misses, and would be a second credential list beside `crates/exec/src/deny.rs:15-38`. |
| A Linux `FICLONE` tree walk | No CI job can prove it; D4 trigger. |
| A new leaf crate for the clone primitives | One new caller (D3); `PHILOSOPHY.md:120-124`. |
| Port all of `pi-iso` | 5,094 lines, trait objects, four untested backends, and ADR 0007 conflicts (`upstream:crates/pi-iso/Cargo.toml:13-26`; D1). |
| Carry into forge checkout | D4. |
| ovnfs as a Python extension over a new isolation seam | About 1,000 lines of Python and a macOS spike first (estimate), for a gain nobody has measured (D6). |
| An `omp-*` virtual-filesystem crate for ovnfs | The brief's shape if adopted, but it reimplements a daemon or embeds Go (a second runtime). |

## Slices

Each slice is one PR; tests are written first and not edited to pass. No slice adds a journal kind, so no golden-journal fixture is needed. No new e2e proof is added: `/wt` has no joined-system seam beyond `create_worktree_at`, which `crates/app` tests drive with real git repositories; P9 guards that subagent isolation is unchanged.

**S1. `omp_envd::cow`: move the clone FFI and add `clone_dir`.** Files: `crates/envd/src/cow.rs` (new), `crates/envd/src/lib.rs`, `crates/envd/src/workspace/operations.rs`. No behaviour change for subagent isolation.
- Tests (unit, `crates/envd/src/cow.rs`): `is_unsupported_matches_the_errno_table_per_os` (table-driven with `io::Error::from_raw_os_error`: macOS `ENOTSUP`, `EXDEV`; Linux `EOPNOTSUPP`, `EXDEV`, `ENOTTY`, `EINVAL`; false for `ENOENT`, `EACCES`, `ENOSPC`; runs on both OSes); `clone_dir_clones_a_nested_tree_with_a_symlink` (macOS; prints SKIP when a probe `clonefile` in the temp directory returns an unsupported error); `clone_dir_refuses_an_existing_destination` (macOS; the error is not unsupported, and the existing destination is untouched); `clone_dir_is_unsupported_off_macos` (non-macOS, unconditional).
- Acceptance: the existing isolation tests pass unchanged (`crates/app/tests/it/envd_workspace.rs:215`, `crates/e2e/tests/p9_isolation.rs:261`); `just test-pkg omp-envd` and `just e2e-p9` pass on macOS and Linux; both lint jobs are clean; no lintx baseline rises.
- Gate: none needed: no behaviour changes, and `clone_dir` is public, so it is not dead code before S2.

**S2. `sv_worktree_clone` and the `/wt` carry step.** Depends on S1. Files: `crates/envd/src/host_settings.rs` (D5), `crates/envd/src/exec_settings/mod.rs` (re-export), `crates/app/src/chat_services/workspace.rs` (D2 steps; `create_worktree_at` gains `clone: bool` and `read_deny: &[Str]`), `crates/chat/src/overlays/services.rs` (types), `crates/chat/src/commands/run.rs` (notice), `crates/chat/tests/pi_commands.rs` (test double sets `carry: None`), `crates/driver/src/v1_import/settings_tests.rs` (a `worktree.clone: true` line in `V1_CONFIG_YML` and its case).
- Tests: `worktree_carries_uncommitted_changes` passes unchanged (`crates/app/src/chat_services/workspace.rs:159-183`), as does `checkout_creates_worktree_under_forge_root` (`crates/envd/src/forge_worktree.rs:362-364`). New, in `workspace.rs`: `worktree_clone_off_reports_no_carry`; `worktree_clone_on_carries_node_modules_and_a_tagged_target` (macOS; SKIP when the probe fails); `worktree_clone_on_skips_a_pyvenv_and_a_read_denied_cache` (macOS); `worktree_clone_on_never_carries_an_ignored_env_file` (all OSes: the file is not a candidate); `worktree_clone_on_without_clone_support_reports_unsupported` (Linux); `carry_selection_ignores_unlisted_and_submodule_directories` (all OSes, real git). In `omp-chat`: `worktree_notice_is_unchanged_without_carry` and `worktree_notice_names_carried_and_skipped_caches`. In `omp-driver`: `worktree_clone_imports_as_sv_worktree_clone`.
- Acceptance: with the convar off, `/wt`, its notice and forge checkout are unchanged. With it on, on APFS, the new worktree has the source's ignored `node_modules/` (and a tagged `target/`), no `.venv` or other unlisted ignored directory, no ignored `.env`, and no cache that holds or lies under a read-denied path, and the notice gives the count and the skips. On Linux it succeeds as today and says caches were not carried. `just test-pkg omp-app`, `omp-chat` and `omp-driver` pass.
- Measurement gate: the PR records, on an APFS Mac, `/wt` wall time and the first `cargo build` in the new worktree of this repository (with a built `target/`), and `/wt` plus the first `bun install` in a JavaScript repository with `node_modules/`, each with the convar off and on. If neither first build or install is faster with it on, S2 is not merged and this record is amended to class b.
- Gate: `sv_worktree_clone`, default off.

## Upstream origin and license

No upstream code is drawn on. The behaviour (new worktrees keep ignored caches) comes from `upstream:packages/coding-agent/src/task/settings.ts:83-94`; the clone FFI is omp2's own, moved within `omp-envd`; the cache directory names and the `CACHEDIR.TAG` rule are conventions listed in ovnfs's documentation (`ovnfs@c43f05a:docs/configuration.md:77`, `:102`; MIT, `ovnfs@c43f05a:LICENSE:1-3`), not code. No NOTICE or `THIRD-PARTY-NOTICES.txt` entry is needed. ovnfs, if a user installs it, is an external binary.

## Consequences

- macOS users who turn the convar on get `/wt` worktrees with their dependency and build caches, at the cost of the measured clone time. With it off, the default, nothing changes.
- Forge checkout, subagents and Linux are unchanged. Subagent isolation keeps its snapshot-hashing cost; this record promises no speedup there.
- The clone FFI has one owner in `omp-envd` plus the sandbox's separate copy, as today; no third copy.
- Only the macOS job proves a successful carry; Linux CI proves the selection, the unsupported report and the off path.
- ovnfs stays opt-in through its CLI and skill; nothing in core mentions it.
- On acceptance, ADR 0007's "future targets" sentence (`docs/adr/0007-subagent-filesystem-isolation.md:33-36`) and its OVNFS proposal (`:82-84`) are marked superseded by D1 and D6. This record does not edit ADR 0007.

## Open questions for the owner

- **O1. Open.** Does ADR 0007's "no gitignored path is safe to copy" (`docs/adr/0007-subagent-filesystem-isolation.md:93`) cover user-initiated worktrees? Default taken: no, because ADR 0007 decides subagent views (`:1`, `:26`). S2 relies on it and ships off. If the owner says yes, S2 is withdrawn and part (a) becomes class b; S1 is harmless either way.
- **O2.** Default of `sv_worktree_clone`. Upstream defaults it on (`upstream:packages/coding-agent/src/task/settings.ts:86`), so a v1 user who never set it loses carried caches after import, silently. Recommendation: keep it off; flipping it needs its own record after S2's numbers and a release.
- **O3. Resolved in round 2.** One `/wt` flow: the carry step is added after the existing flow (D2).
- **O4.** The isolated view omits symlinks and empty directories (`crates/envd/src/workspace/operations.rs:709-727`, `:1053`), read, not exercised. Recommendation: file it against ADR 0007 with a failing test first; a fix changes the manifest (`MANIFEST_MAGIC`, `:52`) and needs the owner.
- **O5.** `cp` and reflink. Upstream `cp` clones through `pi_iso::cow` (`upstream:crates/pi-builtins/src/cp.rs:7`); ADR 0054's `cp` slice is silent on it (`docs/adr/0054-shell-builtins.md:70`). `omp-shell-builtins` is a dependency of `omp-envd` (`crates/envd/Cargo.toml:69-70`), so it cannot call `omp_envd::cow`. Recommendation: if `cp` keeps reflink, that slice moves `cow.rs` into a leaf crate (`libc` only) that both depend on.
- **O6.** Who owns `scheme://` paths inside shell commands (`upstream:packages/coding-agent/src/internal-urls/url-filesystem.ts:1-14`)? ADR 0054 declined `pi-vfs` (`docs/adr/0054-shell-builtins.md:51`) without naming an owner. Not this record's.
- **O7.** Windows: deferred until a Windows CI job exists.
- **O8. Resolved in round 2.** Measurement is S2's merge gate.
- **O9.** ovnfs: no spike until a D6 trigger fires. A spike checks a Seatbelt-sandboxed child on the NFS mount, `hidden` rules via `ovnfs explain`, and admin protocol drift (`upstream:packages/coding-agent/src/ovnfs/client.ts:21`).
- **O10. Resolved in round 2.** No crate is created (D3).
- **O11.** Carry into forge checkout. Recommendation: not until a rule says which of the user's ignored files may sit beside change-request code.
- **O12.** `sv_sandbox_read_deny_globs` apply only "when supported by the selected sandbox backend" (`crates/envd/src/exec_settings/sandbox.rs:314-319`). Recommendation: absolute paths only in S2.
- **O13.** Existing behaviour, not changed here: the off path copies every untracked unignored file into the new worktree without consulting `sv_sandbox_read_deny` (`crates/app/src/chat_services/workspace.rs:80-90`). Recommendation: a separate record decides whether `/wt` should honour read denials at all.
- **O14.** A user-configurable carry list (a list convar defaulting to `CACHE_DIR_NAMES`). Recommendation: not before S2's numbers.

## What the review changed

### Round 1 (EGM, before 2026-11-14)

Four reviewers and a synthesizer reviewed the 2026-11-13 draft (verdict "changes required"). Findings, each confirmed against source on 2026-11-14:
- **One in-scope consumer for a new crate.** Confirmed: ADR 0054's `cp` slice has no reflink (`docs/adr/0054-shell-builtins.md:70`) and the other consumers were out of scope. Changed then to a private `omp-vcs` module; round 2 replaced that with D3.
- **`WorktreeClone::Prefer` has no caller.** Confirmed: its only upstream caller is the napi shim (`upstream:crates/pi-natives/src/vcs.rs:886-892`). Dropped; round 2 removed the enum with the clone-first design.
- **Ignored-file secrets in a forge checkout.** Confirmed: forge checkout checks out fetched change-request code (`crates/envd/src/forge_worktree.rs:248-249`, `:304-306`). Forge checkout no longer carries (D4, O11).
- **O1 answered "No" from ADR 0007.** Confirmed as a reading of ADR 0007's scope (`docs/adr/0007-subagent-filesystem-isolation.md:1`, `:26`, `:93`). Round 2 reopened O1, because the owner has not answered.
- **ovnfs classification.** Kept as class b, re-verified against upstream and a fresh clone; the rule table was read (`ovnfs@c43f05a:docs/configuration.md:75-81`, `:103`).

### Round 2 (EGM, 2026-11-14)

Four fresh reviewers and a synthesizer: 1 blocker, 19 majors, 17 minors. Every finding was checked against source. The blocker and most majors pointed at the clone-first design, so part (a) was redesigned as a carry step (D2, D3) and reclassified from c to d.

Blocker:
- **B1. `worktree_add_with` cannot express "new branch at HEAD".** Confirmed: `worktree_add` resolves `ref_name` as a commit and names the branch from the same string (`crates/vcs/src/git/mutate.rs:447-451`), while `/wt` passes a new branch and `HEAD` (`crates/app/src/chat_services/workspace.rs:66`). Resolved by removal: `/wt` keeps its `git worktree add -b` flow and no `omp-vcs` API is added (D2).

Majors:
- **M1. Withhold mechanism unspecified.** Confirmed: upstream's prune is a marker-file walk with no ignore evaluation (`upstream:crates/pi-vcs/src/git/mutate.rs:1842-1872`). Replaced by D2's list-select-clone steps, with the listing behaviour observed by a real `git ls-files` run.
- **M2. Credential denylist fails open.** Confirmed by construction. Replaced by an allowlist of cache directories; residual exposure is stated (D2).
- **M3. Duplicates `DEFAULT_UPLOAD_DENY`.** Confirmed: `crates/exec/src/deny.rs:15-38` differs from the round-1 table. No credential table remains.
- **M4. Benefit unmeasured.** Confirmed: no `/wt` timing exists (`crates/e2e/src/baseline/workloads.rs:795-830`). Measurement is S2's merge gate; "Do nothing" now defers to it.
- **M5. O1 marked resolved without the owner.** Confirmed (`docs/adr/0007-subagent-filesystem-isolation.md:93` is an owner decision). O1 reopened; S2 is withdrawn on a "yes".
- **M6. Read-deny matching underspecified.** Confirmed: the sandbox keeps absolute and normalized forms (`crates/envd/src/exec_sandbox.rs:1337-1342`). D2 step 2 specifies canonicalization, both directions, non-existent entries and case. Scoped read mode is not considered; glob denials are O12.
- **M7. No typed variant for non-`CowError` failures.** Confirmed: `omp_vcs::Error::Backend` carries a `message: String` (`crates/vcs/src/error.rs:126-133`). Moot: no `omp-vcs` error is involved; skips carry `io::ErrorKind` (D2 step 4).
- **M8. Chat plumbing missing.** Confirmed: the notice is hardcoded (`crates/chat/src/commands/run.rs:317-322`) and `WorktreeInfo` has only `path` and `branch` (`crates/chat/src/overlays/services.rs:949-956`; double `crates/chat/tests/pi_commands.rs:176-182`). Added to D2 step 4 and S2's files and tests.
- **M9. Port understated; `mutate.rs` has diverged.** Confirmed: omp2 has no `status_with_fresh_index`, `remove_existing`, `copy_symlink` or `prune_empty_parents` (searched `crates/vcs/src`), and `load_index_or_empty` differs (`crates/vcs/src/git/mutate.rs:809`). Moot: nothing from `mutate.rs` is ported.
- **M10. Synchronous, uncancellable bulk operation.** Confirmed: `create_worktree_at` is synchronous (`crates/app/src/chat_services/workspace.rs:29-35`). D2 states the contract and S2 measures it.
- **M11. Submodules and index extensions.** Confirmed: upstream's cloned path refuses only sparse checkout and split index (`upstream:crates/pi-vcs/src/git/mutate.rs:768-778`). Moot: no index is copied, and the listing does not descend into submodules (observed; S2 test).
- **M12. Simplest design rejected on weak grounds.** Confirmed: the rejection rested on an [INFERENCE] and on there being nothing upstream to port. Adopted as D2, without the mtime inference; S2's first-build measurement covers it.
- **M13. Third copy of the clone FFI.** Confirmed: envd's `try_clone_file_cow`/`cow_is_unsupported` match upstream's errno sets (`crates/envd/src/workspace/operations.rs:2051-2108`). D3 moves them; nothing is copied.
- **M14. Linux `FICLONE` tree unprovable.** Confirmed: no reflink CI filesystem [INFERENCE] and no ported Linux tree test upstream. Dropped (D4).
- **M15. `CowError` exposed despite "one public type".** Confirmed against the round-1 D2/D3 text. Moot: the public surface is `omp_envd::cow`'s three functions on `io::Result`.
- **M16. Stated invariant stronger than the rule.** Confirmed. D2 now states exactly what is withheld, and O13 records the existing untracked-copy exposure.
- **M17. Classes, brief and EGM undefined.** Confirmed: the brief is not in this tree. Added "Terms".
- **M18. `/wt` and `keep_changes` unexplained.** Confirmed. "The omp2 problem" describes `/wt`; `keep_changes` no longer exists here.
- **M19. Why `pyvenv.cfg`/`CMakeCache.txt` are pruned.** Confirmed: they record absolute paths. Stated in Context and D2 step 2.

Minors:
- **m1.** Withhold ordering contradicts upstream's flow (`upstream:crates/pi-vcs/src/git/mutate.rs:759-800`): confirmed; moot.
- **m2.** Convar definition incomplete; the importer test needs a `V1_CONFIG_YML` key (`crates/driver/src/v1_import/settings_tests.rs:13`, `:105`): confirmed; D5 spells out the declaration and help text; S2 adds the key.
- **m3.** Nightly gates for ported FFI: confirmed that `crates/vcs/src/lib.rs` has none; moot (moved code already compiles in `omp-envd`).
- **m4.** `BackendKind` string form: confirmed upstream hand-writes it (`upstream:crates/pi-iso/src/lib.rs:51-130`); moot (no backend enum).
- **m5.** NOTICE route untraced: confirmed no generator in `scripts/` or `justfile`; moot (no upstream code drawn).
- **m6.** Layering: confirmed; D3 states that policy stays with `/wt` in `crates/app` and the FFI and deny list stay in `omp-envd`.
- **m7.** Test mechanisms: confirmed `chmod 000` is unreliable under root (`crates/vcs/src/git/mutate.rs:1615-1617`); S1 and S2 use a probe `clonefile` and an existing destination instead.
- **m8.** Linux special files fail the walk (`upstream:crates/pi-iso/src/linux_reflink.rs:133-139`): confirmed; moot (no Linux walk). macOS directory clone with special files inside is untested [INFERENCE]; any error becomes `CloneFailed`.
- **m9.** Cleanup removes a caller's empty destination (`upstream:crates/pi-vcs/src/git/mutate.rs:1927-1934`): confirmed; moot. D2 removes only a candidate's own partial destination.
- **m10.** ADR 0007 status wording: confirmed (`docs/adr/STATUS.md:19`; `docs/adr/0007-subagent-filesystem-isolation.md:29-30` vs `:89`). Context now says "Partially implemented" and cites both. The STATUS.md row is the orchestrator's to update.
- **m11.** Private helper and test inventories: confirmed; removed with the clone-first port. S1 and S2 keep the test names, which are their acceptance.
- **m12.** Review history and a strawman row: partly rejected. "What the review changed" is required by the brief's method (step 6), so it stays, shortened. The virtual-filesystem row is the brief's own shape for ovnfs if adopted ("If adopted, it is an `omp-*` virtual-filesystem crate"), so it stays, reworded. D6 was cut to two grounds.
- **m13.** No omp2 problem statement: confirmed; added.
- **m14.** 1,550 inert lines: confirmed; S1 is now a move plus one function exercised by existing tests, and S2 is the vertical slice.
- **m15.** `WorktreeClone { Off, Auto }` is a bool: confirmed; moot.
- **m16.** Undefined vocabulary: confirmed; "Terms" glosses it.
- **m17.** `~/.o2/ovnfs.yml` paragraph has no purpose: confirmed; removed.

Rejected in round 2: half of m12 (above). Round 3: pending. Not owner-approved.

## Acceptance tests

1. `is_unsupported` is exactly the per-OS errno set, and the existing isolation tests and P9 pass unchanged after the move (S1).
2. On macOS, `clone_dir` clones a nested tree with a symlink and refuses an existing destination; elsewhere it is `Unsupported` (S1).
3. With `sv_worktree_clone` off, `/wt`, its notice and forge checkout are unchanged (S2).
4. With it on, on APFS, a new `/wt` worktree holds the source's ignored `node_modules/` and a `CACHEDIR.TAG`-tagged `target/`, and no unlisted ignored directory, no ignored `.env`, no cache holding `pyvenv.cfg` or `CMakeCache.txt`, and no cache that holds or lies under a `sv_sandbox_read_deny` path; the notice reports the count and each skip (S2).
5. On Linux with the convar on, `/wt` succeeds as today and reports that caches were not carried (S2).
6. Directories inside submodules and directories whose names are not listed are never candidates (S2).
7. The importer maps v1 `worktree.clone` to `sv_worktree_clone` (S2).
8. S2's PR records the off and on timings; S2 merges only if carry makes a first build or install faster (S2).

## Status in omp

**Status: Not started.** Reusable today: the clone FFI and errno set in `crates/envd/src/workspace/operations.rs:2051-2108`, the `/wt` flow and its test (`crates/app/src/chat_services/workspace.rs:35-92`, `:159-183`), the `sv_worktree_base` convar and importer pattern (`crates/envd/src/host_settings.rs:138-148`). Unverified: nothing was built or timed; `clonefile` on a directory holding sockets or fifos, and Cargo's `CACHEDIR.TAG` in `target/`, were not observed; ovnfs documents were re-read on 2026-11-14 at `c43f05a517`.

## References

[docs/research/subagent-isolation.md](../research/subagent-isolation.md); ADR 0007 (subagent isolation); ADR 0012 (convars); ADR 0024 (roster); ADR 0035 (errors); ADR 0036 (Python extensions); ADR 0049 (tiers, forge checkout); ADR 0054 (shell builtins, `cp`); `PHILOSOPHY.md`; `crates/app/src/chat_services/workspace.rs`; `crates/chat/src/commands/run.rs`; `crates/chat/src/overlays/services.rs`; `crates/envd/src/workspace/operations.rs`; `crates/envd/src/exec_settings/sandbox.rs`; `crates/envd/src/forge_worktree.rs`; `crates/exec/src/deny.rs`; upstream `crates/pi-iso`, `crates/pi-vcs`, `crates/pi-vfs`, `packages/coding-agent/src/task/ovnfs.ts` and `packages/coding-agent/src/task/settings.ts`; `github.com/stencil-hq/ovnfs`.
