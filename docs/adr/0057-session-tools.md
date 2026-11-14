# 0057. Session tools: local files in the editor, model presets, anonymized export, screen recording

Status: proposed
Date: 2026-11-13
Area: tools

Research: [session-tools](../research/session-tools.md). Ledger gap 10. This record covers the class c and d items only (`/local`, model presets, `/anon`, `/record` and `omp play`). `/forget-approvals` is class b (omp2 persists no approvals) and is explained in the research note.

## Context

- Upstream OMPv1 (can1357/oh-my-pi `c308970263`, paths under `packages/coding-agent/src`, written `up:`) has four session features omp2 lacks:
  - `/local` opens a session `local://` file in `$EDITOR` with a picker (`up:slash-commands/builtin-session.ts:606`).
  - `/modelpreset [list|save|switch|delete]` (`up:slash-commands/builtin-modes.ts:801`) over `up:config/model-presets.ts`: named snapshots of the role assignments and the default thinking level (`saveModelPreset` 104, `applyModelPreset` 321).
  - `/dump anon` (`up:slash-commands/builtin-collaboration.ts:242`) and `omp anonymize` (`up:cli-commands.ts:61`) over `up:session/session-anonymizer.ts` (1818 lines): an allowlist, described below.
  - `/record` (`up:slash-commands/builtin-control.ts:103`) writes a `.ompcast` file (`up:stream/recording.ts:31`) and `omp play` replays it (`up:cli-commands.ts:205`).
- **`/local` today.** `local://` exists (`crates/envd/src/tool_url/local.rs:29,170`, registered at `crates/envd/src/tool_url.rs:689`) and already contains the containment rules: the session root is canonicalized and must start with the sessions directory (`local.rs:256-264`); a target is joined to the root, canonicalized, and must `starts_with` the root (`local.rs:267-279`, "local:// path escapes the session scratch root."); listings skip entries whose canonical path leaves the root and keep only files and directories (`entries`, `local.rs:281-310`, and `completion_files`, 312-327). The editor round trip exists for the composer draft only: `launch_editor` (`crates/chat/src/editor.rs:157-175`, private) runs `sh -c '<editor> "$1"' sh <path>`, so the path is a positional argument, never re-split. `omp local` (`crates/app/src/cli.rs:1141`) is local inference and does not collide with a slash command.
- **Presets today.** `ai_model_roles` is a `Kv` with flags `archive | project` (`crates/catalog/src/settings.rs:1131-1138`); `ai_default_thinking` is a `ThinkingEffort` (1191). `ai_model_role_storage` (1140) has **no consumer**: `ModelSettings.role_storage` is assigned at 425 and read only in tests (1389-1401). The model picker writes roles by reading the whole `ai_model_roles` Kv, replacing one entry, and executing `ai_model_roles {…}` at `Source::Console` (`write_role`, `crates/chat/src/host.rs:3913-3929`), then runs `writecfg` (`host.rs:3901`), ignoring role storage. `Ctx::exec` is strict and has no rollback (`crates/con/src/ctx.rs:1467`). A kv-block leaf is untyped text, so `infer` turns a bare `true`, `false`, integer or float into Bool, Int or Float (`crates/con/src/script.rs:373-398`). `ThinkingEffort` has no `Auto` (`crates/catalog/src/thinking.rs:36`), but `:auto` is a valid role-selector suffix (`crates/catalog/src/selection.rs:1132`).
- **Anonymize today.** `/dump` only copies the transcript and the request sidecar path (`crates/chat/src/commands/control.rs:48,161`). The debug report's `create_bundle` (`crates/app/src/diagnostics.rs:125-173`) ships the **raw** journal as `session/session.jsonl`, logs, profiles and a settings dump, scrubbed only by `redact_sensitive_credentials` (`crates/observability/src/redact.rs:27`, a token-shape scrub); its system facts carry the absolute working directory and `SHELL` (`crates/app/src/debug.rs:35-50`), so a username survives. It is a diagnostic bundle, not an anonymizer.
- **What a journal holds.** The kind vocabulary is twelve closed kinds (`crates/journal/src/kind.rs`: `journal, turn.start, msg.user, msg.assistant.start, stream, msg.assistant.end, tool.call, tool.update, tool.result, turn.receipt, patch, compaction`) but the content is mostly **inside** them: `patch` carries DOM operations (`Ins`, `Set`, `Mv`, `crates/dom/src/op.rs`) whose nodes carry props from a closed `PropId` (`crates/vocab/src/lib.rs:443`) or a custom key, and typed JSON in a `Data` prop (`FileMentions { files: [{ path, content, state }] }`, `MsgUser { text, attachments, author, key }`, `AsyncJobDelivery { label, artifact, fault }`, `SkillPrompt { name, args, path, prompt_body }`, `AdvisorNote { note }`, IRC and workpool `body`, `crates/journal/src/data.rs`). A file mention writes the file's **body** into a DOM `User` node (`crates/session/src/session.rs:739-765`). `stream` carries text deltas, `tool.call` carries `args` and the intent `i`, `compaction` carries `summary` and `warning`, and every `Entry` has a free-text `label` (`crates/journal/src/entry.rs:70-82`). Blob hashes link to attachment content.
- **Upstream's anonymizer, read.** `FIELD_RULES` gives every field omp writes one rule (`up:session/session-anonymizer.ts:28-60`): `num` and `time` kept; `enum`, `identity` and `id` kept only when they match an identifier shape (`IDENTIFIER`, 1235), else tokenized; `path` mock per segment (`path`/`segment`, 1294-1316) preserving separators, scheme, drive, extension and read selectors; `text` replaced by `[redacted #N: <chars> chars, <lines> lines]` (`redactText`, 1289-1294); `label` placeholder (`PLACEHOLDER_N`); `error` redacted except a leading HTTP status; `opaque` replaced whole (signatures, hashes, metadata). Tool args and details are walked only for built-in tools and omp's own entries; everything else is opaque. **A key with no rule is tokenized and its value made opaque** (`#struct`, 1393-1403). One token table per run makes equal originals equal across lines and files (`#index`, 1274-1280). The rules are keyed on v1 field names (for example `filesText`, `oldText`, `diff`, `systemPrompt`, `task`).
- **Record today.** `COMMAND_REGISTRY` (`crates/app/src/cli.rs:1506`) has no recorder, player, clip or stream command; the chat palette has no `record`; nothing matches `ompcast|asciicast|StreamPaint`.
- Constraints: additive only; no vendor code; deterministic rules, no model; no second runtime (ADR 0036); roster untouched (ADR 0024); `cl_`/`sv_` convar names (ADR 0012); errors are `thiserror` enums (ADR 0035); no `pi` identity.

## Decision

**D1. `/local [name]` opens a real file in place, with the containment `local://` already has.**
- The envd `local.rs` containment code is reused, not re-implemented: a small public surface lists files for a session and resolves one name to a canonical path. The chat command calls it through a service, as `btw` does.
- *Symlinks and `..`.* The name is joined to the canonical session root, canonicalized, and must `starts_with` the root by path components (not string prefix). A symlink that leaves the root is refused. Listing skips escaping entries, as `entries` does.
- *Directories and kinds.* Only existing regular files are offered and opened. A directory name, a missing name and a special file are errors. A new file is not created by `/local`; the user writes it with `write local://name`.
- *Encoding.* The picker shows names lossily, but the chosen entry carries the exact `PathBuf`; a non-UTF-8 name opens by that path, not by its printed form. Names with control characters are shown escaped.
- *Re-validation.* After the picker returns, the canonical path is recomputed and checked again (inside the root, still a regular file) immediately before launch, so a swap between pick and launch is refused.
- *Launch.* `launch_editor` is made `pub(crate)` (additive) and used with the path as argv; the editor command stays the user's `VISUAL`/`EDITOR`. No part of the file name reaches the shell string.

**D2. Presets are a setting plus a command, applied through the picker's own write path.**
- A new `Kv` convar `ai_model_presets` (flags `archive`; not `project`): name to `{ roles: { role: selector … }, thinking: <effort> }`. Names match `^[a-zA-Z][A-Za-z0-9_-]*$`.
- *Typed leaves.* Every leaf is a string. A validator on the convar (as `ai_model_roles` has `validate_roles`) refuses a leaf that `infer` would read as Bool, Int or Float, so a preset value that looks numeric cannot change type on reload. Selectors contain `provider/model`, so real ones never trip it; the test covers `true`, `false`, `1`, `1.5`.
- *Apply.* Apply reads the current `ai_model_roles` and `ai_default_thinking`, validates the preset's values first, then writes the roles as **one whole Kv value** the way `write_role` does (`host.rs:3913-3929`), then the thinking effort, then `writecfg`. The roles change is atomic because it is one value. If the thinking write fails after the roles write, the captured previous roles value is written back; the code has no rollback (`ctx.rs:1467`), so the compensating write is the mechanism and the test asserts it.
- *Role storage.* `ai_model_role_storage` has no consumer today, so apply does **not** honour it: it writes where the picker writes (the global `config.cfg` through `writecfg`). That matches today's behaviour and is stated, not hidden. If role storage later gets a consumer, apply follows it in the same PR (O3).
- *Shadowing.* After applying, the effective value of each role and of the thinking effort is re-read and any that differs from the preset (for example a project overlay, `archive | project` on `ai_model_roles`) is reported by name.
- *Save, list, delete.* Save reads the same two variables and writes one `ai_model_presets` entry as a whole-Kv write plus `writecfg`; list and delete likewise.
- `:auto` survives because roles are stored as selectors; the thinking effort cannot hold `auto`, and a preset stores it as absent in that case.

**D3. Anonymized export is a dedicated allowlist anonymizer over journals. It never reuses `create_bundle`.**
- *Why not the bundle.* `create_bundle` ships the raw journal, logs, profiles, settings and system facts through a token-shape scrub (Context). The anonymizer emits only anonymized journal data plus a manifest (entry counts, tool version, the review note). It includes no logs, profiles, settings or system facts.
- *Placement and input.* A pure module in `omp-session` over entries read by `Journal::scan` (read-only, lock-free; the current live session can be exported). The output is a zip of `session.anon.jsonl` (one anonymized entry per line, same `id, kind, by, prior, label, data` envelope) and is **not** a resumable journal.
- *Default deny, as upstream.* Every field that leaves has an explicit rule. A field, prop, tag, kind or payload with no rule is replaced by an opaque marker under a tokenized key. A journal kind outside the closed vocabulary is opaque whole.
- *Rules.*
  - Envelope: `id`, `by`, `prior` kept (machine-minted ids); `kind` kept; `label` kept only if it is in an explicit table of code-chosen labels (for example `file.mention`), else a placeholder.
  - Numbers, booleans and null are kept (counts, sizes, timings), except under an opaque rule.
  - Identifiers (`model`, `provider`, `route`, `stop_reason`, tool names) are kept only when built-in and matching `^[\w.:/@+-]{1,128}$`; otherwise tokenized, as upstream does.
  - Paths (`Genesis.cwd`, `MentionedFile.path`, `SkillPrompt.path`, any path prop) become mock paths: every segment tokenized except `.`, `..`, `~`; extension, scheme and read selector kept. Home-directory and username segments are therefore tokenized like any other.
  - Text (`stream.text`, `MsgUser.text`, node content, `compaction.summary` and `warning`, advisor and IRC `body`, skill `args` and `prompt_body`) becomes a size-annotated marker `[redacted #N: C chars, L lines]`.
  - **File bodies.** `MentionedFile.content` and the duplicated `<file>` body in the DOM `User` node (`session.rs:752-757`) are replaced by the marker; the path is mocked, the `state` kept.
  - Tool calls: the tool name is kept if built-in; `args` and the intent `i` are opaque. Tool results and updates: keep the exit status and numeric fields; everything else is opaque.
  - Blobs and attachments: the hash is dropped; `mime` and size are kept.
  - `patch` ops: parse as `Vec<Op>`; keep op kind, handles and structural ids; keep a node tag only if it is a known tag; a prop is kept only through a per-`PropId` rule (status and duration kept, text props to marker, `Data` JSON only through the typed-payload rules above); an unknown prop or custom key is opaque under a tokenized key.
  - Receipts: token counts, costs and times kept; `identity` by the identifier rule.
- *One token table per run*, shared across every entry and any subagent journal, so equal originals map to equal tokens.
- *Output note.* The manifest and the command output say the redaction is rule-based, not a guarantee, as `up:session/session-anonymizer.ts:26` does.
- *Surfaces.* `omp anonymize [session] [-o dir]` and `/dump anon` (a new subcommand; plain `/dump` is unchanged). Source resolution reuses ADR 0056's resolver once it lands; until then an id or a path under `sessions_dir`.

**D4. Screen recording is proposed, not scheduled.** omp2 has no `omp stream` frame model, so `/record` and `omp play` need a frame source and a redactor first. Until the owner decides (O1), the documented fallback is an external recorder. The design if wanted: an omp-owned frame format (JSON Lines, header plus `[ms, frame]`), a redactor before any frame is written, then `/record` and `omp play`.

**D5. Every item is off until enabled.** Each ships behind a default-off convar. No item adds a roster tool or changes an existing tool, command, schema or convar's flags.

## Options rejected

- **Reuse `create_bundle` for `/dump anon`.** It carries the raw journal, logs, profiles, settings and system facts and scrubs only token shapes.
- **Port the upstream rules as written.** They are keyed on v1 field names; omp2's content is in DOM ops and typed `Data` payloads. The principle (default deny, one token table, size-annotated markers, mock paths) is kept.
- **A per-kind allowlist.** The twelve kinds hide most content inside `patch` ops, so the rules must go down to op, tag and prop.
- **Extend `redact_sensitive_credentials`.** A credential scrub keeps conversation content; it is a different guarantee.
- **`/local` through `edit_draft`.** It copies through a temp file and would need a write-back.
- **Write a preset into the `ai_model_role_storage` scope.** Nothing reads that scope today; apply writes where the picker writes.
- **Several statements for apply.** `exec` has no rollback; one whole-Kv value for the roles is the unit of atomicity.
- **Presets in project overlay files.** No atomic switch.
- **Project-defined presets (upstream allows them).** `ai_model_roles` already accepts a project overlay; a second layer adds rules for no capability. Recorded as O4.
- **Port `omp stream` and `omp clip` with `/record`.** Server and upload features outside this gap.
- **A model to anonymize.** Rules first; a model cannot be audited for what it leaks.

## Consequences

- Users get an editor shortcut, one-step model switching and a safer way to share a session.
- The anonymizer's guarantee is only as good as its rules, and any new prop, tag, kind or payload is opaque until a rule exists. That is deliberate. Because the rules go down to DOM props, they must track `PropId` additions; a test fails when a `PropId` has no rule.
- Preset apply writes the global config, as the picker does, and does not honour role storage.
- A recording is worth nothing without the redactor; D4 makes the redactor a prerequisite.

## Slices

One PR each, in this order: L, P, A, R. Convars: `cl_local_command_enabled`, `cl_modelpreset_enabled`, `cl_anonymize_enabled`, `cl_record_enabled` (all bool, default false). No `cap-*` feature: no new heavy dependency (recording may need one for a frame encoder; then `cap-record`, decided in R1).

| Slice | Content | Acceptance | Tests |
|---|---|---|---|
| L1 | `/local` over the `local.rs` containment; `launch_editor` made `pub(crate)` | D1 rules hold | Symlink inside the root pointing out is refused at pick and at launch; swap of a file for an escaping symlink between pick and launch is refused; `..` and a directory name refused; a missing name errors and creates nothing; a non-UTF-8 file name opens by exact path; a name with spaces, quotes and `$` reaches a fake editor's `$1` unchanged; edit then `read local://name` shows the change |
| P1 | `ai_model_presets` Kv, validator and pure save/apply | Apply is one roles value then thinking, with a compensating restore; values validated first | Round trip; a failing thinking write restores the previous roles; `true`, `false`, `1`, `1.5` leaves refused; `:auto` selector preserved; apply does not read `ai_model_role_storage`; a project overlay that shadows a role is reported by name |
| P2 | `/modelpreset` verbs list, save, switch, delete, and picker rows | Four verbs work; absent when the convar is off | Command parse table; switch changes `ai_model_roles` and writes `config.cfg`; list shows the active preset |
| P3 | v1 importer maps `modelPresets` | A v1 preset becomes an omp2 preset; an unmappable one is reported, not dropped silently | Import fixture with a valid and an unmappable preset |
| A1 | Pure anonymizer in `omp-session` | D3 rules; default deny; one token table | Golden output per kind and per `PropId`; a test fails when a `PropId` or a label literal in `crates/session/src` has no rule; unknown kind, tag, prop and custom key are opaque; the canary set below does not survive |
| A2 | `omp anonymize [session] [-o dir]` | Reads with `Journal::scan`; writes the zip; prints the review note | Source bytes unchanged; output contains no canary |
| A3 | `/dump anon` | Subcommand writes the zip; `/dump` without args unchanged | Existing `/dump` test (`crates/chat/tests/pi_commands.rs`) passes |
| R1 | Frame source and redactor | A seeded secret never reaches a frame | Redactor table; frame golden |
| R2 | `/record` toggle and writer | Header plus timed frames; a truncated last line is ignored on read | Writer then reader round trip; kill mid-write test |
| R3 | `omp play [file] [-s] [-i]` | Needs a TTY; defaults to the newest recording | Idle-limit and speed tests with a fake clock |

**Canary set for A1-A2.** A journal built with: a username-bearing path (`/Users/alice/work/payroll/secret.rs`) in `Genesis.cwd`, in a `tool.call` arg, in a shell command argument and in a file mention path; a mentioned file whose body contains the string `CANARY-FILE-BODY`; an `Entry.label` of `CANARY-LABEL`; a user message, assistant stream text, a compaction summary and an IRC body each containing a distinct canary; an attachment with a known blob hash; a custom node prop and an unknown payload with canaries. The test scans every byte of the output for every canary, the username, every path segment and the blob hash hex, and requires none.

## Open questions

- **O1.** Does the owner want screen recording at all? Recommendation: defer R1-R3 and document an external recorder.
- **O2.** Subagent journals in the export: how does the exporter find them? A source session records `<subagent>` nodes with `session_path` (`ChildResult.session_path`), but the lookup from an entry to a child journal was not traced. Recommendation: A1-A3 export the main journal only; a later slice adds subagents under the same token table.
- **O3.** If `ai_model_role_storage` ever gets a consumer, should apply follow it? Recommendation: yes, in the same PR that adds the consumer.
- **O4.** Allow project-defined presets as upstream does? Recommendation: no (see Options rejected).
- **O5.** Does the v1 importer warn today when `modelPresets` is present? Not found by grep of `crates/driver/src/v1_import`; P3 settles it.

Settled by the code, not open: `auto` thinking survives presets through role selectors (D2); `omp local` does not collide with `/local`; what a journal holds and where free text lives (D3).

## Acceptance tests

1. `/local` never opens a path outside the session local root, including through a symlink or a swap after the picker.
2. Applying a preset writes the roles as one value; a failing thinking write restores the previous roles; role storage is not read.
3. The anonymized output contains none of the canary set for any input, and every unknown field, prop, tag and kind is opaque.
4. `/dump` with no argument behaves as today, and `create_bundle` is not called by the anonymizer.
5. With each convar off the command is absent and no existing command, tool, schema or convar flag changes.

## Status in omp

**Not started.** Reusable: `local://` and its containment code, `launch_editor`, `ai_model_roles`, the picker's `write_role` write path, `Journal::scan`, the existing `/dump` command. Unverified: the frame source for recording (the TUI's frame buffer was not examined); how an entry maps to its subagent journals (O2).

## Review (EGM, 2026-11-13)

Zoom-out and design critic findings, and what changed:
- The preset premise was false: `ai_model_role_storage` has no consumer, and `exec` has no rollback. Changed: apply reuses the picker's whole-Kv write (one value), restores the old roles on a later failure, does not honour role storage and says so (D2).
- `infer` turns bare `true`, `1`, `1.5` into non-strings. Changed: a validator on `ai_model_presets` refuses such leaves, with tests (D2, P1).
- `/dump anon` must not use `create_bundle` (raw journal, logs, profiles, settings, system facts with the username, token-shape scrub only). Changed: a dedicated anonymizer that emits only anonymized journal data (D3).
- The first rule set was per kind and missed file bodies in `patch` entries and `Entry.label`. Changed: rules go down to op, tag, prop and typed payload; file bodies, labels, tool args and results, blob hashes and paths are covered, with a canary test that includes a username-bearing path and a mentioned file (D3, A1).
- Upstream's anonymizer was read (rules, token table, marker format, default-deny for unknown keys) and the principles carried over; its v1 field-name keys are not (Context, Options rejected).
- `/local` now carries the containment, encoding, directory and post-picker re-validation rules that `local://` already enforces, and launches through `launch_editor` with the path as argv (D1).
- `/record` stays deferred (O1).

## Upstream origin and license

Draws on `up:slash-commands/builtin-session.ts`, `builtin-modes.ts`, `builtin-collaboration.ts`, `builtin-control.ts`; `up:config/model-presets.ts`; `up:session/session-anonymizer.ts`; `up:stream/recording.ts`, `redactor.ts`; `up:commands/play.ts`. License: MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." (upstream `LICENSE`). omp2 re-implements the rules in Rust against its own schema; if the `.ompcast` format description or any rule text is copied, keep the notice and list it in `THIRD-PARTY-NOTICES.txt`.

## References

ADR 0006; ADR 0012 (convars); ADR 0024; ADR 0035; ADR 0036; ADR 0049; ADR 0056 (source resolution); `crates/envd/src/tool_url/local.rs`; `crates/chat/src/editor.rs`; `crates/chat/src/host.rs`; `crates/catalog/src/settings.rs`; `crates/con/src/script.rs`; `crates/journal/src/data.rs`; `crates/session/src/session.rs`; `crates/app/src/diagnostics.rs`; research note [session-tools](../research/session-tools.md).
