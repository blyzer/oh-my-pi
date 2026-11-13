# Gap 10: session tools (`session-tools`)

Scope: `/anon`, `/record` and `omp play`, `/local`, `/forget-approvals`, model presets.
Upstream: can1357/oh-my-pi `c308970263`, paths under `packages/coding-agent/src` (`up:`). omp2: branch `docs/upstream-gap-research` at `f4186b783b`.
Classes: **a** already covered, **b** not needed, **c** port, **d** propose. Decision record: [ADR 0057](../adr/0057-session-tools.md) (c and d items only).

| Item | Class | One line |
|---|---|---|
| `/anon` | **d** | Upstream spells it `/dump anon` plus `omp anonymize`; omp2 has a credential-redacted bundle but no content-anonymized export |
| `/record` | **d** | Screen recorder to `.ompcast`; nothing in omp2 |
| `omp play` | **d** | Player for `/record` files; goes with `/record` |
| `/local` | **c** | `local://` exists; the open-in-editor command does not |
| `/forget-approvals` | **b** | omp2 has no persisted approval cache to forget |
| Model presets | **c** | omp2 has `ai_model_roles` and `ai_default_thinking`; no named snapshots or command |

## 1. `/anon`

**Upstream.** There is no top-level `/anon`. `/dump anon` (`up:slash-commands/builtin-collaboration.ts:242`, under `name: "dump"` at 231) writes "a zip of anonymized session JSONL (contents redacted, metadata kept) plus subagents"; `/dump all` writes the raw zip. `omp anonymize [session] [-o dir]` (`up:cli-commands.ts:61`) does the same from the shell. The engine `up:session/session-anonymizer.ts` is an **allowlist**: every field is exported by an explicit rule; usage, timing, models, ids and tool names are kept; turn contents become size-annotated markers; paths become mock paths; shell commands keep program names and flags; a field with no rule becomes an opaque marker under a tokenized key; one token table per instance maps equal originals to equal tokens. `ANONYMIZED_REVIEW_NOTE` (line 26) says the redaction is rule-based, not a guarantee. A user needs it to share a session for a bug report without the content.

**omp2.**
- `/dump` (`crates/chat/src/commands/control.rs:48`, handler `fn dump` at 161) copies the transcript plus the LLM-request sidecar path to the clipboard; it takes no `all`/`anon` argument.
- The debug "Report: dump session" (`crates/chat/src/overlays/info.rs:69`; `crates/app/src/chat_services/debug.rs:35,84`) calls `diagnostics::create_bundle` (`crates/app/src/diagnostics.rs:125`): a size-bounded tar.gz with the live journal as `session/session.jsonl`, artifacts, same-day logs, a sanitized settings dump and raw SSE frames. Text members pass through `redact_sensitive_credentials` (diagnostics.rs:241), a credential-pattern scrub; conversation content is kept. That matches upstream's raw `/dump all`, not `/dump anon`.
- No anonymizer: no `anonymi` string in `crates/app/src/chat_cmd.rs`, `crates/chat/src/overlays/services.rs` or `crates/dom/src/redact.rs`; `COMMAND_REGISTRY` (`crates/app/src/cli.rs:1506`) has no `anonymize`.

**Class d.** The rules are written against the v1 JSONL entry schema; omp2 journals use a closed kind vocabulary (`crates/journal/src/kind.rs`: `journal, turn.start, msg.user, msg.assistant.start, stream, msg.assistant.end, tool.call, tool.update, tool.result, turn.receipt, patch, compaction`), so the allowlist is re-derived per kind. A kind with no rule must become an opaque marker (fail closed).
**Options:** (A) port the TypeScript rules as is: rejected, they target another schema; (B) per-journal-kind allowlist in Rust, same principle (chosen); (C) extend `redact_sensitive_credentials`: rejected, a credential scrub is not an anonymizer.
**Plan (default-off `cl_anonymize_enabled`):** S1 `omp-journal`-level allowlist function over entries with golden tests per kind and an unknown-kind test; S2 `omp anonymize` CLI (`-o`) reading with `Journal::scan`; S3 `/dump anon` subcommand writing a bundle.

## 2. `/record` and `omp play`

**Upstream.** `/record` toggles recording (`up:slash-commands/builtin-control.ts:103`). The format `up:stream/recording.ts` (`.ompcast`, line 31) is JSON Lines, asciicast-like: header `{ompcast:1, cols, rows, title, createdAt}`, then `[ms, frame]` lines of the single-pane `omp stream` feed, normalized and redacted (`up:stream/redactor.ts`); a truncated last line is ignored. `omp play [file] [-s speed] [-i idle-limit]` (`up:cli-commands.ts:205`, `commands/play.ts`) needs a TTY and defaults to the newest recording; `omp clip` (`cli-commands.ts:200`) uploads one. Users need it for demos and reproducible bug reports.

**omp2.** No recorder, player, clip or stream command: `COMMAND_REGISTRY` (`cli.rs:1506`) lists `local, ext, install, images, config, ps, read, search, shell, token, update, registry, join, share, collab, attach, models, worktree, stats, gc, gallery, git, usage, bench, dry-balance, tiny-models, setup, say, grievances, rules, trust, ssh, cleanse, completions, compress`; the chat palette has no `record`. Nothing matches `ompcast|asciicast|StreamPaint`. Neighbours: `omp share`, `collab`/`join`, `gallery`.

**Class d for both** (`omp play` has no meaning without `/record`). Prerequisites: a frame source from the omp2 TUI (not examined) and a redactor; frames contain secrets. **Options:** (A) port the format and encoder: needs the `omp stream` frame model first, which omp2 lacks; (B) own format on the existing TUI frame buffer; (C) do nothing and document an external recorder (asciinema). **Choice:** C is the fallback; B is the proposal, gated on a design that names the frame source. **Plan (default-off `cl_record_enabled`):** S1 frame source and redactor with tests that a seeded secret never reaches a frame; S2 `/record` toggle and file writer; S3 `omp play`.

## 3. `/local`

**Upstream.** `up:slash-commands/builtin-session.ts:606`: "Open a session local:// file in $EDITOR (picker when no name given)".
**omp2.** `local://` exists: `crates/envd/src/tool_url/local.rs` (`session_local_root` 29, `migrate_session_artifacts` 170, which moves it across `/fork` and `/resume`), registered at `crates/envd/src/tool_url.rs:689`. An editor round trip exists for the composer draft: `crates/chat/src/editor.rs` (`resolve_editor_command` 107, `edit_draft` 137; `VISUAL`/`EDITOR`). No `/local` slash command. `omp local` in `COMMAND_REGISTRY` is local inference (`Command::Local(LocalArgs)`, cli.rs:1141), unrelated; the slash name does not collide.
**Class c.** **Options:** (A) reuse `edit_draft`: it copies through a temp file, so it must write back; (B) open the real file in place (chosen: one source of truth). **Plan (default-off `cl_local_command_enabled`):** one PR: list files under the session local root, picker, open in the resolved editor, refuse paths outside the root. Tests: picker lists only the root; a `..` name is refused; the edit lands in the file read back by `local://`.

## 4. `/forget-approvals`

**Upstream.** `up:slash-commands/builtin-control.ts:122` calls `clearApprovalGrants` (`up:tools/approval-cache.ts:200`) on `approval-cache.db` (line 19), the store behind `tools.approvalMode: always-ask-cached`.
**omp2.** `ApprovalMode` (`crates/contract/src/posture.rs:30`) has `AlwaysAsk`, `Write`, `Yolo` (default): no cached mode and no persisted file/folder grant store (no `always.ask.cached` in `crates`). What is remembered is in-memory network endpoints, revoked by `SessionGrants::revoke` (`crates/envd/src/lib.rs:1842`) and `revoke_approval_grants` (`crates/envd/src/server/session.rs:1271`), which `RevokeSessionGrants` (`crates/driver/src/headless/kernel.rs:536`) triggers on session change.
**Class b.** Nothing persists, so nothing to forget; session grants clear on switch or rewind. If a cached approval mode is ever ported, this command ships with it. Not checked: a mid-session user revoke of network grants.

## 5. Model presets

**Upstream.** `up:config/model-presets.ts`: named snapshots of `modelRoles` and `defaultThinkingLevel`. `saveModelPreset` (104) and `applyModelPreset` (321); stored at `modelPresets.<name>` in global config; project config may define presets that are listed and applied but never copied to global; applying writes roles into the `modelRoleStorage` scope and reports roles still decided by another layer. Names match `/^[a-zA-Z][\w-]*$/`. Surfaces: `/modelpreset [list|save|switch|delete] [name]` (`up:slash-commands/builtin-modes.ts:801`) and the model hub.
**omp2.** State exists: `AI_MODEL_ROLES` (`crates/catalog/src/settings.rs:1131`, legacy `modelRoles`), used by `crates/chat/src/host.rs:225` and watched by `crates/agent/src/model_watch.rs`; `AI_DEFAULT_THINKING` (settings.rs:1191, a `ThinkingEffort`) and `AI_THINKING_CEILING` (1203). No presets (nothing matches `modelPresets|model_presets` outside STT and status-line presets).
**Auto thinking, settled.** `ThinkingEffort` (`crates/catalog/src/thinking.rs:36`) is `Off, Minimal, Low, Medium, High, XHigh, Max`: no `Auto`. But `auto` is a valid role-selector suffix (`crates/catalog/src/selection.rs:1132`, and the test at 1160 "non_default_role_retains_explicit_auto_thinking"). A preset that stores the role selectors keeps `:auto`; only `ai_default_thinking` cannot hold it.
**Class c.** **Options:** (A) new setting `ai_model_presets` Kv + `/modelpreset` (chosen); (B) rely on cfg overlay files (`.omp/<name>.cfg`): no atomic switch. **Plan (default-off `cl_modelpreset_enabled`):** S1 `ai_model_presets` Kv and pure save/apply functions with the layering rule; S2 `/modelpreset list|save|switch|delete`; S3 v1 importer maps `modelPresets` (today dropped: not checked for a warning). Tests: apply is atomic over roles and thinking; a project preset is never copied to global; a role decided by another layer is reported.

## Open questions

- Does the owner want screen recording at all (d items)? Recommendation: defer; document an external recorder.
- `/anon`: which journal kinds carry free text beyond the twelve listed? Recommendation: treat all as opaque except usage, timing, model ids.

## Upstream origin and license

Files drawn on: `up:slash-commands/builtin-collaboration.ts`, `builtin-control.ts`, `builtin-session.ts`, `builtin-modes.ts`, `session/session-anonymizer.ts`, `stream/recording.ts`, `stream/redactor.ts`, `commands/play.ts`, `config/model-presets.ts`. License MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." Rules and formats are re-implemented in Rust; keep the notice if any text or the `.ompcast` format description is copied.
