# 0054. Port the missing upstream utility builtins onto the host seam, opt-in per name

Status: Proposed 2026-11-13
Date: 2026-11-13
Area: tools

## Context

- OMPv1 ships in-process utilities in the crate `pi-builtins` (`upstream:crates/pi-builtins/Cargo.toml:11`), one module and one cargo feature per utility (`upstream:crates/pi-builtins/Cargo.toml:208` `util.cp`, `:262` `util.rsync`, `:278` `util.tar`). A macro table registers them (`upstream:crates/pi-builtins/src/extras.rs:48` `cp`, `:86` `rsync`, `:102` `tar`, `:110` `uptime`, `:122` `lsof`), enumerated by `upstream:crates/pi-shell/src/extras.rs:69`. Pinned at `c308970263` (v18.20.0).
- omp2's in-process shell (ADR 0028) registers its utilities in `utility_builtins()` (`crates/shell-builtins/src/factory.rs:29`). Nineteen upstream utilities are missing: cp, chmod, tar, rsync, du, df, dd, env, id, sd, shasum, strings, od, nl, fold, rmdir, lsof, uptime, md5. For each, no `crates/shell-builtins/src/<name>.rs` and no registration exists (`docs/research/shell-builtins.md` §2).
- A missing builtin is not a missing capability: the command runs as an external process under the OS sandbox (ADR 0028 §4, lines 44-47). What is lost is the in-process path policy (`Host::open_read` `host.rs:273`, `Host::ensure_writable` `host.rs:300`), cancellation, and behaviour on hosts without the binary (minimal containers, Windows, BSD `md5` versus GNU `md5sum`).
- All registered utilities are installed unconditionally by every caller (`crates/envd/src/exec.rs:834`, `crates/envd/src/shell_child.rs:51`, `crates/app/src/shell_cmd.rs:41`, `crates/shell-builtins/src/bin/omp-sh.rs:83`). The comment at `factory.rs:22-27` says an embedder "may withhold destructive utilities"; none does.
- A builtin shadows the system command of the same name, so enabling one changes what existing commands do (option sets, output, exit statuses, and which policy applies). The change must be additive, so it must be opt-in.
- Research: `docs/research/shell-builtins.md` (per-builtin table, upstream sizes, platform and sandbox concerns, license).

## Decision

**D1. Port 18 of the 19; do not port `rsync`.** Each port copies the upstream module and replaces its filesystem seam (`pi_vfs` and `uucore` calls) with the omp2 host: reads through `Host::open_read`, writes through `Host::ensure_writable` (`PathPolicy::check_write`), spawns through `Host::command` (`host.rs:345`), pid visibility through `may_observe` (`host.rs:322`). Recursive walks use the existing fd-relative `support/safe_traversal.rs` (`DirFd::open:38`, `read_dir:98`, `unlink_at:165`) so a link swapped in mid-walk cannot leave the root (ADR 0028 §4).

**D2. Opt-in per name; nothing changes by default.** A new convar `sv_shell_builtins_extra: Vec<Str>`, default empty, declared beside `SV_SHELL_EMBEDDED_BUILTINS` (`crates/envd/src/exec_settings/shell.rs:95`; the vector form and `validate` hook exist at `crates/envd/src/exec_settings/sandbox.rs:254`), lists the names to register. A new factory function `extra_builtins(enabled)` returns only those. `utility_builtins()` and the registrations of `rm`, `mv`, `ln` and every existing name are untouched; the four call sites each add one `.builtins(extra_builtins(…))` call.

**D3. Heavy dependencies are behind a cargo feature.** `tar` needs `tar`, `bzip2` and `lzma-rust2`, none of which is in the workspace; they sit behind `cap-shell-archive` on `omp-shell-builtins`, forwarded like the existing `cap-*` features (`crates/envd/Cargo.toml:17-22`, `crates/app/Cargo.toml:36-43`). Without the feature, naming `tar` in the convar is a validation error.

**D4. Shared helpers are ported with the first slice that needs them,** into `src/support/`: `bre` (`nl` needs `bre::bre_to_ere`, upstream `nl.rs:217`), `locale`, `block_size`, `quoting_style`, `time_zone`, `atomic_write`, `file_backup`, `progress`, `openfile`. `sed.rs` keeps its private `bre_to_ere` (`sed.rs:1139`); it is not refactored.

**D5. `rsync` is not shadowed (class b).** A system `rsync` runs as a spawned process under the OS sandbox and, in `scoped` network mode, the egress broker (ADR 0028 §4). The upstream implementation is a clean-room protocol 29-33 client, server and daemon (`upstream:crates/pi-builtins/src/rsync.rs:1`), 2144097 B over 67 files, with raw sockets that would bypass the broker. Local copies are served by `cp -a` once slice 7 lands.

**D6. Destructive builtins (cp, dd, tar, chmod, rmdir, sd) follow D2 like the rest.** There is no separate "withheld" mechanism to copy: `rm`, `mv` and `ln` are installed everywhere today. Registration stays opt-in; moving any name to on-by-default is a later amendment, per name, after its write-deny and symlink-race tests have run in a release.

**D7. No policy moves out of the host.** Builtins run on the sandbox side and never call into host authority (ADR 0006). Output is bounded by the transport (ADR 0009, `0009-bound-output-once.md`); builtins only honour cancellation. No new tool, role or roster entry (ADR 0024), no vendor code (ADR 0049), no new language runtime (ADR 0035, ADR 0036).

**D8. No `pi` identity in ported code.** Crate, feature, convar and message names use `omp-*`, `cap-*`, `sv_*`; strings such as upstream's "not a pi extra" are renamed. Errors follow ADR 0035.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Depend on `pi-vfs` and `uucore` and port unchanged | omp2 has neither (`crates/shell-builtins/Cargo.toml`). A second filesystem seam beside the path policy lets a builtin skip `check_write`, and `pi` names enter omp2. |
| Compose the commands from existing builtins | Would not reach GNU/uutils option sets, exit statuses and diagnostics, which is the product. |
| Leave all 19 external | Correct for `rsync` only. Elsewhere it loses path denial, cancellation and portability (Context). |
| Register all 18 by default | Changes the behaviour of commands users already run, and the destructive ones have not yet had a release under the policy tests. Not additive. |
| One cargo feature per utility, like upstream `util.<name>` | A build-time control with no stated user in omp2; the convar gives the same control at run time. Only `tar`'s crates earn a feature (D3). |
| Ship them as an extension | Extensions are Python (ADR 0036) and these are core shell commands with no vendor involved (ADR 0049). |
| Extend `sv_shell_embedded_builtins` | It is a bool, default true (`shell.rs:95`); reusing it to mean "also the new ones" would change behaviour for everyone who has it on. |

## Slices

Each slice is one PR. The gate for slices 2-10 is the convar from slice 1 (default empty), plus `cap-shell-archive` for slice 9. Linux checks: `just remote-test-pkg omp-shell-builtins` (`justfile:129`); the macOS and Windows branches need `just test-pkg omp-shell-builtins` (`justfile:294`) run on those hosts.

| # | Contents | Gate | Acceptance tests |
|---|---|---|---|
| 1 | Convar `sv_shell_builtins_extra`, `extra_builtins(enabled)`, four call sites, registry-parity test | the convar | With the default, the set of registered names equals today's. An unknown name fails validation. |
| 2 | nl, fold, od, strings; `support/{bre,locale}.rs` | convar | Golden tests per option group. One read-deny case per command in `read_policy_tests.rs` (matrix at `:1-5`). `sed` tests unchanged. |
| 3 | shasum, md5 | convar | Digest equals the matching `*sum` builtin on the same input; `shasum -c`; BSD `md5 -q/-r/-s` output; read-deny. |
| 4 | id (unix), env, uptime; `support/{time_zone,quoting_style}.rs` | convar | `env cmd` launches through the spawn wrapper (`set_test_spawn_wrapper`, `host.rs:1641`). uptime with utmp unreadable by policy still prints, with no user-count error. |
| 5 | du, df; `support/block_size.rs` | convar | du refuses or skips a read-denied root. df lists no mount outside the policy's readable view. |
| 6 | rmdir, chmod (unix), sd; `support/atomic_write.rs` | convar | One write-deny test per command (`set_test_path_policy`, `host.rs:1636`). chmod `-R` stays inside the tree through a link. sd `-i` replaces atomically. |
| 7 | cp; `support/{file_backup,progress}.rs` | convar | Write-deny on the destination. A symlink swapped during `-r` stays inside the root. Backup and xattr options pass. |
| 8 | dd; `support/openfile.rs` | convar | `of=` under a write-deny root fails; `if=` goes through `open_read`; cancellation stops the copy. |
| 9 | tar | convar + `cap-shell-archive` | Members `../x`, absolute names and symlink-then-file chains are refused. Create, list and extract round-trip for gz, bz2, xz, zst. `cargo deny` passes with the new crates. |
| 10 | lsof | convar | Pids rejected by `may_observe` are never listed. A Linux `/proc` fixture test. macOS and Windows backends run locally. |

Slices that add `chmod`, `tar` or `sd` also add the matching `crates/shell/src/analysis.rs` arm (it has none today; it has `cp` at :518, `rm`/`rmdir` at :564, `dd` at :588), subject to open question O3.

## Consequences

- No behaviour changes until a user or profile sets the convar. The slice-1 parity test pins that.
- With names enabled, the commands run in-process under the path policy, cancel cleanly and behave the same on every host.
- The port is large: cp 124650 B, dd 83897 B, tar 80327 B, lsof 109059 B (upstream sizes, `docs/research/shell-builtins.md` §1). Slices 7-10 are the risk; slices 2-6 are mechanical.
- Nine helper modules join `support/`; `sed` keeps a duplicate BRE converter until someone chooses to merge them.
- Platform coverage splits: Linux on the remote runner, macOS and Windows locally (ADR 0040).
- `rsync` stays external. Users who want in-process sync get `cp -a`.

## Open questions for the owner

- **O1. Shadow `rsync` at all?** Recommendation: no (D5). Reopen only with an amendment to ADR 0028 on in-process sockets and the egress broker.
- **O2. Register cp, dd, tar unconditionally or withhold them like rm, mv, ln?** The code cannot settle it: nothing withholds today (all four call sites install the full set). Recommendation: opt-in per name through the convar (D2, D6), moved to default-on per name by amendment after one release.
- **O3. `analysis.rs` arms for chmod, tar, sd.** An unconditional arm changes how an existing external `chmod` is analysed and approved. Recommendation: make the arm apply only when the name is in `sv_shell_builtins_extra`. Whether the analyser can see the enabled set is not established by the code read here.
- **O4. `dd of=/dev/…` under a sandbox.** Not verified whether `check_write` already refuses device paths. Recommendation: refuse unless the path policy lists the path, whenever `sv_sandbox_mode` is not `off`.
- **O5. Spelling of the convar value.** Recommendation: names, plus the keyword `all` meaning every name `extra_builtins` offers in that build.

## Acceptance tests

1. With `sv_shell_builtins_extra` empty, the registered name set is exactly today's (slice 1).
2. Naming a name not offered by this build (for example `tar` without `cap-shell-archive`, or `rsync`) is a validation error.
3. Every file-reading new builtin fails on a read-denied file and prints nothing from it (`read_policy_tests.rs`).
4. Every writing new builtin fails on a write-denied destination (`set_test_path_policy`).
5. `env cmd` runs through the installed spawn wrapper.
6. `tar` extraction refuses `..`, absolute and symlink-chain members; `cp -r` and `chmod -R` stay inside their root when a link is swapped mid-walk.
7. `lsof` never lists a pid `may_observe` rejects.
8. No identifier, message or convar in the ported code uses `pi` as a product name (grep check).

## Status in omp

**Status: Not started.** None of the 19 exists (`docs/research/shell-builtins.md` §2). The host seam, the `support/{mounts,human,entries,mode,backup,safe_traversal,checksum,xattr}` modules and the test hooks the slices need already exist; `backup` and `safe_traversal` have no consumer yet.

## Upstream origin and license

Draws from `upstream:crates/pi-builtins/src/{nl,fold,od,strings,shasum,md5,id,env,uptime,du,df,rmdir,chmod,sd,cp,dd,tar,lsof}.rs` (with `dd/` and `lsof/`) and the helpers named in D4. License: MIT (`upstream:crates/pi-builtins/LICENSE-MIT`; `upstream:Cargo.toml:33`). `upstream:crates/pi-builtins/LICENSE` carries the lineage:

- uutils coreutils, MIT, "Copyright (c) uutils developers" (LICENSE lines 8-16): its list names `cp` and `nl`. It does not name od, du, df, dd, env, id, chmod, fold, rmdir, uptime or shasum, although those module headers cite uutils or `uucore`: each slice reads the headers of the files it ports and carries the notices they name.
- sd (https://github.com/chmln/sd), MIT: `sd` (LICENSE lines 149-155).
- libproc-rs (https://github.com/andrewdavidmackenzie/libproc-rs), MIT: macOS `proc_info.h` layouts in `lsof/macos.rs` (LICENSE lines 203-209).
- Original work under the workspace MIT license: lsof, tar, shasum, md5, strings (LICENSE lines 230-236).

omp2 already keeps this lineage in `crates/shell-builtins/NOTICE` and `LICENSE`; each slice appends its lines to `NOTICE`.

## References

ADR 0006 (policy on the host); ADR 0009 (`0009-bound-output-once.md`); ADR 0024 (roster); ADR 0028 (in-process interpreter, §4 enforcement); ADR 0035 (Rust for the engine, error discipline); ADR 0036 (Python extensions); ADR 0040 (remote verification); ADR 0049 (vendor integrations in extensions); `docs/research/shell-builtins.md`; `crates/shell-builtins/src/{factory,host}.rs`; `crates/shell-builtins/src/support/`; `crates/envd/src/exec_settings/shell.rs`; `crates/shell/src/analysis.rs`.
