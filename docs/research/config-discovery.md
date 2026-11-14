# Gap 7: Copilot instructions, `.github/instructions` and `~/.ssh/config`

Status: research only. Nothing here is implemented or authorized.
Date: 2026-11-13
Baseline: omp2 f4186b783b; upstream c308970263 (pinned checkout).

Evidence keys:

- **OBSERVED** means read in code or docs at the baseline. Citations use `path:line`. Upstream paths carry the `upstream:` prefix.
- **[INFERENCE]** means the author's reasoning. Every size estimate is an inference.
- **UNVERIFIED** means no run or source confirmed it.

No build, test or run was made for this document.

## Question

Upstream discovers three things omp2 does not: GitHub Copilot instruction files, `.github/instructions/*.instructions.md` rules scoped by `applyTo`, and SSH hosts listed in `~/.ssh/config`. What do they do there, what does omp2's own context, rules and SSH code do instead, and what would each addition cost?

Out of scope: Copilot prompts and skills directories (upstream also reads `.github/prompts` and `.github/skills`), and the plugin and marketplace loaders.

## 1. Upstream

### Copilot context files

All of this lives in one provider, `github`, priority 30 (`upstream:packages/coding-agent/src/discovery/github.ts:40-42`).

- Project file: `<cwd>/.github/copilot-instructions.md`, read once, with no ancestor walk-up (`upstream:packages/coding-agent/src/discovery/github.ts:52`; the docs say "no ancestor walk-up" at `upstream:docs/context-files.md:76`).
- User file: `~/.copilot/copilot-instructions.md`, relocatable with `COPILOT_HOME`, only when the user source is enabled (`upstream:packages/coding-agent/src/discovery/github.ts:70-81`).
- Extra user `AGENTS.md` candidates from the comma-separated environment variable `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` (`upstream:packages/coding-agent/src/discovery/github.ts:86-97`, `upstream:packages/coding-agent/src/discovery/github.ts:267-270`).
- Priority matters: at one directory depth the `github` file (30) shadows a standalone `AGENTS.md` (10) (`upstream:docs/context-files.md:161`).
- Users can add other basenames through `contextFiles.extra` (`upstream:docs/context-files.md:89`).

### `.github/instructions/**/*.instructions.md`

Two views of the same files.

- As **instructions** (a separate capability): recursive scan of the project directory and of each `<dir>/.github/instructions` for the custom dirs; only names ending `.instructions.md`; the frontmatter `applyTo` string is kept; the rule name is the file name without the suffix (`upstream:packages/coding-agent/src/discovery/github.ts:105-134`, `upstream:packages/coding-agent/src/discovery/github.ts:136-157`).
- As **rules**: the same files become rulebook entries (`upstream:packages/coding-agent/src/discovery/github.ts:163-190`). `applyTo` is split on commas and lists are accepted (`upstream:packages/coding-agent/src/discovery/github.ts:221-227`). A glob of `*`, `**` or `**/*` means "always apply" and clears the globs (`upstream:packages/coding-agent/src/discovery/github.ts:213-215`, `upstream:packages/coding-agent/src/discovery/github.ts:229-232`). Any other glob keeps the rule listed, scoped by those globs, with a generated description when the file gives none (`upstream:packages/coding-agent/src/discovery/github.ts:217-218`, `upstream:packages/coding-agent/src/discovery/github.ts:234-237`). A missing `applyTo` loads the rule unscoped and adds a warning (`upstream:packages/coding-agent/src/discovery/github.ts:204-207`).
- The rulebook docs say the same and add that a rule with `condition`, `astCondition` or `question` fields stays a stream-rule regardless of `applyTo` (`upstream:docs/rulebook-matching-pipeline.md:150-159`).
- Tests exist: `upstream:packages/coding-agent/test/discovery/github-copilot.test.ts`.

### `~/.ssh/config`

- A provider `ssh-config`, priority 3, below the explicit JSON host file so the JSON entry wins a name clash (`upstream:packages/coding-agent/src/discovery/ssh-config.ts:10`, `upstream:packages/coding-agent/src/discovery/ssh-config.ts:96-110`).
- It lists only concrete `Host` aliases from `~/.ssh/config` then `/etc/ssh/ssh_config`, following `Include` in place. Each entry carries the alias and nothing else, because OpenSSH resolves HostName, User, Port, IdentityFile and ProxyJump itself at connect time (`upstream:packages/coding-agent/src/discovery/ssh-config.ts:1-10`).
- Negated (`!x`) and wildcard (`*`, `?`) patterns are skipped, as are `Match` blocks (`upstream:packages/coding-agent/src/discovery/ssh-config.ts:78-83`).
- `Include` nests at most 16 levels, the OpenSSH limit; relative includes resolve against `~/.ssh` or `/etc/ssh`; globs expand and sort; a visited-file set stops cycles (`upstream:packages/coding-agent/src/discovery/ssh-config.ts:22-24`, `upstream:packages/coding-agent/src/discovery/ssh-config.ts:52-65`, `upstream:packages/coding-agent/src/discovery/ssh-config.ts:84-92`).
- The first definition of an alias wins, user file first (`upstream:packages/coding-agent/src/discovery/ssh-config.ts:98-104`).
- Tests: `upstream:packages/coding-agent/test/discovery/ssh-config.test.ts`.

The design relies on upstream shelling out to the system `ssh`, which does the real resolution. That is the reason the parser can stay tiny.

## 2. omp2 inventory

### Context files and rules

- Context files are `.omp/AGENTS.md`, `.claude/CLAUDE.md`, `.gemini/GEMINI.md`, `AGENTS.md` and `CLAUDE.md` per directory depth, in that provider order (`crates/driver/src/discovery/rules.rs:153-159`). One user file wins from native, Claude, Codex, Gemini and OpenCode locations (`crates/driver/src/discovery/rules.rs:164-170`). There is no Copilot entry in either table. A search for `copilot` in `crates/driver/src`, `crates/agent/prompts`, `crates/ext/README.md` and `crates/core/src` found only the provider-credential shaper, not instruction discovery (`crates/driver/src/registry.rs:32`).
- Discovery walks up from the project root (`crates/driver/src/discovery/rules.rs:175-215`). Reads are size-capped at 1 MiB (`crates/driver/src/discovery/rules.rs:110`) and project files are read contained inside the repository root (`crates/driver/src/discovery/rules.rs:116`).
- Rules come from `.omp/rules`, user `agent/rules`, `RULES.md`, `.agent/rules`, `.agents/rules`, Claude plugins, `.cursor/rules`, `.cursorrules`, `.windsurf/rules`, `.windsurfrules` and `.clinerules`. Name collisions are first-wins with a warning (`crates/driver/src/discovery/rules.rs:372-392`, `crates/driver/src/discovery/rules.rs:401`, `crates/driver/src/discovery/rules.rs:445`, `crates/driver/src/discovery/rules.rs:482-503`, `crates/driver/src/discovery/rules.rs:542`). The module header lists the same set (`crates/driver/src/discovery/rules.rs:13-20`).
- Rule frontmatter is `description`, `globs`, `alwaysApply`, `condition`, `scope`, `interrupt_mode` and `agents` (`crates/driver/src/discovery/rules.rs:791-803`). `globs` accepts one value or a list. This is the shape `applyTo` needs to map onto.
- Trust model: context files, rules and skills are "data the model reads, never authority" and load contained, so they are excluded from the workspace-trust digest (`crates/ext/README.md:115-117`). Discovery runs once at composition and is toggled by the `include_context_files` and `include_rules` overrides (`crates/driver/src/headless/kernel.rs:4020-4036`).
- No `.github/instructions` support: a search for `.github/instructions` and `applyTo` in `crates/` matched nothing in discovery code.

### SSH

- `omp-ssh` is a native client (russh) with its own host authority, `hosts.toml`; `omp-exec-ssh` never shells out to an SSH binary (`crates/exec-ssh/README.md:1-6`, `crates/ssh/README.md:1-4`). There is no parser for `~/.ssh/config`; a search for `ProxyJump`, `ssh_config` and `IdentityFile` in `crates/ssh/src` found nothing.
- A configured host has five required fields: `address`, `user`, a pinned SHA-256 `host_key`, an `auth` policy and optional `port` and `timeout_secs` (`crates/ssh/src/lib.rs:60-75`). Auth is the SSH agent or one unencrypted key file whose permissions are checked; passwords are refused (`crates/ssh/src/lib.rs:92-105`). Validation requires a non-empty address and user, a non-zero port and a `SHA256:` host key (`crates/ssh/src/lib.rs:442-451`). Aliases are limited to alphanumerics, dot, underscore and dash, 128 bytes (`crates/ssh/src/lib.rs:430-438`).
- Two `hosts.toml` files are layered. The project file is read only in a trusted workspace, and a project alias never shadows a user alias (`crates/ssh/src/lib.rs:157-170`). The workspace-trust inventory gates "envd's MCP, LSP/DAP, SSH host and vault loaders" (`crates/ext/README.md:58-63`).
- The pin can already be read from OpenSSH's `known_hosts`: `known_host_fingerprint` returns the SHA-256 for a host and port, preferring Ed25519, then ECDSA, then RSA (`crates/ssh/src/lib.rs:397-421`).
- Precedent for deriving hosts from another source exists. The v1 importer converts `ssh.json` into `hosts.toml` and reads each pin from `~/.ssh/known_hosts`, "so the pin is read from the same entry v1's `ssh` trusted"; a name v2 already declares keeps v2's host (`crates/driver/src/v1_import/assets/ssh.rs:1-9`, `crates/driver/src/v1_import/assets/ssh.rs:128-129`).
- The user-facing command is `ssh add` with explicit `--host`, `--user`, `--port`, `--host-key` and `--key` (`crates/app/src/ssh_cmd.rs:43-64`).
- ProxyJump, ProxyCommand and agent forwarding: not found (searched: `jump`, `proxy`, `forward_agent` in `crates/ssh/src/lib.rs`).

### Gap table

| Item | Upstream | omp2 | Gap |
|---|---|---|---|
| `.github/copilot-instructions.md` (project) | context file, priority 30 | absent from `PROJECT_CONTEXT_PROVIDERS` | one table row, plus a priority decision |
| `~/.copilot/copilot-instructions.md` (user) | yes, `COPILOT_HOME` | absent from `USER_CONTEXT_PROVIDERS` | one table row |
| `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` | yes | none | optional; adds an env-driven read path, so defer |
| `.github/instructions/**/*.instructions.md` | rules with `applyTo` | no `.github` rule provider | new rule source with an `applyTo` mapper |
| `applyTo` to `globs` / `alwaysApply` | yes | `globs` and `alwaysApply` exist in `RuleHeader` | mapping only |
| Missing `applyTo` warning | yes | `Warning` type exists | reuse |
| `~/.ssh/config` host aliases | yes, alias only, resolved by `ssh` | no import; native client needs full fields | real parser plus a pin source |
| `Include` handling | yes, depth 16, glob, cycle set | n/a | port with the same limits |
| ProxyJump, ProxyCommand | handled by `ssh` | unsupported | refuse in the first slice |

## 3. Class

- Copilot context files and `.github/instructions` rules (parts A and B): **class c (port).** Upstream's logic is small and maps onto omp2's existing provider tables and `RuleHeader`.
- `~/.ssh/config` (part C): **class d (propose).** Upstream lists aliases and lets the system `ssh` resolve them. omp2 has no `ssh` binary in the loop and needs every `HostConfig` field filled (`crates/ssh/src/lib.rs:60-75`). A direct port would produce hosts the native client cannot connect to. So part C is a new design, not a port.

Verdict: implement A and B after two open questions are answered; implement C only as an explicit, user-level import.

## 4. Options

### Parts A and B

1. **Build on existing omp2 code.** Add table rows and one rule provider in `crates/driver/src/discovery/rules.rs`. Cost: about 2 days for both with tests. Fit: best. The read path already gives the 1 MiB cap and containment.
2. **Port upstream's `github.ts` as a separate provider module.** Cost: higher, and it would duplicate the discovery engine. Fit: poor. omp2 has no provider registry like upstream's.

Choice: option 1. [INFERENCE]

### Part C

1. **Auto-discover aliases only, like upstream.** Cost: low. Fit: poor. The listed hosts could not connect: no `HostName`, user or pin.
2. **Explicit import command into `hosts.toml`.** Cost: 3 to 5 days. Fit: good. It follows the v1 importer's precedent of pinning from `known_hosts` (`crates/driver/src/v1_import/assets/ssh.rs:128-129`) and keeps the trust inventory intact.
3. **Shell out to `ssh -G <alias>`** to let OpenSSH resolve the config. Cost: medium. Fit: poor. It adds a runtime dependency on an `ssh` binary and runs `ProxyCommand` or `Match exec` from config, which `omp-exec-ssh` deliberately never does (`crates/exec-ssh/README.md:1-6`).

Choice: option 2, with a parser that is either hand-written (upstream's 117-line file is a model: `upstream:packages/coding-agent/src/discovery/ssh-config.ts:41-93`) or a crate. Which one is open; no crate was evaluated (UNVERIFIED), and `deny.toml` applies. [INFERENCE]

## 5. Plan

### Slice 1: Copilot context files

- Add `("github", ".github/copilot-instructions.md")` to the project table and `("github", ".copilot/copilot-instructions.md")` to the user table in `crates/driver/src/discovery/rules.rs`. Rank them after `CLAUDE.md` so no existing repository changes behaviour. Skip `COPILOT_HOME` and `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`: an environment variable that points discovery at arbitrary directories is a prompt-injection path for a parent process that sets it.
- Acceptance: a repository with only the Copilot file gets it injected; a repository with both `AGENTS.md` and the Copilot file behaves as before; a symlink that escapes the repository is refused, like the existing case at `crates/driver/src/discovery/rules.rs:1394`.
- Tests: extend the discovery tests at `crates/driver/src/discovery/rules.rs:1278` and `crates/driver/src/discovery/rules.rs:1305`.
- Gate: default-off convar `sv_discover_copilot`.

### Slice 2: `.github/instructions` rules

- Add a `github` rule provider scanning `<project>/.github/instructions` recursively for `*.instructions.md`. Strip the suffix for the name. Map `applyTo` into `globs`, with `*`, `**` and `**/*` meaning `alwaysApply`. A missing `applyTo` loads the rule unscoped with a `Warning`. Place it after `cursor` and `windsurf` so first-wins collisions keep today's behaviour. Read through the same contained reader as `.cursor/rules` (`crates/driver/src/discovery/rules.rs:482-503`).
- Acceptance: the three always-apply spellings inject the rule whole; a path glob lists it for `rule://<name>`; a missing `applyTo` warns; a name that collides with an existing rule is skipped with the existing collision warning.
- Tests: port the cases from `upstream:packages/coding-agent/test/discovery/github-copilot.test.ts`.
- Gate: the same convar.

### Slice 3: SSH config parser (no wiring)

- A pure function from `~/.ssh/config` text to a list of resolved candidate hosts and a list of refusals. Handles `Host` (concrete aliases only), `HostName`, `User`, `Port`, `IdentityFile`, `Include` (depth 16, glob, cycle set), quoting and `=`. Refuses `ProxyJump`, `ProxyCommand`, `Match`, `CertificateFile` and unknown `%` tokens, each with a reason.
- Acceptance: tests from upstream's `ssh-config.test.ts` for `Include` and aliases pass; every refused directive yields a typed reason; no file is read outside `~/.ssh` and `/etc/ssh`.
- Gate: none needed; nothing calls it yet.

### Slice 4: import command

- `omp ssh import-config` (name open) reads `~/.ssh/config`, pins each host from `known_hosts` through `known_host_fingerprint`, validates through `validate_configured_host`, and writes the user-scope `hosts.toml`. A host without a pin is skipped with the reason. A name already declared keeps the declared host, as the v1 importer does.
- Acceptance: a fixture config and `known_hosts` produce the expected `hosts.toml`; a second run changes nothing; the project `hosts.toml` is never written.
- Gate: `cap-ssh` (the existing feature) plus the command itself being opt-in.

### Security

- **Prompt injection.** A project `.github/copilot-instructions.md` is untrusted text in the system prompt. omp2 already treats `AGENTS.md` this way: data, contained, no authority (`crates/ext/README.md:115-117`). Slices 1 and 2 add no new class of risk. Keep the size cap and containment. Add no tool permission or hook from these files.
- **SSH is authority.** Keep the import manual and user-level. Never read `.ssh/config` from a workspace. The trust inventory already gates project SSH hosts (`crates/ext/README.md:58-63`).
- **ProxyCommand** runs an arbitrary program. The native client cannot and must keep refusing it.
- **Pin from `known_hosts`** trusts whatever the user's OpenSSH already accepted. Say so in the command's help.

## 6. Open questions

1. Ranking of the Copilot file against `AGENTS.md`: upstream puts it above (30 against 10). The plan puts it last so nothing changes. Which does the owner want?
2. `applyTo` semantics against omp2 `globs` matching. How `Rule.globs` is matched at read time was not traced (UNVERIFIED). Check before claiming parity.
3. Whether the import should also be a `/ssh` chat subcommand (`crates/app/src/ssh_cmd.rs:35` is the CLI enum).
4. Hosts with `Host a b c` (several names) and `HostName %h` tokens.
5. Hand-written parser or a crate. No crate was evaluated (UNVERIFIED).
6. A new `sv_discover_copilot` convar is additive. No existing tool, convar, hook, scheme or CLI command changes. No locked ADR conflicts: ADR 0049 concerns vendor integrations; reading a Copilot file is a file-format read, not a vendor integration. Flag for the owner if they read it differently.

## 7. Upstream origin and license

A port would draw from `upstream:packages/coding-agent/src/discovery/github.ts` (340 lines) and `upstream:packages/coding-agent/src/discovery/ssh-config.ts` (117 lines), with their tests. Both are TypeScript and would be rewritten in Rust. The upstream `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). Keep the notice for any copied text or test fixture.

