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

**Class d.** Two points from the EGM review change the shape. First, `create_bundle` is not a base: it ships the raw journal, logs, profiles and settings, scrubbed only by a token-shape filter (`crates/observability/src/redact.rs:27`), and its system facts carry the working directory and `SHELL` with the username (`crates/app/src/debug.rs:35-50`). Second, the content is not in the journal kinds but inside them: `patch` ops carry DOM nodes and typed `Data` payloads (`FileMentions` with file bodies, `MsgUser`, `AsyncJobDelivery`, `SkillPrompt`, advisor and IRC bodies; `crates/journal/src/data.rs`, `crates/session/src/session.rs:739-765`), `stream` and `compaction` carry text, and every entry has a free-text `label` (`crates/journal/src/entry.rs:79`). So the rules go down to op, tag, prop and typed payload, default deny, with file bodies, labels, tool args and results, blob hashes and paths all covered.
**Upstream rules, read.** `FIELD_RULES` (`up:session/session-anonymizer.ts:28-60`): numbers and times kept; identifiers kept only in identifier shape; `text` becomes `[redacted #N: C chars, L lines]`; `label` a placeholder; `path` mocked per segment keeping extension; `opaque` replaced whole; a key with no rule is tokenized and its value made opaque (`#struct`, 1393-1403); one token table per run. The principles carry over; the v1 field-name keys do not.
**Options:** (A) port the TypeScript rules as is: rejected, v1 field names; (B) a dedicated Rust anonymizer in `omp-session` over `Journal::scan`, default deny, output a zip of anonymized entries that is not a resumable journal (chosen); (C) reuse `create_bundle` or extend `redact_sensitive_credentials`: rejected.
**Plan (default-off `cl_anonymize_enabled`):** A1 the pure anonymizer with a canary test (username-bearing path, a mentioned file body, an `Entry.label`, text in every text-bearing kind, a blob hash, unknown prop and kind) and a test that fails when a `PropId` or a label literal has no rule; A2 `omp anonymize`; A3 `/dump anon`. Details in ADR 0057.

## 2. `/record` and `omp play`

**Upstream.** `/record` toggles recording (`up:slash-commands/builtin-control.ts:103`). The format `up:stream/recording.ts` (`.ompcast`, line 31) is JSON Lines, asciicast-like: header `{ompcast:1, cols, rows, title, createdAt}`, then `[ms, frame]` lines of the single-pane `omp stream` feed, normalized and redacted (`up:stream/redactor.ts`); a truncated last line is ignored. `omp play [file] [-s speed] [-i idle-limit]` (`up:cli-commands.ts:205`, `commands/play.ts`) needs a TTY and defaults to the newest recording; `omp clip` (`cli-commands.ts:200`) uploads one. Users need it for demos and reproducible bug reports.

**omp2.** No recorder, player, clip or stream command: `COMMAND_REGISTRY` (`cli.rs:1506`) lists `local, ext, install, images, config, ps, read, search, shell, token, update, registry, join, share, collab, attach, models, worktree, stats, gc, gallery, git, usage, bench, dry-balance, tiny-models, setup, say, grievances, rules, trust, ssh, cleanse, completions, compress`; the chat palette has no `record`. Nothing matches `ompcast|asciicast|StreamPaint`. Neighbours: `omp share`, `collab`/`join`, `gallery`.

**Class d for both** (`omp play` has no meaning without `/record`). Prerequisites: a frame source from the omp2 TUI (not examined) and a redactor; frames contain secrets. **Options:** (A) port the format and encoder: needs the `omp stream` frame model first, which omp2 lacks; (B) own format on the existing TUI frame buffer; (C) do nothing and document an external recorder (asciinema). **Choice:** C is the fallback; B is the proposal, gated on a design that names the frame source. **Plan (default-off `cl_record_enabled`):** S1 frame source and redactor with tests that a seeded secret never reaches a frame; S2 `/record` toggle and file writer; S3 `omp play`.

## 3. `/local`

**Upstream.** `up:slash-commands/builtin-session.ts:606`: "Open a session local:// file in $EDITOR (picker when no name given)".
**omp2.** `local://` exists: `crates/envd/src/tool_url/local.rs` (`session_local_root` 29, `migrate_session_artifacts` 170, which moves it across `/fork` and `/resume`), registered at `crates/envd/src/tool_url.rs:689`. An editor round trip exists for the composer draft: `crates/chat/src/editor.rs` (`resolve_editor_command` 107, `edit_draft` 137; `VISUAL`/`EDITOR`). No `/local` slash command. `omp local` in `COMMAND_REGISTRY` is local inference (`Command::Local(LocalArgs)`, cli.rs:1141), unrelated; the slash name does not collide.
**Class c.** **Options:** (A) reuse `edit_draft`: it copies through a temp file; (B) open the real file in place under the containment `local://` already has (chosen). The rules, from the EGM review: symlink containment (canonicalize plus component-wise `starts_with`, `crates/envd/src/tool_url/local.rs:267-279`, listings skip escaping entries at 281-310), regular files only (no directories, no creation), non-UTF-8 names open by exact path, the path is re-validated after the picker returns, and the launch passes the path as argv through `launch_editor` (`crates/chat/src/editor.rs:157`, to be made `pub(crate)`). **Plan (default-off `cl_local_command_enabled`):** one PR; tests for a symlink out, a swap between pick and launch, `..`, a directory, a missing name, a non-UTF-8 name, and a name with quotes and `$` reaching a fake editor's `$1`.

## 4. `/forget-approvals`

**Upstream.** `up:slash-commands/builtin-control.ts:122` calls `clearApprovalGrants` (`up:tools/approval-cache.ts:200`) on `approval-cache.db` (line 19), the store behind `tools.approvalMode: always-ask-cached`.
**omp2.** `ApprovalMode` (`crates/contract/src/posture.rs:30`) has `AlwaysAsk`, `Write`, `Yolo` (default): no cached mode and no persisted file/folder grant store (no `always.ask.cached` in `crates`). What is remembered is in-memory network endpoints, revoked by `SessionGrants::revoke` (`crates/envd/src/lib.rs:1842`) and `revoke_approval_grants` (`crates/envd/src/server/session.rs:1271`), which `RevokeSessionGrants` (`crates/driver/src/headless/kernel.rs:536`) triggers on session change.
**Class b.** Nothing persists, so nothing to forget; session grants clear on switch or rewind. If a cached approval mode is ever ported, this command ships with it. Not checked: a mid-session user revoke of network grants.

## 5. Model presets

**Upstream.** `up:config/model-presets.ts`: named snapshots of `modelRoles` and `defaultThinkingLevel`. `saveModelPreset` (104) and `applyModelPreset` (321); stored at `modelPresets.<name>` in global config; project config may define presets that are listed and applied but never copied to global; applying writes roles into the `modelRoleStorage` scope and reports roles still decided by another layer. Names match `/^[a-zA-Z][\w-]*$/`. Surfaces: `/modelpreset [list|save|switch|delete] [name]` (`up:slash-commands/builtin-modes.ts:801`) and the model hub.
**omp2.** State exists: `AI_MODEL_ROLES` (`crates/catalog/src/settings.rs:1131`, legacy `modelRoles`), used by `crates/chat/src/host.rs:225` and watched by `crates/agent/src/model_watch.rs`; `AI_DEFAULT_THINKING` (settings.rs:1191, a `ThinkingEffort`) and `AI_THINKING_CEILING` (1203). No presets (nothing matches `modelPresets|model_presets` outside STT and status-line presets).
**Auto thinking, settled.** `ThinkingEffort` (`crates/catalog/src/thinking.rs:36`) is `Off, Minimal, Low, Medium, High, XHigh, Max`: no `Auto`. But `auto` is a valid role-selector suffix (`crates/catalog/src/selection.rs:1132`, and the test at 1160 "non_default_role_retains_explicit_auto_thinking"). A preset that stores the role selectors keeps `:auto`; only `ai_default_thinking` cannot hold it.
**Class c.** **Options:** (A) new setting `ai_model_presets` Kv + `/modelpreset` (chosen); (B) rely on cfg overlay files (`.omp/<name>.cfg`): no atomic switch. **How apply works (corrected in the EGM review):** `ai_model_role_storage` (settings.rs:1140) has no consumer (`ModelSettings.role_storage` is assigned at 425 and read only in tests), so apply does not honour it; it writes where the picker does: the whole `ai_model_roles` Kv as one value through `exec` at `Source::Console` (`write_role`, `crates/chat/src/host.rs:3913-3929`) and then `writecfg` (3901). `Ctx::exec` has no rollback (`crates/con/src/ctx.rs:1467`), so values are validated first and a later failure restores the previous roles. A kv leaf is untyped text and `infer` reads `true`, `1`, `1.5` as Bool, Int, Float (`crates/con/src/script.rs:373-398`), so preset leaves are strings and a validator refuses such values. **Plan (default-off `cl_modelpreset_enabled`):** P1 `ai_model_presets` Kv and pure save/apply; P2 `/modelpreset` verbs; P3 v1 importer maps `modelPresets`. Details in ADR 0057.

## Open questions

- Does the owner want screen recording at all (d items)? Recommendation: defer; document an external recorder.
- How does the anonymizer find a session's subagent journals? Not traced; recommend the main journal only at first.
- If `ai_model_role_storage` gets a consumer, should preset apply follow it? Recommendation: yes, in the same PR.
- Project-defined presets as upstream allows? Recommendation: no.

## Upstream origin and license

Files drawn on: `up:slash-commands/builtin-collaboration.ts`, `builtin-control.ts`, `builtin-session.ts`, `builtin-modes.ts`, `session/session-anonymizer.ts`, `stream/recording.ts`, `stream/redactor.ts`, `commands/play.ts`, `config/model-presets.ts`. License MIT, "Copyright (c) 2025 Mario Zechner; (c) 2025-2026 Can Bölük; (c) 2026 Stencil Labs, Inc." Rules and formats are re-implemented in Rust; keep the notice if any text or the `.ompcast` format description is copied.
