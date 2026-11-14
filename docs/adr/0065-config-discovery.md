# 0065. Discovery of Copilot instructions, `.github/instructions` rules, and an explicit import of `~/.ssh/config`

Status: proposed
Date: 2026-11-13
Area: runtime

Research: [docs/research/config-discovery.md](../research/config-discovery.md) (Gap 7 of the upstream gap closure). This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- One provider, `github`, priority 30 (`upstream:packages/coding-agent/src/discovery/github.ts:40-42`). It reads `<cwd>/.github/copilot-instructions.md` with no ancestor walk (`upstream:packages/coding-agent/src/discovery/github.ts:52`), a user file `~/.copilot/copilot-instructions.md` relocatable by `COPILOT_HOME` (`upstream:packages/coding-agent/src/discovery/github.ts:70-81`), and extra `AGENTS.md` files from the environment variable `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` (`upstream:packages/coding-agent/src/discovery/github.ts:86-97`). At one directory depth the Copilot file (30) shadows a plain `AGENTS.md` (10) (`upstream:docs/context-files.md:161`).
- `.github/instructions/**/*.instructions.md` become rules. `applyTo` is split on commas; `*`, `**` and `**/*` mean always apply; any other glob scopes the rule, with a generated description; a missing `applyTo` loads unscoped with a warning (`upstream:packages/coding-agent/src/discovery/github.ts:204-232`).
- `~/.ssh/config` is read only for concrete `Host` aliases, following `Include` to depth 16 with a cycle guard, skipping wildcards, negations and `Match` blocks (`upstream:packages/coding-agent/src/discovery/ssh-config.ts:52-65`, `upstream:packages/coding-agent/src/discovery/ssh-config.ts:78-92`). An entry carries only the alias, because OpenSSH resolves the rest at connect time (`upstream:packages/coding-agent/src/discovery/ssh-config.ts:1-10`). That works there only because upstream shells out to `ssh`.

### What omp2 has

- Context files: `.omp/AGENTS.md`, `.claude/CLAUDE.md`, `.gemini/GEMINI.md`, `AGENTS.md`, `CLAUDE.md` in provider order, one user winner, walked up from the project root, 1 MiB cap, project files read contained inside the repository (`crates/driver/src/discovery/rules.rs:110`, `crates/driver/src/discovery/rules.rs:116`, `crates/driver/src/discovery/rules.rs:153-170`). No Copilot entry in either table.
- Rules from `.omp/rules`, `RULES.md`, `.agent[s]/rules`, `.cursor/rules`, `.windsurf/rules`, `.clinerules` and legacy single files, first-wins on name with a warning (`crates/driver/src/discovery/rules.rs:372-392`, `crates/driver/src/discovery/rules.rs:482-503`). Frontmatter already has `globs` and `alwaysApply` (`crates/driver/src/discovery/rules.rs:791-803`). No `.github/instructions` support.
- Context files and rules are data the model reads, never authority; they load contained and sit outside the workspace-trust digest (`crates/ext/README.md:115-117`).
- SSH: `omp-ssh` is a native client with its own `hosts.toml`; a host needs an address, user, a pinned SHA-256 host key and an auth policy (`crates/ssh/src/lib.rs:60-75`). The project file is read only in a trusted workspace and never shadows a user alias (`crates/ssh/src/lib.rs:157-170`). `known_host_fingerprint` reads a pin from OpenSSH `known_hosts` (`crates/ssh/src/lib.rs:397-421`), and the v1 importer already derives hosts this way (`crates/driver/src/v1_import/assets/ssh.rs:128-129`). `omp-exec-ssh` never shells out to `ssh` (`crates/exec-ssh/README.md:1-6`). No ProxyJump or ProxyCommand.

### Rules that bind

- Additive only; no existing provider order, convar, command or schema changes.
- ADR 0049: vendor integrations live in extensions. Reading a Copilot instruction file is a file-format read, not a vendor integration (O6).
- Untrusted repository text must never gain authority; SSH hosts are authority and stay gated by workspace trust.

## Decision

**D1. Copilot context files, appended last.** Add `.github/copilot-instructions.md` to the project table and `~/.copilot/copilot-instructions.md` to the user table, ranked after `CLAUDE.md` so no existing repository changes behaviour. No `COPILOT_HOME`, no `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` in this record: an environment variable that points discovery at arbitrary directories is a prompt-injection path for any parent process that sets it.

**D2. `.github/instructions` as rules.** A `github` rule provider scans `<project>/.github/instructions` recursively for `*.instructions.md`, strips the suffix for the name, maps `applyTo` into `globs`, treats `*`, `**` and `**/*` as `alwaysApply`, loads a missing `applyTo` unscoped with a `Warning`, and sits after the cursor, windsurf and cline providers so first-wins collisions keep today's behaviour. Contained reads, same as `.cursor/rules`.

**D3. `~/.ssh/config` is imported, not discovered.** An explicit user-level command reads `~/.ssh/config` only, never a workspace file, and writes user-scope `hosts.toml` entries. Per host: concrete `Host` alias, `HostName`, `User`, `Port`, `IdentityFile` (key if present and unencrypted, else agent), pin from `known_hosts`. A host with no pin or no user is skipped with a reason. `ProxyJump`, `ProxyCommand`, `Match`, `CertificateFile` and unknown `%` tokens are refused with a reason. A name already declared keeps the declared host. The project `hosts.toml` is never written.

**D4. Parser.** `Include` to depth 16, glob expansion, a visited set, quoting and `=` separators, reads limited to `~/.ssh` and `/etc/ssh`. Hand-written, modelled on upstream's 117-line file, unless an evaluated crate passes `deny.toml` (O5).

**D5. Gates.** D1 and D2 ship behind default-off convar `sv_discover_copilot`. D3 ships with the existing `cap-ssh` feature and is opt-in by being a command.

## Rejected options

- **Rank the Copilot file above `AGENTS.md`, as upstream does.** It changes which file wins in repositories that have both today.
- **Auto-discover aliases only, as upstream does.** The aliases could not connect: omp2 needs address, user and pin.
- **Shell out to `ssh -G`.** Adds a runtime dependency on a binary and runs `ProxyCommand` or `Match exec` from the config, which the native client deliberately never does.
- **Read `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`.** See D1.
- **Read `.ssh/config` from a workspace.** SSH hosts are authority.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**S1. Copilot context files.**
- Tests: `copilot_file_alone_is_injected`, `repo_with_agents_md_and_copilot_file_behaves_as_before`, `copilot_file_outside_repo_via_symlink_is_refused` (as `crates/driver/src/discovery/rules.rs:1394`), `user_copilot_file_loads_after_other_user_files`. Extend `crates/driver/src/discovery/rules.rs:1278` and `crates/driver/src/discovery/rules.rs:1305`.
- Gate: `sv_discover_copilot` default off.

**S2. `.github/instructions` rules.**
- Tests, ported from `upstream:packages/coding-agent/test/discovery/github-copilot.test.ts`: `always_apply_spellings_inject_whole`, `path_glob_lists_rule_for_rule_url`, `missing_apply_to_warns_and_loads_unscoped`, `name_collision_keeps_first_with_existing_warning`, `non_instructions_suffix_ignored`.

**S3. SSH config parser, no wiring.** A pure function from config text to candidate hosts and refusals.
- Tests, from `upstream:packages/coding-agent/test/discovery/ssh-config.test.ts`: `include_depth_16_and_cycle_guard`, `glob_include_sorted`, `quoted_and_equals_forms`, plus `proxyjump_proxycommand_match_certificatefile_each_refused_with_reason` and `no_read_outside_ssh_dirs`.

**S4. The import command.** Pins from `known_hosts` through `known_host_fingerprint`, validates through `validate_configured_host`, writes user-scope `hosts.toml`.
- Tests: `fixture_config_and_known_hosts_produce_expected_hosts_toml`, `second_run_changes_nothing`, `declared_host_is_not_overwritten`, `project_hosts_toml_never_written`, `host_without_pin_is_skipped_with_reason`.
- Gate: `cap-ssh`.

## Upstream origin and license

A port draws from `upstream:packages/coding-agent/src/discovery/github.ts` (340 lines) and `upstream:packages/coding-agent/src/discovery/ssh-config.ts` (117 lines) with their tests. TypeScript, rewritten in Rust. Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). Keep the notice for copied text and fixtures.

## Consequences

- Repositories with Copilot instructions gain them in the prompt, only when the convar is on.
- No new authority: the text is contained and size-capped like `AGENTS.md`.
- SSH users get a one-shot way to bring existing hosts, with an explicit list of what was refused.

## Open questions for the owner

- **O1.** Rank of the Copilot file against `AGENTS.md`. Recommendation: last, as decided above; revisit with usage data.
- **O2.** How `Rule.globs` is matched at read time was not traced (UNVERIFIED). Recommendation: check in S2 before claiming `applyTo` parity; if the semantics differ, document the difference in the rule's description.
- **O3.** Should the import also be a chat `/ssh` subcommand (`crates/app/src/ssh_cmd.rs:35` is the CLI enum)? Recommendation: CLI first.
- **O4.** Multi-name `Host a b c` entries and `%h` tokens. Recommendation: import each concrete name; refuse tokens other than `%h` in `HostName`.
- **O5.** Hand-written parser or a crate. Recommendation: hand-written; no crate was evaluated.
- **O6.** Does ADR 0049 apply to a Copilot file? Recommendation: no, it is a plain file format like `.cursorrules`; the owner may overrule.

## Status in omp

**Status: Not started.** Reusable today: the provider tables, `RuleHeader`, the contained reader, `known_host_fingerprint`, `validate_configured_host`, the v1 `ssh.json` importer as a model.

## References

[docs/research/config-discovery.md](../research/config-discovery.md); ADR 0049 (vendor integrations in extensions); ADR 0035 (error discipline).

