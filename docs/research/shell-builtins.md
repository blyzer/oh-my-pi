# Gap 6: in-process shell builtins (`shell-builtins`)

Scope: cp, chmod, tar, rsync, du, df, dd, env, id, sd, shasum, strings, od, nl, fold, rmdir, lsof, uptime, md5.
ADR: 0054 (`docs/adr/0054-shell-builtins.md`). Upstream pin: `c308970263` (v18.20.0); omp2 base: `origin/omp2 f4186b783b`.

## 1. Upstream

OMPv1 ships the in-process utilities as the crate `pi-builtins` (`upstream:crates/pi-builtins/Cargo.toml:11`, "In-process command-line utilities and process builtins for the embedded shell"). Each utility is one module with its own cargo feature, `util.<name>` (`upstream:crates/pi-builtins/Cargo.toml:208` for `util.cp`, `:262` `util.rsync`, `:278` `util.tar`), and `utils` turns on the whole set (`Cargo.toml:189-190`). The shell finds them through a macro table in `upstream:crates/pi-builtins/src/extras.rs` (`cp` at line 48, `rsync` 86, `tar` 102, `uptime` 110, `lsof` 122, each under its own `cfg`), which `upstream:crates/pi-shell/src/extras.rs:69` enumerates. A user needs them because the agent's `Bash` tool runs commands in-process (ADR 0028): a builtin answers on every host, including a minimal container or Windows, and runs under the in-process path policy instead of depending on whichever binary the machine has.

Upstream sizes (bytes, lines; module directories counted whole): cp `upstream:crates/pi-builtins/src/cp.rs` 124650 B, 3880 L; chmod 25575 B, 849 L; tar 80327 B, 2493 L; rsync `rsync.rs` plus its `rsync/` tree (67 files in all), 2144097 B, 63321 L; du 52243 B, 1525 L; df 56977 B, 2021 L; dd `dd.rs` plus `dd/{human,tables}.rs`, 83897 B, 2614 L; env 57228 B, 1801 L; id 11837 B, 408 L; sd 18370 B, 569 L; shasum 31471 B, 993 L; strings 36350 B, 1187 L; od 44018 B, 1404 L; nl 25279 B, 835 L; fold 15778 B, 518 L; rmdir 14550 B, 484 L; lsof `lsof.rs` plus `lsof/{linux,macos,windows}.rs`, 109059 B, 3521 L; uptime 26342 B, 774 L; md5 11786 B, 381 L.

## 2. omp2 inventory

- **None of the 19 exists.** No `crates/shell-builtins/src/<name>.rs` for any of them, and no `("<name>", …)` registration under `crates/*/src`. The registry is `crates/shell-builtins/src/factory.rs:29` (`utility_builtins()`, 67 `m.push` registrations); its neighbours are `ln` (:57), `md5sum` (:59), `mv` (:62), `printenv` (:65), `rm` (:68), `whoami` (:90).
- **Everything registered is installed unconditionally.** `utility_builtins()` is added without a filter at `crates/envd/src/exec.rs:834`, `crates/envd/src/shell_child.rs:51`, `crates/app/src/shell_cmd.rs:41` and `crates/shell-builtins/src/bin/omp-sh.rs:83`. The doc comment at `factory.rs:22-27` says an embedder "may withhold destructive utilities such as `rm`, `mv`, and `ln`", but no caller does.
- **Convar and feature homes.** `sv_shell_embedded_builtins: bool` (default true) is declared at `crates/envd/src/exec_settings/shell.rs:95` and read at `:207`. Vector convars exist (`sv_sandbox_writable_roots: Vec<Str>`, `crates/envd/src/exec_settings/sandbox.rs:254`, with a `validate` hook). `cap-*` features live in `crates/envd/Cargo.toml:17-22` and are forwarded by `crates/app/Cargo.toml:36-43`. `crates/shell-builtins/Cargo.toml` has no `[features]` table. No `sv_shell_builtins*` name exists (grep over `crates`).
- **Host seam.** Reads go through `Host::open_read` (`host.rs:273`), writes through `Host::ensure_writable` (`host.rs:300`, calling `PathPolicy::check_write`), spawns through `Host::command` (`host.rs:345`), pid visibility through `may_observe` (`:322`) and `may_signal` (`:330`). Current writers that call `ensure_writable`: `mv.rs:985-986`, `ln.rs`, `mkdir.rs`, `mktemp.rs`. Test hooks: `set_test_path_policy` (`host.rs:1636`), `set_test_spawn_wrapper` (`:1641`), `run_util_with_policy` (`:1695`); the read-deny matrix is `read_policy_tests.rs:1-5`.
- **Support modules that already exist, with their consumers.** `support/mounts.rs` (`read_fs_list:109`, `statfs:577`; used by `find`), `support/human.rs` (`human_readable:38`; `ls`), `support/entries.rs` (`uid2usr:12`, `gid2grp:22`; `ls`, `whoami`), `support/mode.rs` (`parse_chmod:141`, `get_umask:164`; `find`, `ls`, `mkdir`, `sort`), `support/backup.rs` (`backup():88`; no consumer), `support/safe_traversal.rs` (`DirFd::open:38`, `read_dir:98`, `unlink_at:165`; no consumer), `support/checksum.rs` (the `*sum` builtins), `support/xattr.rs` (`ls`, `mkdir`, `mv`).
- **Process scoping.** `ProcInfo::all_filtered(allowed)` takes a pid predicate (`proc_snapshot.rs:96, :431, :869`). `proc_snapshot.rs:239` reads `/proc/uptime` only to date a process start.
- **Static analysis.** `crates/shell/src/analysis.rs` has arms for `cp`/`install` (:518), `mv` (:541), `rm`/`rmdir`/`unlink` (:564), `touch`/`mkdir`/`truncate` (:576), `dd` (:588), network commands `curl wget nc ssh scp` (:611), `grep` (:617) and a no-op arm that includes `env` (:618). It has no arm for `chmod`, `tar`, `rsync` or `sd`.
- **Workspace crates.** Present: flate2, zstd, xxhash-rust, jiff, regex, rustix, nix, libc, windows-sys, sha1, sha2, md-5 (root `Cargo.toml`). Absent: `tar`, `bzip2`, `lzma-rust2`, `lz4_flex`, `md4`.
- **Other.** `crates/minimizer/src/filters/binary_tools.rs:22` filters the *output* of `strings` and `od` run as external commands; it provides no implementation. `sed.rs:1139` has a private `bre_to_ere`; there is no shared BRE module (upstream `nl.rs:217` needs `bre::bre_to_ere`, and `upstream:crates/pi-builtins/src/bre.rs` is 22653 B).
- **No importer stub**, and no `cl_*`/`sv_*` convar mentions any of the 19.

## 3. Class

**c (port)** for 18 of the 19. **b (not needed)** for `rsync`: a system `rsync` already runs as a spawned process under the OS sandbox and the egress broker (ADR 0028 §4, lines 44-47); the in-process port would open its own sockets and bypass the broker, and local copies are covered by `cp -a` once `cp` lands. What the user does instead: run `rsync` as an external command.

| Builtin | Upstream file | Size | Class | Platform and ADR 0006/0028 concerns | Slice |
|---|---|---|---|---|---|
| nl | `nl.rs` | 25279 B | c | pure filter; needs a BRE helper | 2 |
| fold | `fold.rs` | 15778 B | c | pure filter; display width via `locale.rs` (7209 B) | 2 |
| od | `od.rs` | 44018 B | c | pure filter; its 2 `libc::` uses are errno constants (`od.rs:736, :884`) | 2 |
| strings | `strings.rs` | 36350 B | c | pure filter, GNU binutils 2.46 semantics, no object parsing | 2 |
| shasum | `shasum.rs` | 31471 B | c | reads via `open_read`; reuses `support/checksum.rs` | 3 |
| md5 | `md5.rs` | 11786 B | c | BSD `md5(1)` CLI, distinct from `md5sum` | 3 |
| id | `id.rs` (`cfg(unix)`) | 11837 B | c | identity of the process, correct for an in-process builtin; uses `support/entries.rs` | 4 |
| env | `env.rs` | 57228 B | c | runs a command: must use `Host::command` so the spawn wrapper and sandbox apply; needs `locale.rs`, `quoting_style.rs` | 4 |
| uptime | `uptime.rs` | 26342 B | c | boot time and load per OS (23 `target_os` sites); reads utmp (`uptime.rs:28-32`), which names other users: read it through `open_read` so policy decides | 4 |
| du | `du.rs` | 52243 B | c | recursive read: fd-relative walk (`safe_traversal`), honours read-deny; needs `block_size.rs` (13113 B), `locale.rs`, `quoting_style.rs`, `time_zone.rs` | 5 |
| df | `df.rs` | 56977 B | c | mount table discloses mounts outside the workspace; `support/mounts.rs` exists | 5 |
| rmdir | `rmdir.rs` | 14550 B | c | destructive; `ensure_writable` per directory; `--parents` must stop at policy | 6 |
| chmod | `chmod.rs` (`cfg(unix)`) | 25575 B | c | metadata write; `-R` must not follow links out of the tree; `support/mode.rs` exists | 6 |
| sd | `sd.rs` | 18370 B | c | in-place edit through `atomic_write.rs` (2506 B) and `ensure_writable` | 6 |
| cp | `cp.rs` | 124650 B | c | largest; 31 `vfs` sites to rewrite; writes, recursion, backup (`support/backup.rs`), xattr, symlink races | 7 |
| dd | `dd.rs` + `dd/` | 83897 B | c | raw block I/O; `of=` through `ensure_writable`; device paths; `openfile.rs` (15330 B); `fsync` and `direct` flags (`dd.rs:71-109`) | 8 |
| tar | `tar.rs` | 80327 B | c | extraction hazards: `..`, absolute names, symlink chains, hardlinks, device and setuid members; new crates | 9 |
| lsof | `lsof.rs` + `lsof/` | 109059 B | c | cross-process disclosure; `/proc` (Linux), `proc_pidinfo` (macOS), `windows-sys`; pid filter `may_observe` | 10 |
| rsync | `rsync.rs` + `rsync/` | 2144097 B | b | raw sockets bypass the egress broker; clean-room protocol 29-33 client, server and daemon | none |

Counts of `cfg(unix)`, `cfg(windows)`, `target_os` and `libc::` lines (`grep -c` on the upstream module files) point to where platform code lives and are not a measure of effort: cp 7/6/12/7, du 7/1/0/1, df 15/0/5/5, dd 20/3/5/13, env 7/0/2/9, tar 12/1/0/0, uptime 3/3/23/16, lsof 6/3/6/61.

## 4. Options

**A. Port module by module onto the omp2 host seam (chosen).** Copy each upstream module, replace the `pi_vfs` and `uucore` calls with `Host::open_read`, `Host::ensure_writable`, `Host::command` and `support/*`, keep the CLI behaviour. Cost: large for cp, dd, tar, lsof; small for the filters. Fit: it is how omp2's utilities were made (`crates/shell-builtins/NOTICE:4`: "ported from the earlier `pi-builtins` implementation"), keeps the path policy (ADR 0028 §4) and needs no new runtime (ADR 0035).

**B. Build on existing omp2 code only.** Compose the new commands from existing builtins and support modules. Cost: lower per command, but still new code. Rejected: parity with the reference tools (GNU/uutils option sets, exit statuses, diagnostics) is the product, and hand-composing from existing builtins would not reach it.

**C. Depend on `pi-vfs` and `uucore` and port unchanged.** Cost: smallest diff. Rejected: omp2 has no vfs crate or `uucore` dependency (`crates/shell-builtins/Cargo.toml`), a second filesystem seam beside the path policy would let a builtin skip `check_write`, and `pi` names must not enter omp2.

**D. Leave them external.** The OS sandbox already contains spawned processes (ADR 0028 §4). Cost: zero. Rejected as the whole answer: it fails where the binary is absent (minimal containers, Windows, BSD `md5` versus GNU `md5sum`) and gives no in-process path denial or cancellation. It is the answer for `rsync` only.

**E. Ship them as an extension.** Rejected: ADR 0036 makes extensions Python, and these are core shell commands with no vendor involved (ADR 0049).

Choice: A, in ten slices, off by default and enabled per name (see the ADR).

## 5. Plan

Gate shared by every slice: convar `sv_shell_builtins_extra: Vec<Str>` (default empty, declared next to `SV_SHELL_EMBEDDED_BUILTINS` in `crates/envd/src/exec_settings/shell.rs`) plus a new factory function `extra_builtins(enabled)`; tar's compression crates sit behind the cargo feature `cap-shell-archive`. Linux tests run with `just remote-test-pkg omp-shell-builtins` (`justfile:129`); the macOS and Windows branches need a local `just test-pkg omp-shell-builtins` (`justfile:294`) on those hosts.

| # | Slice | Acceptance |
|---|---|---|
| 1 | Gate: convar, `extra_builtins`, four call sites | with the default, the registered name set equals today's; naming an unknown name is a validation error |
| 2 | nl, fold, od, strings (+ `support/bre.rs`, `support/locale.rs`) | golden tests per option group; read-deny test in `read_policy_tests.rs` for each; `sed` behaviour unchanged |
| 3 | shasum, md5 | digests equal `sha*sum` on the same input; `-c` check mode; read-deny |
| 4 | id, env, uptime | `env cmd` launches through the spawn wrapper (`set_test_spawn_wrapper`); uptime with utmp unreadable by policy prints without a user-count error |
| 5 | du, df | du skips or refuses a read-denied root; df output has no mount outside the policy's readable view |
| 6 | rmdir, chmod, sd | write-deny test per command; chmod `-R` does not leave the tree through a link; sd `-i` is atomic |
| 7 | cp | write-deny on destination; symlink-swap race during `-r` stays inside the root; backup and xattr options |
| 8 | dd | `of=` under a write-deny root fails; reads through `open_read`; cancellation stops the copy |
| 9 | tar (`cap-shell-archive`) | extraction of `../x`, absolute, and symlink-then-file members is refused; create, list and extract round-trip for gz, bz2, xz, zst; `cargo deny` passes with the new crates |
| 10 | lsof | pids outside `may_observe` never listed; Linux `/proc` fixture test; macOS and Windows local |

Slices that add `chmod`, `tar` or `sd` also add the matching `analysis.rs` arm (see open question 3).

## 6. Open questions

1. Should the new commands be on by default later? Recommendation: no change now; revisit in an amendment after slices 6-9 have run a release with the convar available.
2. How is the `sv_shell_builtins_extra` value spelled: names only, or also the keyword `all`? Recommendation: names plus `all`; `all` means every name `extra_builtins` offers in that build.
3. `analysis.rs` has no arm for `chmod`, `tar`, `sd`; an unconditional arm changes how an existing external `chmod` is analysed. Recommendation: apply the arm only when the name is in the convar value (whether the analyser can see the enabled set was not established).
4. `dd of=/dev/…` under a sandbox: not verified whether `check_write` already refuses device paths. Recommendation: refuse unless the policy lists the path, whenever `sv_sandbox_mode` is not `off`.
5. Conflict check with hard constraints: none found. Additive (new names, default off, `utility_builtins()` unchanged), no vendor code (ADR 0049), no new language (ADR 0035, 0036), no roster change (ADR 0024), policy stays in the host (ADR 0006).

## 7. Upstream origin and license

Files a port draws from: `upstream:crates/pi-builtins/src/{nl,fold,od,strings,shasum,md5,id,env,uptime,du,df,rmdir,chmod,sd,cp,dd,tar,lsof}.rs` with `dd/` and `lsof/`, and the helpers `bre.rs`, `locale.rs`, `block_size.rs`, `quoting_style.rs`, `time_zone.rs`, `atomic_write.rs`, `file_backup.rs`, `progress.rs`, `openfile.rs` (helper need derived from `<module>::` references in each builtin; `bre` from `nl.rs:217`).

License: the crate is MIT (`upstream:crates/pi-builtins/LICENSE-MIT`; upstream workspace `license = "MIT"`, `upstream:Cargo.toml:33`). `upstream:crates/pi-builtins/LICENSE` reproduces third-party notices and the lineage of each file:
- uutils coreutils, MIT, "Copyright (c) uutils developers" (LICENSE lines 8-16): its "Covers" list includes `cp` and `nl` but omits od, du, df, dd, env, id, chmod, fold, rmdir, uptime and shasum, although those module headers cite uutils or `uucore`. A port reads each module header and carries whatever notice it names.
- sd (https://github.com/chmln/sd), MIT: covers `sd` (LICENSE lines 149-155).
- libproc-rs (https://github.com/andrewdavidmackenzie/libproc-rs), MIT: covers the macOS `proc_info.h` layouts in `lsof/macos.rs` (LICENSE lines 203-209).
- Original work under the workspace MIT license: `lsof`, `tar`, `shasum`, `md5`, `strings` (LICENSE lines 230-236). `rsync` is a clean-room implementation (`rsync.rs:1`) and is not ported.

omp2 already carries this lineage in `crates/shell-builtins/NOTICE` and `LICENSE`; each slice appends the matching notice lines to `NOTICE`. Strings in ported code that name `pi` (for example "not a pi extra", `extras.rs`) must be renamed.
