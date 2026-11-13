# 0057. Session tools: local files in the editor, model presets, anonymized export, screen recording

Status: proposed
Date: 2026-11-13
Area: tools

Research: [session-tools](../research/session-tools.md). Ledger gap 10. This record covers the class c and d items only (`/local`, model presets, `/anon`, `/record` and `omp play`). `/forget-approvals` is class b (omp2 persists no approvals) and is explained in the research note.

## Context

- Upstream OMPv1 (can1357/oh-my-pi `c308970263`, paths under `packages/coding-agent/src`, written `up:`) has four session features omp2 lacks:
  - `/local` opens a session `local://` file in `$EDITOR` with a picker (`up:slash-commands/builtin-session.ts:606`).
  - `/modelpreset [list|save|switch|delete]` (`up:slash-commands/builtin-modes.ts:801`) over `up:config/model-presets.ts`: named snapshots of the role assignments and the default thinking level (`saveModelPreset` 104, `applyModelPreset` 321).
  - `/dump anon` (`up:slash-commands/builtin-collaboration.ts:242`) and `omp anonymize` (`up:cli-commands.ts:61`) over an allowlist anonymizer (`up:session/session-anonymizer.ts`): every field is exported by an explicit rule, turn contents become size-annotated markers, and a field with no rule becomes an opaque marker.
  - `/record` (`up:slash-commands/builtin-control.ts:103`) writes a `.ompcast` file (`up:stream/recording.ts:31`, JSON Lines, asciicast-like) and `omp play` replays it (`up:cli-commands.ts:205`).
- `/local`: `local://` exists (`crates/envd/src/tool_url/local.rs:29,170`, registered at `crates/envd/src/tool_url.rs:689`). An editor round trip exists for the composer draft only (`crates/chat/src/editor.rs:107,137`). `omp local` (`crates/app/src/cli.rs:1141`) is local inference and does not collide with a slash command.
- Presets: the state exists. `ai_model_roles` is a `Kv` with flags `archive | project` (`crates/catalog/src/settings.rs:1131-1138`); `ai_default_thinking` is a `ThinkingEffort` (1191); `ai_model_role_storage` says where role assignments are saved (1140). The model picker already assigns roles and saves them (`crates/chat/src/host.rs:3877`). `ThinkingEffort` has no `Auto` (`crates/catalog/src/thinking.rs:36`), but `:auto` is a valid role-selector suffix (`crates/catalog/src/selection.rs:1132`), so role selectors carry it.
- Anonymize: `/dump` only copies the transcript and the request sidecar path (`crates/chat/src/commands/control.rs:48,161`). The debug report builds a bundle through `create_bundle` (`crates/app/src/diagnostics.rs:125`); text members pass through `redact_sensitive_credentials` (241), a credential scrub that keeps conversation content. There is no anonymizer. Journals have a closed kind vocabulary (`crates/journal/src/kind.rs`: `journal, turn.start, msg.user, msg.assistant.start, stream, msg.assistant.end, tool.call, tool.update, tool.result, turn.receipt, patch, compaction`), not the v1 JSONL schema the upstream rules target.
- Record: `COMMAND_REGISTRY` (`crates/app/src/cli.rs:1506`) has no recorder, player, clip or stream command and the chat palette has no `record`. Nothing matches `ompcast|asciicast|StreamPaint`.
- Constraints: additive only; no vendor code; deterministic rules, no model; no second runtime (ADR 0036); roster untouched (ADR 0024); `cl_`/`sv_` convar names (ADR 0012); no `pi` identity (the `.ompcast` name is `omp`).

## Decision

**D1. `/local [name]` opens the real file in place.** It lists files under the session local root (`session_local_root`, `local.rs:29`), asks for a pick when no name is given, and opens the chosen file in the editor from `resolve_editor_command` (`VISUAL`, then `EDITOR`). A name that resolves outside the root is refused. It is a chat command, not a tool.

**D2. Presets are a setting plus a command.** A new `Kv` convar `ai_model_presets` (flags `archive` only) maps a name to a role-selector table and a thinking effort. `/modelpreset list|save|switch|delete` and the model picker use pure save and apply functions. Apply writes the roles through the same path as the picker's `assign_role`, into the scope `ai_model_role_storage` selects, in one atomic change, and reports any role a higher layer still decides. Names match `^[a-zA-Z][A-Za-z0-9_-]*$`. Because roles are stored as selectors, `:auto` survives; only the default thinking effort cannot hold `auto`, and a preset stores it as absent in that case.

**D3. Anonymized export is a per-kind allowlist over journals.** A pure function maps each journal entry to an anonymized entry by rule per kind; usage, timing, model ids and tool names stay; message and tool text become size-annotated markers; paths become mock paths; an entry of a kind with no rule becomes an opaque marker under a tokenized key. One token table per run keeps equal originals equal. Reading is `Journal::scan`, so a live journal is safe. The output says it is rule-based, not a guarantee, as upstream does (`up:session/session-anonymizer.ts:26`). Surfaces: `omp anonymize [session] [-o dir]` and `/dump anon`. `/dump` keeps its current behaviour when no argument is given.

**D4. Screen recording is proposed, not scheduled.** omp2 has no `omp stream` frame model, so `/record` and `omp play` need a frame source and a redactor first. The proposal is an omp-owned frame format (JSON Lines, header plus `[ms, frame]`), a redactor that runs before any frame is written, and `/record` and `omp play` on top. Until the owner decides (O1), the documented fallback is an external recorder.

**D5. Every item is off until enabled.** Each ships behind a default-off convar. No item adds a roster tool or changes an existing tool, command, schema or convar.

## Options rejected

- **Port the upstream anonymizer rules as is.** They target the v1 JSONL schema; omp2 journals have different kinds. The principle ("allowlist, never denylist") is kept.
- **Extend `redact_sensitive_credentials` into an anonymizer.** A credential scrub keeps conversation content; it is a different guarantee.
- **`/local` through `edit_draft`.** It copies through a temp file and would need a write-back; opening the real file keeps one source of truth.
- **Presets in project overlay files.** No atomic switch, and a preset is a bulk write of a setting.
- **Project-defined presets (upstream allows them).** `ai_model_roles` already accepts a project overlay (`archive | project`); a second, listed-but-never-copied layer adds rules for no new capability. Recorded as O3.
- **Port the `omp stream` and `omp clip` family with `/record`.** `stream` and `clip` are server and upload features outside this gap.
- **A model to anonymize.** Rules first; a model cannot be audited for what it leaks.

## Consequences

- Users get an editor shortcut, one-step model switching and a safer way to share a session.
- The anonymizer's guarantee is only as good as its per-kind rules, and a new journal kind is opaque until a rule exists. That is deliberate.
- A recording is worth nothing without the redactor; D4 makes the redactor a prerequisite.

## Slices

One PR each, in this order: L, P, A, R. Convars: `cl_local_command_enabled`, `cl_modelpreset_enabled`, `cl_anonymize_enabled`, `cl_record_enabled` (all bool, default false). No `cap-*` feature: no new heavy dependency (recording may need one if a frame encoder is added; then `cap-record`, decided in R1).

| Slice | Content | Acceptance | Tests |
|---|---|---|---|
| L1 | `/local` | Picker lists only files under the local root; the edit lands in the file `local://` reads back | A `..` name is refused; picker lists only the root; edit then `read local://name` shows the change |
| P1 | `ai_model_presets` Kv and pure save/apply | Apply is atomic over roles and thinking; a role set by a higher layer is reported; a bad name is refused | Round trip; atomicity under a failing write; `:auto` selector preserved; name table |
| P2 | `/modelpreset` verbs list, save, switch, delete, and picker rows | Four verbs work; absent when the convar is off | Command parse table; switch changes `ai_model_roles`; list shows the active preset |
| P3 | v1 importer maps `modelPresets` | A v1 preset becomes an omp2 preset; an unmappable one is reported, not dropped silently | Import fixture with a valid and an unmappable preset |
| A1 | Per-kind allowlist function (pure) | Every journal kind has a rule or the opaque marker; paths mock; tokens stable | Golden file per kind; unknown-kind entry becomes opaque; a seeded secret in every free-text field does not survive |
| A2 | `omp anonymize` | Reads with `Journal::scan`; writes `-o` directory; the review note is printed | Source bytes unchanged; output contains no seeded literal |
| A3 | `/dump anon` | Subcommand writes the bundle; `/dump` without args unchanged | Existing `/dump` test (`crates/chat/tests/pi_commands.rs`) still passes |
| R1 | Frame source and redactor | A seeded secret never reaches a frame | Redactor table; frame golden |
| R2 | `/record` toggle and writer | Header plus timed frames; a truncated last line is ignored on read | Writer then reader round trip; kill mid-write test |
| R3 | `omp play [file] [-s] [-i]` | Needs a TTY; defaults to the newest recording | Idle-limit and speed tests with a fake clock |

## Open questions

- **O1.** Does the owner want screen recording at all? Recommendation: defer R1-R3 and document an external recorder. D4 stays as the design if it is wanted.
- **O2.** Which journal free-text fields exist beyond the twelve kinds listed (for example tool arguments inside `tool.call`)? The code reads them as opaque JSON, so the answer is a rule per tool. Recommendation: treat all tool arguments and results as opaque except tool name and exit status.
- **O3.** Allow project-defined presets as upstream does? Recommendation: no (see Options rejected).
- **O4.** Does the v1 importer warn today when `modelPresets` is present? Not found by grep of `crates/driver/src/v1_import`; P3 settles it.

Settled by the code, not open: `auto` thinking survives presets through role selectors (D2); `omp local` does not collide with `/local`.

## Acceptance tests

1. `/local` never opens a path outside the session local root.
2. Applying a preset changes roles and thinking in one change; a failure leaves both unchanged.
3. The anonymizer's output contains none of a set of seeded secrets, paths or message texts, for every journal kind, and an unknown kind is opaque.
4. `/dump` with no argument behaves as today.
5. With each convar off the command is absent and no existing command, tool or schema changes.

## Status in omp

**Not started.** Reusable: `local://` and its root, `resolve_editor_command`, `ai_model_roles` and the picker's `assign_role`, `Journal::scan`, `create_bundle` and its credential redaction. Unverified: the frame source for recording (the TUI's frame buffer was not examined); whether the debug bundle is the right carrier for `/dump anon`.

## Upstream origin and license

Draws on `up:slash-commands/builtin-session.ts`, `builtin-modes.ts`, `builtin-collaboration.ts`, `builtin-control.ts`; `up:config/model-presets.ts`; `up:session/session-anonymizer.ts`; `up:stream/recording.ts`, `redactor.ts`; `up:commands/play.ts`. License: MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." (upstream `LICENSE`). omp2 re-implements the rules in Rust; if the `.ompcast` format description or any rule text is copied, keep the notice and list it in `THIRD-PARTY-NOTICES.txt`.

## References

ADR 0006; ADR 0012 (convars); ADR 0024; ADR 0036; ADR 0049; `crates/envd/src/tool_url/local.rs`; `crates/chat/src/editor.rs`; `crates/catalog/src/settings.rs`; `crates/journal/src/kind.rs`; `crates/app/src/diagnostics.rs`; research note [session-tools](../research/session-tools.md).
