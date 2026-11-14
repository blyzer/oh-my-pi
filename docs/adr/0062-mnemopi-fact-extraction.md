# 0062. Mnemopi retains main-session turns and extracts facts without a model: journal-keyed retention, English and Spanish rules, expiring facts

Status: Proposed 2026-11-14; EGM round 2 resolved, round 3 pending
Date: 2026-11-13
Area: state

Research: [docs/research/mnemopi-fact-extraction.md](../research/mnemopi-fact-extraction.md) (gap 2 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations are `path:line`; upstream paths carry the prefix `upstream:`; bare `store.rs`, `extract.rs`, `retain.rs`, `session.rs`, `runtime.rs` and `config.rs` are in `crates/memory/src/`; `kernel.rs` is `crates/driver/src/headless/kernel.rs`; `hooks.rs` is `crates/contract/src/hooks.rs`. Docs-only revision: nothing was built or run for it. The `Date:` line is the record's creation date (`docs/adr/README.md`, "Format"); the status line carries the revision date.

**Scope of this record.** Round 2 cut the scope (see "What the review changed"). This ADR covers: wiring retention for main sessions, a deterministic English and Spanish rule extractor for `llm_mode = none`, and fact expiry. Three parts of gap 2 move to a follow-up ADR, not yet numbered: (1) structured tool observations (`package.json`, `Cargo.toml`, version banners); (2) model lanes, meaning a lane over the `@memory` role and the Apple Foundation Models lane, with typed lane faults, a bounded worker and the fallback to the rules; (3) the sixth `FACT` field. The follow-up needs decisions this record does not make (D12).

## Glossary

- **Bank**: one SQLite memory database (`store.rs`), possibly shared by several omp processes (`store.rs:1`).
- **Episode**: one retained window of a session's transcript, stored as a `working_memory` row (`store.rs:512-526`).
- **Retention window**: the settled messages since the last retention. **Cursor**: the highest user-turn count already retained for a key, stored durably in `retention_cursors` (`store.rs:545-550`; `retain.rs:109-127`).
- **Working memory / consolidation**: working rows are evicted after 24 h or past 1000 rows per key (`config.rs:263-268`; `store.rs:1599-1616`). Consolidation (`MemoryRuntime::enqueue`, `runtime.rs:766-778`) promotes them to durable episodic rows.
- **Extraction job**: a row in `extraction_jobs` holding the user-authored text of one episode, written in the retention transaction (`store.rs:527-536`), deleted together with storing its facts by `complete_extraction` (`store.rs:712-760`).
- **Fact**: an immutable `(subject, predicate, object, confidence)` row in `facts`, recalled by full-text search over `subject predicate object` (`store.rs:826-850,1740-1762`).
- **`FACT` line**: the existing model output protocol `FACT<TAB>subject<TAB>predicate<TAB>object<TAB>confidence`, parsed by `parse_facts` (`extract.rs:95-136`). This record does not change it.
- **`llm_mode`**: `MnemopiSettings.llm_mode`, one of `none`, `smol` (default), `remote`, `local-memory-model` (`config.rs:113-123,306`). `none` means the user chose no memory model.
- **Memory role**: the catalog's `@memory` selector, which resolves configured memory selectors, then `@commit`, then `@smol`; reflection already uses it (`crates/driver/src/headless/reflection.rs:21-24`).
- **Lane**: an `ExtractionLane` implementation that turns a job into `FACT` lines with a model (`extract.rs:30-35`). None exists.
- **Native hook host**: an in-process `NativeHookHost` attached to the session's `HookGate`, which receives lifecycle events such as `turn_end` and `session_shutdown` (`hooks.rs:760-784`).
- **Main session**: a session the user talks to, composed through `ChatCommand::compose` (Context). Everything else that calls `compose_kernel` is auxiliary: subagents, workpools, the `/btw` side question, background "tan" jobs, the adw, cleanse and compress pipelines, and standalone tool runs.

## Context

- Upstream keeps a deterministic floor under LLM fact extraction. `heuristicExtractFacts` turns eight English first-person patterns into at most five fact sentences (`upstream:packages/mnemopi/src/core/extraction.ts:276-315`). `extractAndStoreFacts` adds metric, date, version and relation patterns (`upstream:packages/mnemopi/src/core/beam/consolidate.ts:675-783`). Both run when no model resolves (`upstream:packages/coding-agent/src/mnemopi/backend.ts:539,558-563`). Upstream's rules write key/value rows (`upstream:packages/mnemopi/src/core/beam/consolidate.ts:289-314`), keep only counters for decisions (`:781-782`), and are English only. omp2 indexes facts as `subject predicate object` text (`store.rs:1666,1751-1753`), so its rows must be real triples.
- Nothing is wired. No code outside `crates/memory/src` calls `retain_settled`, `shutdown_flush`, `pending_extractions` or `ExtractionLane` (repository search). The finalizer's Mnemopi slot has no registrant and `finalize` has no caller (`crates/driver/src/headless/finalize.rs:100-136`). `HostSettings::from_con` never sets `llm_mode`, so production is always `smol` (`crates/envd/src/host_settings.rs:204-234`). The only existing app-side consolidation trigger is `/memory enqueue` (`crates/app/src/chat_services/misc.rs:144-146`).
- In `session.rs`, both retention paths build a `Retainer` keyed by `runtime.session_id()` with `extraction_enabled = llm_mode != None` (`session.rs:145-152,221-228`). The test `disabled_extraction_lane_retains_without_enqueuing` pins "`none` enqueues no job" (`session.rs:384`).
- **The runtime's session id is not durable.** It is a fresh ULID at every environment start (`crates/envd/src/lib.rs:1612-1617`), passed to `start_memory_runtime` (`crates/envd/src/server/session.rs:39-63,351`). The cursor, the episode id and the job's `session_id` are all derived from it (`store.rs:490-499,545-550`). Retention keyed this way would retain a resumed journal's whole history again, and could never recover a crashed process's window.
- A journal has a stable name: `SessionSwitched::to` uses `journal_name(next)` and the journal path (`hooks.rs:733-747`), and `compose_kernel` already hands the plugin host the session id and journal path at construction (`kernel.rs:3081-3092`).
- Facts have no end of validity: `search_facts` returns them forever (`store.rs:826-850`). They are immutable (`store.rs:706`; `crates/envd/src/tool_url/memory.rs:195-197`). Upstream has validity on memory rows and triples, as ISO `TEXT`, but not on facts (`upstream:packages/mnemopi/src/core/beam/schema.ts:346-356,414`; `upstream:packages/mnemopi/src/core/beam/helpers.ts:200`).
- Schema migration: `SCHEMA_VERSION = 3` (`store.rs:32`). `migrate` runs in an exclusive transaction (`store.rs:1215`) and refuses a newer bank (`:1217-1219`). Column additions (`migrate_authoritative_tables` via `add_column_if_missing`) run only when `version < SCHEMA_VERSION`, together with `RESET_REBUILDABLE_INDEXES`. The reset drops the FTS tables, `memory_embeddings`, `triples` and `memory_links`, and zeroes the vector and graph generations (`store.rs:1220-1233,1642-1658`). Every reader and writer of `facts` names its columns (`store.rs:379,734-735,832-836,957-959,1666`).
- Hook surface. The kernel notifies `turn_end` after every completed inference turn (`crates/agent/src/loop.rs:1844-1858`); its payload carries no transcript (`"items": []`, `:1856`). `observe` must not block. The session's end awaits `decide(session_shutdown)` for at most `SESSION_SHUTDOWN_BUDGET` = 1500 ms (`hooks.rs:571,760-784`). `compose_kernel` attaches the plugin host to the environment's admission gate, but only `if tools_enabled` (`kernel.rs:3081-3099`). `Journal::scan` reads committed entries without the writer lock (`crates/journal/src/lib.rs:186-203`).
- Journal shapes. A user message is `msg.user@1` with `text` (`crates/journal/src/data.rs:102-104`). Assistant text is a `msg.assistant.start@1`, then `stream@1` entries that open a stream on a block node with prop `text` (thinking uses `thinking`; `crates/agent/src/loop.rs:4797`; `crates/vocab/src/lib.rs:462-465`) and append deltas, then `msg.assistant.end@1` (`crates/journal/src/data.rs:505-526`). Tool results are `tool.result@1` with raw outcome JSON and an optional CAS blob (`data.rs:552-575`).
- A Director would also see completed turns (`crates/contract/src/director.rs:686-689`). But engaging one journals a `<director>` node (`director.rs:950-975`), and `DirectorStack::from_dom` stops the active chain at the first node the registry cannot construct (`director.rs:855-857`). A session resumed with the gate off would then lose every Director engaged below it.
- Main-session owners. Interactive, print, RPC, ACP and share modes all compose through `ChatCommand::compose` (`crates/app/src/chat_cmd.rs:747-760`; callers `session_runner.rs:133`, `print_mode.rs:101`, `rpc_mode.rs:89`, `acp_mode.rs:66`, `share_cmd.rs:91`, `chat_cmd.rs:1166`). `--no-session` sets `ephemeral` on the same options (`chat_cmd.rs:602,657`). The `/btw` side question (`crates/app/src/chat_services/session_ops.rs:269-275`) and tan jobs (`crates/app/src/chat_control.rs:3064-3073`) compose their own kernels. `KernelOptions` derives `Default` and marks children with `parent_session` (`kernel.rs:138,184-185`).
- Secrets. Retention stores transcripts unredacted (`retain.rs:172-175`). `omp-secrets` ships credential-shaped rules (`crates/secrets/src/builtins.rs:18`) and a one-way `SecretRedactor` (`crates/secrets/src/redact.rs:24-37`); `omp-driver` already depends on it (`crates/driver/Cargo.toml:69`). Upstream redacts memory writes the same way (`upstream:packages/coding-agent/src/memory-backend/redact.ts:1-7`).
- Text tooling. `omp-memory` already depends on `jiff` (`crates/memory/Cargo.toml:24`). `regex` is a workspace dependency (`Cargo.toml:215`). `xutf` offers NFC checks but no decomposition or mark stripping (`xutf` 1.6.0 `src/normalize.rs:134-199`).
- Owner decisions fixed before this ADR: (1) the rule extractor runs only when no memory model is set; (2) `valid_until` is set by both the rule extractor and the model's `FACT` lines through an optional sixth field, and five-field lines stay valid; (3) recall filters expired facts in `search_facts` and read-by-id still returns them. The owner also directed that session wiring is part of this work (research note, header). Decision (2)'s model half moves to the follow-up ADR with the lanes.

## Decision

**D1. The rules run only when the user chose no memory model.** The rule extractor runs for a main session when `sv_memory_session_retention` is on and `llm_mode` is `none`. It MUST NOT run in any other mode. That is the narrow reading of owner decision (1): in `smol` the `@memory` role normally resolves to a model, so a memory model is set. It is also checkable without a lane. The rules MUST NOT call any model; this is this record's own rule. In `smol`, retention stores episodes and enqueues no extraction job, because nothing in this record could drain one (D11). The follow-up ADR adds the lanes, which serve `smol` and `local-memory-model` (D12).

**D2. Facts may expire.** `facts.valid_until INTEGER NULL` holds the exclusive end of validity in UTC epoch milliseconds. INTEGER, not upstream's ISO `TEXT`, so the comparison is exact and offset-free. In this record the only producer is the explicit-bound rule family (D5). With `sv_memory_fact_expiry` on, `search_facts` excludes facts with `valid_until <= now` in its `WHERE` clause, ahead of `LIMIT`, so expired rows cannot use up result slots; read-by-id (`store.rs:955-963`) still returns them. With the gate off no producer writes the column and `search_facts` is exactly today's. `parse_facts` is unchanged (the sixth field is D12).

**D3. The column is additive, added only while the gate is on, and `SCHEMA_VERSION` stays 3.**
- *Opening.* `BankStore` gains `BankOptions { fact_expiry: bool }` and `open_with(path, bank, root, options)`. `open` (`store.rs:193`) keeps its signature and means `BankOptions::default()`. `BankStore` keeps the option. The two production open sites pass `MnemopiSettings.fact_expiry` (`runtime.rs:285,310`). `clear()`, the v1 importer and tests keep `open`.
- *Migration.* With `fact_expiry`, `migrate` runs one new step inside its existing exclusive transaction (`store.rs:1215`), after `SCHEMA` (`:1224`) and whatever the version: `add_column_if_missing(facts, valid_until, INTEGER)` (`store.rs:1360-1371`). Concurrent first opens add it once. Without `fact_expiry`, the schema text, the migration and the bank file are exactly today's.
- *Writes.* `NewFact` gains `valid_until: Option<i64>`. `persist_extracted_facts` (`store.rs:724-760`) uses a nine-column INSERT when the store's option is on, else today's eight-column INSERT, which ignores the field.
- *Reads.* `search_facts_at(query, limit, now_ms)` is the filtered query. `search_facts` (signature unchanged, called at `crates/memory/src/recall.rs:205`) delegates to it with `jiff::Timestamp::now()` when the option is on, else runs today's query.
- *State matrix.* Column absent with gate off is today. Column present with gate on: filtered. Column present with gate off (enabled earlier): NULL for new rows, unread, so previously expired facts are recalled again; this is stated as a cost. An older binary on a bank with the column: unaffected, because every statement names its columns (Context), so it does not filter.
- *Recall cache.* The recall cache is invalidated by index generation only, so a cached result can show a fact for up to one generation after its `valid_until`; accepted.
- *Why no bump.* A bump resets every bank's derived indexes, so every user re-embeds and the graph generation resets, and it locks every older binary out (`store.rs:1217-1233,1642-1658`). Rule for later changes: a nullable column nothing else touches MAY be added this way; a change to meaning, a derived index or a constraint MUST bump.
- *Owner ruling.* This extends an existing schema that upstream does not have, so the mechanism waits for the owner (O3), and slice S4 does not merge until O3 is answered.

**D4. Rule output is typed, closed and bounded.** Triples over the closed predicate set `name`, `is`, `works_at`, `lives_in`, `uses`, `prefers`, `dislikes`, `instruction`, `version`, `depends_on`, `decided`, `rejected`, `deadline`.
- *Subjects* are `user`, a package or tool name captured by the rule, or `project`. `project` means the bank's project; with `Global` scoping it is ambiguous across projects (O6).
- *Ids* are `rule_` plus a `Hash32` prefix over source memory id, rule id and the triple, so replays are idempotent under `INSERT OR IGNORE` (`store.rs:733-737`), and provenance is visible in the id.
- *Bounds* (starting values, kept in code): a cap on facts per source memory, subject and object byte limits inside the parser's (`extract.rs:114-119`), and fixed confidence per family.
- *Timestamp.* `NewFact.timestamp` is the job's `created_at` (`store.rs:533`), read through the new queued-job type (D11).
- *Determinism.* Rules are pure functions of the job text. They read no clock: dates come from the text.
- *Precision over recall.* Questions, hypotheticals, negations, fenced code and clauses containing a secret word produce nothing.

**D5. The rule tables are data, specified here.** Rules live as `const` tables in `crates/memory/src/extract/rules/{en,es}.rs`. Each row is `Rule { id, family, pattern, subject, predicate }`, compiled once with `regex` (`(?i)`, Unicode-aware simple case folding).
- *Input.* The job text, with retention markers stripped (`strip_protocol_markers`, `retain.rs:212`) and fenced code blocks removed.
- *Clauses.* Split on `.`, `!`, `?`, `;`, newline, " and ", " but ", " y ", " pero " (upstream splits on the first six, `upstream:packages/mnemopi/src/core/extraction.ts:283`).
- *Skipped clauses.* A clause is skipped when it ends with `?` or starts with `¿`. It is skipped when it contains a hypothetical marker (`if`, `unless`, `would`, `could`, `maybe`, `si`, `a menos que`, `quizá`, `quizás`, `tal vez`, `podría`). It is skipped when it contains a secret word (`password`, `passwd`, `token`, `api key`, `apikey`, `secret`, `private key`, `contraseña`, `clave`, `secreto`, `credencial`).
- *Negation.* It is handled by adjacency: a pattern requires its verb directly after its subject, so "I don't use X" or "no uso X" cannot match "use X" or "uso X".
- *Accents.* Accent-insensitive matching is written into the patterns as explicit alternations (`l[ií]mite`, `versi[oó]n`). No Unicode normalization crate is added (AGENTS.md "Unicode/ANSI"; `xutf` has no folding). Input in decomposed form (NFD) does not match accented alternatives; accepted.
- *Starting set.* Each row has a paired negative test:

| Family | English trigger | Spanish trigger | Triple |
|---|---|---|---|
| identity | `my name is X` | `me llamo X`, `mi nombre es X` | `(user, name, X)` |
| identity | `I am X`, `I work as X` | `soy X`, `trabajo como X` | `(user, is, X)` |
| identity | `I work at/for X` | `trabajo en/para X` | `(user, works_at, X)` |
| identity | `I live in X`, `I am based in X` | `vivo en X` | `(user, lives_in, X)` |
| preference | `I use X`, `I am using X` | `uso X`, `estoy usando X` | `(user, uses, X)` |
| preference | `I like/love/prefer/enjoy X` | `me gusta X`, `prefiero X` | `(user, prefers, X)` |
| preference | `I hate/dislike/don't like X` | `odio X`, `no me gusta X` | `(user, dislikes, X)` |
| instruction | `(I\|you) always\|never X` | `(yo\|tú)? siempre\|nunca X` | `(user, instruction, "always X"/"never X")` |
| version | `<name> (is at\|is on\|to\|upgraded to) [version] V` | `<name> (está en\|a la versión) V`, `actualizamos <name> a [la versión] V` | `(<name>, version, V)` |
| dependency | `<name> depends on\|requires <dep>` | `<name> depende de\|requiere <dep>` | `(<name>, depends_on, <dep>)` |
| decision | `we decided on X`, `we chose X over\|instead of Y`, `we are going with X` | `decidimos usar X`, `elegimos X en lugar de Y`, `optamos por X` | `(project, decided, X)`, `(project, rejected, Y)` |
| bound | `the deadline is D`, `X expires on D`, `X is valid until D` | `la fecha límite es D`, `X vence el D`, `X es válido hasta el D` | `(project, deadline, D)` or `(X, valid_until, D)`, with `valid_until` set (S4 only) |

- *Captures.* `X`, `<name>` and `<dep>` stop at the next clause boundary or comma. `<name>` and `<dep>` match a package-shaped token (letters, digits, `-`, `_`, `.`, `/`, `@`).
- *Versions.* `V` uses upstream's version guard: a bare `x.y` only after a version cue, `v1.2` and `x.y.z` alone, and a slice of a longer dotted run such as an IP address never (`upstream:packages/mnemopi/src/core/beam/consolidate.ts:68-88`).
- *Dates.* `D` is ISO `YYYY-MM-DD`, or `D Month YYYY` / `Month D, YYYY` / `D de mes de YYYY` with English or Spanish month names, parsed with `jiff`. Invalid dates (`2027-02-30`) produce nothing. Numeric (`03/04/2027`) and relative dates are not parsed (O5). "Valid through D" ends at the next UTC midnight (O4).
- *Not ported.* Upstream's metric and relation patterns (`upstream:packages/mnemopi/src/core/beam/consolidate.ts:693-744,776-780`): an `is` relation without a first-person subject over-matches narrative, and metrics have no consumer.
- *Supersession* ("we no longer use X") is not in v1: a `stopped_using` fact would coexist with the `uses` fact it contradicts, since facts are immutable (D10).

**D6. Retention content is user and assistant text, redacted and bounded.** The projection from the journal to `OwnedRetentionMessage` (`retain.rs:31-40`) is a pure function in `omp-driver` over the live chain of `Journal::scan`:
- *User messages.* Each `msg.user@1` is `RetentionRole::User` with its `text`, and `stable_id` is the entry id.
- *Assistant messages.* Each `msg.assistant.start@1` … `msg.assistant.end@1` span is one `RetentionRole::Assistant` message. Its text is the concatenation, in order, of `stream@1` appends whose stream was opened with prop `text`, and its `stable_id` is the start entry's id. Thinking streams are excluded. An assistant span with no `msg.assistant.end@1` (an unsettled turn) is not projected.
- *Excluded entries.* Tool calls, tool results, compaction entries and all other kinds are not projected in v1, so blob-spilled payloads are never read. Tool observations are D12.
- *Recalled memories.* Text between `<memories>` and `</memories>` (the rendering at `session.rs:111-120`) is removed, so recalled memories are never retained again.
- *Live chain.* The live chain is the scanned entries' branch that materialises into the session, by the same rule as `crates/driver/src/sessions.rs:269-294` (one shared helper, not a third copy).
- *Redaction and bounds.* Every projected text passes through `SecretRedactor::new(omp_secrets::builtins::credential_rules()?)` before retention. Each message is then truncated on a character boundary at a per-message byte cap (a starting value, kept in code). The extraction text keeps its existing 256 KiB bound (`retain.rs:189-203`).

**D7. Gates: three convars, default-off or default-unchanged; no `cap-*` feature.** They are declared beside `SV_WORKTREE_BASE` in `crates/envd/src/host_settings.rs` (`:139`), read in `HostSettings::from_con` (`:204-234`), and carried as `#[serde(default)]` fields of `MnemopiSettings` (pattern `config.rs:196-198`); `omp-memory` gains no `omp-con` dependency.
- `sv_memory_session_retention` (bool, off): the rollout gate for D11. It is needed because `ai_memory_backend = mnemopi` with `auto_retain` (default true, `config.rs:193-195`) already *promises* retention ("recall/retain backend", `host_settings.rs:84`) but nothing does it. Turning it on for every existing Mnemopi user before the acceptance tests pass would change what that setting does today. *Sunset:* once S1-S3 pass and the owner approves (O8), the convar is removed and retention follows `ai_memory_backend` and `auto_retain` alone.
- `sv_memory_llm_mode` (new con enum `MemoryLlmModeSetting { Smol, None }`, default `Smol`, today's value), mapped in `from_con` to `MnemopiSettings.llm_mode` like `ai_memory_backend` (`host_settings.rs:211-214`). It offers no other value in this record.
- `sv_memory_fact_expiry` (bool, off) → `MnemopiSettings.fact_expiry` → `BankOptions.fact_expiry`.

They use the `sv_` prefix as omp2-native host policy; the `ai_mnemopi_*` family carries pi-era `legacy.path` aliases (`host_settings.rs:85`). There is no `cap-*` feature because nothing adds build weight. New dependencies: `regex` in `omp-memory` (a workspace dependency); `omp-driver` already has `omp-journal` and `omp-secrets`. With every gate at its default, behaviour and the bank file are byte-identical to today.

**D8. Retention is keyed by the journal, not by the runtime.**
- *Key.* `SessionMemory` gains `SessionMemory::keyed(runtime, key, policy)`, where `key` is the journal name (the value `SessionSwitched::to` uses, `hooks.rs:741-747`) and `policy` is `ExtractionPolicy::{Off, Rules}`. Its `retain_settled`, `shutdown_flush` and drain use `key` where today's code uses `runtime.session_id()` (`session.rs:145-152,221-228`). That covers the cursor, the episode id material and the job's `session_id`.
- *Unchanged paths.* `SessionMemory::top_level` and the explicit `retain` tool keep the runtime id and today's predicate (`runtime.rs:511,525`; `session.rs:151,227`), so `disabled_extraction_lane_retains_without_enqueuing` and every existing test hold.
- *Seeding.* When the host attaches to a journal (D11), it seeds the key's cursor with the live chain's current user-turn count through a new `BankStore::seed_retention_cursor(key, turns)` (an `INSERT OR IGNORE` into `retention_cursors`). A journal retained before keeps its cursor. A pre-feature or forked journal starts from "now" instead of re-retaining its history. A crash before the first retention resumes from the seeded value.
- *Recovery and rewind.* Resuming the same journal continues from its durable cursor, which recovers a window that the shutdown budget or a crash cut off. After a rewind, the live chain can hold fewer user turns than the cursor; new turns are retained only once the count passes it (O7).

**D9. `none` mode in `session.rs`: enqueue, then drain with the rules, in the same call.** With `ExtractionPolicy::Rules` the retainer is built with `extraction_enabled = true`, so the job is written in the retention transaction (`store.rs:527-536`). Afterwards `retain_settled` and `shutdown_flush` call `extract::rules::drain_key(store, key)`:
- *Read.* It reads this key's queued jobs with a new `BankStore::pending_extractions_for_key(key, max_jobs)`, which has the same count and byte bounds as `store.rs:571-599` and returns `QueuedExtraction { request: ExtractionRequest, created_at: Str }`. `ExtractionRequest` itself is unchanged (`extract.rs:19-28`).
- *Commit.* It runs the rules and calls `complete_extraction` per job, so the facts and the job's deletion commit together.
- *Crash safety.* A crash between retention and drain leaves the job, and the next retention, shutdown or attach of the same journal drains it.
- *Scope.* The drain reads only this key's jobs: another process sharing the bank may own other jobs, and D1 forbids rules on them.
- *Errors.* A drain error is logged with its typed error and does not fail retention. `RetentionOutcome` gains `rule_facts_inserted: usize`.
- *Stale jobs.* At attach the host deletes this key's jobs whose `created_at` is older than the working-memory TTL (new `BankStore::prune_stale_extractions(key, max_age)`). This bounds jobs whose episode was consolidated, which eviction no longer reaches (`store.rs:1599-1605`).
- *Other modes.* With `ExtractionPolicy::Off` no job is written, so no queue grows.

**D10. Facts stay immutable.** There is no mutation path. A newer fact does not expire or replace an older one, so `(user, works_at, X)` and a later `(user, works_at, Y)` coexist, and recall ranks them by text match only (`store.rs:832-836`). This is an accepted cost (O6).

**D11. Wiring: a native hook host in `omp-driver`.**
- *Attach.* `crates/driver/src/memory_retention.rs` defines `MemoryRetentionHost: NativeHookHost`. `compose_kernel` attaches it to the admission gate beside the plugin host (`kernel.rs:3081-3099`), but not under `tools_enabled`, so `--no-tools` print runs retain. All of these must hold:
  - `sv_memory_session_retention` is on;
  - the new `KernelOptions.memory_retention: bool` (default false) is true; only `ChatCommand::compose` sets it (`chat_cmd.rs:747-760`);
  - `options.parent_session` is `None` and `options.ephemeral` is false, so `--no-session` retains nothing;
  - `environment.memory_runtime().is_active()` (`crates/envd/src/lib.rs:1337-1339`; `runtime.rs:347-350`).
- *Construction.* It receives the journal name and path from the same values the plugin host gets (`kernel.rs:3083-3086`). It builds `SessionMemory::keyed(runtime, journal_name, policy)` with `policy = Rules` when `llm_mode == None`, else `Off`. It then seeds the cursor, prunes stale jobs and drains this key's jobs once (D8, D9).
- *Turn end.* `observe(turn_end)` increments a counter and returns. When the counter reaches `retain_every_n_turns`, it resets it and does a `try_send` of a unit signal onto a bounded `flume` channel of capacity 1; a full channel drops the signal (one scan is already pending). A user turn produces at least one `turn_end`, so scanning at every N-th `turn_end` never misses a due window; it may scan early, and the durable cursor decides (`retain.rs:109-127`). The retention thread (a plain `std::thread` owned by the host; all work in this record is synchronous) scans the journal, projects it (D6), and calls `retain_settled`, which also drains in `Rules` mode (D9). Each scan is a `tracing` span with entry count and duration. An incremental journal reader is a follow-up if the measured cost warrants it.
- *Session end.* `decide(session_shutdown)` sends a close request to the retention thread. The thread does one final scan, `shutdown_flush` (which drains in `Rules` mode), then `MemoryRuntime::enqueue`, the same consolidation `/memory enqueue` runs, which is idempotent. `decide` awaits the reply with `recv_async` under `tokio::time::timeout(SESSION_SHUTDOWN_BUDGET)` and returns `NativeReply::defer()` (`hooks.rs:563-567`; the verdict is ignored, `hooks.rs:770-772`). Work still running at the deadline stays detached on the thread, as `shutdown_flush` already allows (`session.rs:171-174`). If the process exits first, the unretained turns are retained the next time that journal is resumed (D8). Unconsolidated episodes age out after 24 h (O12).
- *Session switch.* `session_switched` must not block (`hooks.rs:778-783`). The host queues the old journal's close on the thread without waiting; `shutdown_flush` is a no-op if it already ran (`session.rs:182-187`). It then builds a new `SessionMemory::keyed` for the new journal and seeds, prunes and drains it. `/new`, `/resume` and hand-offs therefore keep retaining under their own journal's key.
- *Daemons.* `memory_runtime()` is the runtime of the session process's own server, so retention writes the bank from the session's process. Whether a project daemon should also retain is out of scope (O2).

**D12. Deferred to the follow-up ADR.** It is not designed here. The follow-up MUST settle, among other things:
- *Tool observations.* How tool observations reach the rules: their transport (a job column or table, which is another schema change) and crash safety. Also how manifest facts are retired when a manifest changes.
- *Lanes.* A lane over the `@memory` role (`reflection.rs:21-24`) and the Apple FM lane, with their bounds.
- *Producer separation across processes.* A per-key drain, or a claim or lease on jobs.
- *Faults.* The typed fault and worker API, with a meaning for "rejected" that agrees with D1.
- *Fallback.* The fallback from an unavailable lane to the rules.
- *The sixth `FACT` field* and whether out-of-vocabulary model predicates are accepted (`parse_facts` accepts any predicate today, `extract.rs:111-122`).

## Alternatives considered

| Option | Why not |
|---|---|
| Literal port of the upstream patterns | English only; key/value rows would index as "version version 1.2.3"; no decisions or dependencies. |
| Rules as an `ExtractionLane` emitting `FACT` text through `extract_and_store` | `parse_facts` derives ids from the line number and source (`extract.rs:125-128`), so rule provenance (`rule_` ids) and rule-id idempotence are lost. The five-field line cannot carry `valid_until` in this record (D2, D12). `none` mode would still need its own drain trigger. |
| Broad reading of owner decision (1): rules also run in `smol` when no lane is bound | In `smol` the `@memory` role normally resolves, so a memory model is set; rules there would contradict the owner. A real `@memory` lane is the right consumer (D12). |
| Enqueue jobs in `smol` now and drain them later | Nothing in this record drains them; consolidated jobs escape eviction (`store.rs:1599-1605`; `runtime.rs:766-778`), so the queue would grow without bound. |
| Key retention by the runtime's session id | A fresh ULID per process (`crates/envd/src/lib.rs:1617`): resume re-retains history, and crash recovery is impossible. |
| Re-key the runtime itself per journal | Changes `omp-envd`'s runtime lifecycle and every explicit memory tool; a per-retention key is additive. |
| A memory Director registered by `compose_kernel` | It journals a `<director>` node, and `from_dom` stops the active chain at an unconstructible node (`crates/contract/src/director.rs:855-857`). Retention claims no slot, so it is not control. |
| The headless finalizer's Mnemopi phase (30 s budget, `crates/driver/src/headless/finalize.rs:50-56`) | No owner calls `finalize` (`:118-136`); five modes would each need a close call. `session_shutdown` reaches every mode, and the journal key makes a missed window recoverable on resume. |
| Fill `turn_end`'s `items` with the transcript | Changes an existing hook payload (`crates/agent/src/loop.rs:1846-1856`). |
| `project_thread` over a mirrored DOM (`crates/session/src/projection.rs:311`) | It drops turns before the newest compaction, so the user-turn count shrinks and the cursor stalls; the journal's live chain keeps them. |
| Retain tool results now | Large, often blob-spilled, and the most secret-prone content; it has no consumer until tool observations (D12). |
| Add the column on every open regardless of the gate, or bump to 4 | The first changes every user's bank with every gate off. The second resets indexes and locks older binaries out. Both are kept as alternatives in O3. |
| A Unicode folding dependency for accents | Banned outside `xutf` (AGENTS.md), and `xutf` has no folding. Explicit alternations are enough for a fixed vocabulary. |
| A separate `sv_memory_rule_facts` convar | `sv_memory_llm_mode = none` is already the user's explicit choice of "no memory model", and retention has its own gate. |

## Slices

Each slice is one PR, written test-first; tests are not edited to pass. Verification is remote: `just remote-test-pkg omp-memory`, `just remote-test-pkg omp-driver` and a clippy run (`AGENTS.md`, "Commands"). Every slice leaves `just lint`, `just test` and the touched e2e proofs green, and updates this ADR's `Status in omp` and its STATUS.md row when it moves code. No slice adds a journal kind or payload, so no golden journal is added; no slice changes model-facing prompts.

**S1. Journal-keyed retention for main sessions.** Gate: `sv_memory_session_retention`.
- *Scope.* D6 projection and redaction; D8 key, seeding and `SessionMemory::keyed` with `ExtractionPolicy::Off`; D11 host, thread, turn counter, shutdown and switch; `KernelOptions.memory_retention`; the convar.
- *Tests.*
  - Unit, `omp-driver`: `projection_maps_user_and_assistant_text` (thinking, unsettled spans and tool entries excluded; `<memories>` removed); `projection_redacts_credential_shaped_tokens`; `projection_follows_the_live_chain`.
  - Unit, `omp-memory`: `keyed_retention_uses_the_key_for_cursor_and_episode`; `seed_does_not_override_an_existing_cursor`; `off_policy_writes_no_job`.
  - Driver test `crates/driver/tests/memory_retention.rs`, on the `crates/driver/tests/memory_reflect.rs` template (real `ProjectEnvironment`, scripted `Inference`, real journal).
- *Acceptance, gate off.* A scripted session leaves no working-memory row, no cursor, no job and no host attached, and the journal holds nothing memory-specific.
- *Acceptance, gate on:*
  - a session of `retain_every_n_turns` user turns stores one episode recallable through `search`;
  - a shorter print-mode session stores its window at `session_shutdown` within the budget on a small bank;
  - closing and resuming the same journal (`--continue`, a new process) does not re-retain earlier turns, and does retain turns cut off before;
  - a forked journal starts from the fork point;
  - `observe(turn_end)` returns while the thread is blocked (a test seam on the thread's channel);
  - `/new` retains the new journal under its own key;
  - child, `--no-session`, `/btw`, tan and non-opted-in compositions retain nothing.

**S2. English rules in `none` mode.** Gate: `sv_memory_session_retention` and `sv_memory_llm_mode = none`. Depends on S1.
- *Scope.* `crates/memory/src/extract/rules/` (core, English identity, preference and instruction rows of D5); `ExtractionPolicy::Rules`; `drain_key`, `pending_extractions_for_key`, `prune_stale_extractions`, `QueuedExtraction`; the `sv_memory_llm_mode` convar; the `regex` dependency; the `omp-memory` attribution notice in `THIRD-PARTY-NOTICES.txt`.
- *Tests.*
  - Unit: one positive and one negative per D5 row.
  - Upstream vectors as triples:
    - "My name is Ada. I work at Example Corp and I prefer dark mode." gives `(user, name, Ada)`, `(user, works_at, Example Corp)` and `(user, prefers, dark mode)` (`upstream:packages/mnemopi/test/extraction.test.ts:93-103`);
    - "I live in Berlin and I use TypeScript." gives `lives_in` and `uses`;
    - "I never use semicolons and you always wrap lines at 100." gives two `instruction` facts;
    - the subjectless assistant narrative gives none (`:205-227`).
  - Adversarial inputs (256 KiB of "I am ", 1 MiB without separators) stay within the fact cap (asserted on counts).
  - Store: `drain_key_leaves_other_keys_alone`; `crash_between_retention_and_drain_is_recovered_at_attach` (two processes, same journal key); `stale_jobs_are_pruned_at_attach`.
- *Acceptance.* `disabled_extraction_lane_retains_without_enqueuing` and every existing `omp-memory` test pass unchanged. With `smol`, no job and no rule fact appear, which remains true after this ADR because lanes are D12. Replays leave one row per fact, and eviction removes rule facts. The S1 driver test gains a `none` row: the vector above stored and recalled end to end.

**S3. Spanish and technical rules.** Gate: as S2. Depends on S2.
- *Scope.* The Spanish rows and the version, dependency and decision rows of D5, in both languages.
- *Tests.*
  - Paired vectors:
    - "Me llamo Ada" gives `(user, name, Ada)`;
    - "We upgraded tokio to 1.40.0" and "Actualizamos tokio a la versión 1.40.0" give `(tokio, version, 1.40.0)`;
    - "omp-memory depends on jiff" and "omp-memory depende de jiff" give `(omp-memory, depends_on, jiff)`;
    - "We decided on SQLite instead of Postgres" and "Elegimos SQLite en lugar de Postgres" give `(project, decided, SQLite)` and `(project, rejected, Postgres)`.
  - Negatives: "Do we use tokio 1.40?" and "¿Usamos tokio 1.40?"; "I don't use Postgres" and "No uso Postgres"; "If we chose Postgres…"; a password sentence; `10.0.0.1`; accented and unaccented spellings both match.
  - Upstream's version regressions (`upstream:packages/mnemopi/test/beam-consolidate-unit.test.ts:424,442,459,476,490`).
- *Acceptance.* All pass; S2's tests are unchanged.

**S4. Fact expiry.** Gate: `sv_memory_fact_expiry`. Depends on S2. Merges only after O3 is answered.
- *Scope.* D2 and D3: `BankOptions`, `open_with` at `runtime.rs:285,310`, the gated migration step, `NewFact.valid_until`, the nine-column INSERT, `search_facts_at`, and the bound rows of D5 in both languages.
- *Tests.*
  - Gate off: after open, the `facts` columns and `user_version` equal today's; recall does not filter; bound rules write no `valid_until`.
  - Gate on:
    - `SCHEMA_VERSION` stays 3 and `memory_embeddings` rows survive the open;
    - two stores opening a pre-column bank at once leave one column;
    - the old eight-column INSERT text succeeds on a migrated table;
    - "The deadline is 2027-03-15" and "La fecha límite es el 15 de marzo de 2027" store `(project, deadline, 2027-03-15)` with `valid_until` at 2027-03-16T00:00Z, a constant computed independently in the test;
    - `valid_until == now` is expired;
    - expired rows are hidden ahead of `LIMIT`;
    - read-by-id returns them;
    - `2027-02-30` produces nothing.
  - Gate on, then off: expired facts are recalled again (the stated cost).
- *Acceptance.* As the tests; S2 and S3 tests are unchanged.

## Consequences

- With `sv_memory_session_retention` on, interactive, print, RPC, ACP and share sessions retain redacted episodes under their journal, which nothing does today. Resume, crash and session switch behave per journal. With `llm_mode = none` the user also gets recallable facts in English and Spanish, including versions, dependencies and decisions. With expiry on, deadline facts expire. With every gate at its default nothing changes, including the bank file.
- Prohibited: rules in any mode but `none`; rules calling any model or touching another key's jobs; mutating a fact; retaining tool results in this record; a memory Director; a JS/TS rule engine.
- Costs accepted:
  - one journal decode per N `turn_end` events, unmeasured until S1 records it;
  - a column added without a version bump, setting the D3 precedent; older binaries ignore expiry; turning expiry off re-surfaces expired facts; cached recall can lag expiry by one index generation;
  - immutable facts accumulate stale values (D10), and `project` is ambiguous in a `Global` bank;
  - NFD input misses accented patterns;
  - the 1.5 s shutdown budget may miss consolidation (O12);
  - turns before the feature was turned on, or before a fork, are not retained;
  - three convars until the retention gate's sunset.
- Not delivered by this record, although the gap brief asks for them: structured tool results first, the Apple FM `local-memory-model` lane with its fallback to the rules, and model-written `valid_until`. They are the follow-up ADR (D12; O9).

## Open questions for the owner

- **O1. Narrow reading of "no memory model is set".** D1 runs rules only in `none`, so default `smol` users get episodes but no facts until the follow-up adds a lane. Alternative: the broad reading (rules whenever no lane is bound). Recommendation: D1.
- **O2. Project-daemon retention.** No wire message carries memory to a daemon (research note 2.2); D11 retains from the session's own process. Recommendation: revisit with the follow-up ADR.
- **O3. Additive `valid_until` column without a `SCHEMA_VERSION` bump (D3).** This extends an existing schema. Recommendation: confirm D3 (column only while `sv_memory_fact_expiry` is on; no bump). Alternatives: add the column unconditionally in the exclusive migrate step (simpler, changes every bank), or bump to 4 (index reset, older-binary lock-out). S4 waits; S1-S3 do not depend on it.
- **O4. Date-only bounds are UTC.** "Valid through that day" ends at the next UTC midnight. Alternative: local time.
- **O5. Numeric and relative dates are out of v1.** Revisit when retained messages carry timestamps.
- **O6. Fact identity and recency.** Facts are immutable, have no recency preference, and `project` has no project identity in a `Global` bank. Recommendation: accept for v1; a later record may add supersession.
- **O7. Rewind.** After a rewind the cursor can exceed the live chain's user-turn count, and new turns retain only once the count passes it. Alternative: re-seed the cursor on rewind, which needs a rewind signal the host does not receive today. Recommendation: accept for v1.
- **O8. Gate sunset.** Remove `sv_memory_session_retention` once S1-S3 pass, so retention follows `ai_memory_backend` and `auto_retain`. Recommendation: confirm.
- **O9. The scope split.** Tool observations, model lanes (`@memory` and Apple FM), lane faults and the sixth `FACT` field move to a follow-up ADR (D12). Recommendation: confirm.
- **O10. Languages.** Spanish only. Upstream's detector also knows Russian and German (`upstream:packages/mnemopi/src/core/beam/consolidate.ts:555-612`); each language needs reviewed vectors.
- **O12. Shutdown budget.** `session_shutdown` waits 1.5 s (`hooks.rs:571`). Consolidation scans every scoped working row (`runtime.rs:766-778`), and unconsolidated episodes and their facts are evicted after 24 h (`config.rs:266-268`; `store.rs:1599-1605`). Options: accept and measure in S1; consolidate on the retention thread after each periodic retention; or a longer budget for this host, which changes a shared hook contract. Recommendation: accept, measure, decide.

(O11, O13 and O14 of round 1 were resolved or moved: O11 and O13 to D12, O14 by D8.)

## What the review changed

### Round 1 (findings recorded in `docs/adr/STATUS.md` row 0062; verified and resolved 2026-11-14)

- **Session wiring slice missing; the extraction worker has no production caller; the ADR must say how `session.rs` calls it in `none` mode.** Confirmed: nothing outside `crates/memory/src` references `pending_extractions`, `ExtractionLane`, `retain_settled` or `shutdown_flush` (repository search). `RegisteredMemoryRuntime::session` has no caller (`crates/envd/src/memory.rs:100-102`), nor does `finalize` (`crates/driver/src/headless/finalize.rs:118-136`). `session.rs:151,227` skip the job in `none` mode. Changed: a native hook host in `omp-driver`, the `session.rs` change and `drain_session`; wiring as the first slice.
- **`SCHEMA_VERSION` choice for `valid_until` undecided.** Confirmed. The previous D3 also misread the code: column additions run only when `version < SCHEMA_VERSION`, with the index reset (`store.rs:1220-1222`). Changed: additive column with no bump, a new migration step only while the gate is on, `open_with`, and O3 kept for the owner.
- **Apple FM lane bounds unspecified.** Confirmed in part: chunk size, chunk count, deadline and token cap were stated; the token arithmetic, time and request bounds, the mid-job unavailability rule and the `Runtime` and `ModelNotReady` mappings were not (`applefm.rs:98-99,1480-1484`). Changed: D9 rewritten. Round 2 then moved the lane to a follow-up ADR (below).
- Corrected against this tree in round 1:
  - the prototype commit `daab333d13` is not in the repository;
  - the research note's close-path owners were wrong (`session_ops.rs:275` is `/btw`, `chat_control.rs:3073` a tan job);
  - a Director would truncate the active chain (`director.rs:855-857`);
  - `sv_memory_local_extraction` was dropped.

### Round 2 (four fresh-context reviewers and a synthesizer; 3 blockers, 20 majors, 5 minors; verified 2026-11-14)

Blockers:
- **B1. The cursor and job identity are per-process, so the resume and crash claims were false.** Confirmed: the runtime id is `Ulid::generate()` per start (`crates/envd/src/lib.rs:1617`), and the cursor, episode id and job `session_id` derive from it (`store.rs:490-499,545-550`). Resolved by D8: a journal key through `SessionMemory::keyed`, cursor seeding at attach, and attach-time drain and prune. The round-1 "first session only" limit (old O14) disappears. New S1 and S2 tests use two processes on one journal.
- **B2. No path carries tool observations from the journal to the rules.** Confirmed: the job holds user text only (`retain.rs:177-187`; `store.rs:527-536`), and `OwnedRetentionMessage` has no tool fields (`retain.rs:31-40`). Resolved by scope cut: tool observations (old D5 and S4) move to the follow-up ADR, which must choose a transport (D12). Not designed here.
- **B3. The journal projection was unspecified.** Confirmed against the journal shapes (`crates/journal/src/data.rs:102-104,505-575`; `crates/agent/src/loop.rs:4797`). Resolved by D6: kinds and roles, the text source, `stable_id`, thinking, unsettled spans, compaction and `<memories>` handling, live chain, no blob reads, redaction and per-message bound. `project_thread` was checked and rejected because it drops pre-compaction turns (`crates/session/src/projection.rs:305-320`).

Majors, accepted:
- **M1 `Rejected` contradicts D1; M2 cross-process producer mixing; M17 untyped worker API.** Confirmed (`store.rs:571-599` has no claim; `extract.rs:30-35` is RPITIT). Resolved by moving the lanes, faults and worker to the follow-up ADR, with these three as requirements it must meet (D12).
- **M3 one thread for retention, drain and shutdown under 1.5 s.** Confirmed (`hooks.rs:571`). With no model drain left, the thread runs only millisecond-scale synchronous work. `decide` uses `recv_async` under `tokio::time::timeout` (D11). S1 has a print-mode shutdown test. O12 remains for consolidation time.
- **M4 dead and unbounded queue.** Confirmed: consolidated jobs escape eviction (`store.rs:1599-1605`; `runtime.rs:766-778`). Resolved: `ExtractionPolicy::Off` writes no job outside `none`, and attach-time `prune_stale_extractions` bounds orphans (D9).
- **M5 the S2 and S6 contradiction.** Resolved by removing S6; S2's `smol` assertion is now final for this record.
- **M6 scope too large.** Accepted (O9).
- **M7(a) the existing `@memory` binding.** Confirmed (`crates/driver/src/headless/reflection.rs:21-24`). It informs D1's narrow reading, and the `@memory` lane is named in D12.
- **M8 gate layering.** Confirmed that `ai_memory_backend` and `auto_retain` exist (`host_settings.rs:74-87`; `config.rs:193-195`). Changed: D7 states why a rollout gate is still needed (the brief requires default-off until acceptance; enabling retention for existing Mnemopi users changes that setting's effect today), with a sunset (O8). `sv_memory_rule_facts` was dropped. `sv_memory_llm_mode`'s enum type and mapping are named. `local-memory-model` and the `AuxModel` relation leave with the lane.
- **M9 expiry plumbing.** Confirmed (`runtime.rs:285,310`; `recall.rs:205`; `store.rs:724-760`). D3 now names the open sites, the stored option, INSERT selection, `search_facts` delegation and clock, the state matrix and the recall-cache lag. Unconditional addition is kept as the O3 alternative rather than adopted, because the additive rule asks the owner and a default-off gate keeps every bank unchanged. The sixth field moved to D12, so `parse_facts` needs no parameter.
- **M10 stale manifest facts and the missing `toml` dependency.** Resolved by scope cut (D12 requires a retirement story).
- **M11 vocabulary too broad and under-specified.** Confirmed (`upstream:packages/mnemopi/test/extraction.test.ts:93-103` expects sentences). Changed: the vocabulary went from 17 to 13 predicates (`dev_depends_on`, `requires_runtime`, `stopped_using` and `status` dropped), and D5 now specifies clause splitting, guards, word lists, table format and location, per-row triggers and triples, and which upstream patterns are not ported. The triple contract for upstream vectors is written out in S2.
- **M12 accent matching mechanism.** Confirmed: `xutf` 1.6.0 has no folding (`src/normalize.rs:134-199`). D5 uses explicit alternations, names `jiff` and `regex`, and states the NFD limit.
- **M13 no redaction.** Confirmed (`retain.rs:172-175`; `crates/secrets/src/builtins.rs:18`). D6 redacts every projected text with `SecretRedactor` and does not retain tool results.
- **M14 `--no-session`.** Confirmed (`chat_cmd.rs:602,657`). D11 excludes `ephemeral`, and S1 tests it.
- **M15 O(N²) rescans.** Confirmed (`crates/journal/src/lib.rs:186-203`). D11 scans only every N-th `turn_end` and records a span. Still a full decode per scan: accepted cost, with an incremental reader as a follow-up.
- **M16 first session only.** Resolved by D8. Rewind is now O7.
- **M18 `created_at` and `now` unreachable.** Confirmed (`store.rs:578`; `extract.rs:19-28`). `QueuedExtraction` carries `created_at`; "now" was removed, since rules read no clock.
- **M19 handle and attach site.** Confirmed: `RegisteredMemoryRuntime` is private to the server and the plugin host is under `tools_enabled` (`kernel.rs:3081`). D11 uses `environment.memory_runtime()` with `SessionMemory::keyed`, takes the initial journal from compose, and does not depend on `tools_enabled`. The "in-process only" discriminator was removed: `memory_runtime()` is always the session process's own server's runtime (`crates/envd/src/lib.rs:1337-1339`).
- **M20 `FACT` format and glossary.** Added the Glossary section.

Majors, rejected:
- **M7(b) adopt `RuleLane: ExtractionLane`.** Rejected. `parse_facts` ids come from the source and line number (`extract.rs:125-128`), so rule provenance and rule-id idempotence would be lost, and the five-field line cannot carry `valid_until` in this record. Recorded in Alternatives.

Minors:
- **m1 Apple FM bounds weaker than stated.** Moot here (moved to D12).
- **m2 subject ambiguity and recency.** Accepted as costs (D4, D10, O6).
- **m3 `/memory enqueue`.** Acknowledged in Context and D11 (the same idempotent consolidation).
- **m4 altitude.** Accepted: caps, confidences and byte limits are starting values kept in code; the Apple fault table has left; the exact epoch literal moved into the test as "computed independently".
- **m5 header and citations.** The ADR 0023 citation for "rules call no model" is corrected to this record's own rule. The `Date:` versus status difference is rejected as a defect: `Date:` is the creation date (`docs/adr/README.md`, "Format"), and the header now says so. STATUS.md is outside this change. The ambiguous "research note S4" references are removed.

Corrected against this tree in round 2, beyond the findings:
- The plugin host attaches only under `tools_enabled` (`kernel.rs:3081`); the memory host must not.
- `project_thread` omits turns before the newest compaction (`crates/session/src/projection.rs:305-320`).

Machine-reviewed (EGM rounds 1 and 2), not owner-approved. Round 3 is pending.

## Acceptance tests

1. S1. With `sv_memory_session_retention` off, a composed session attaches no memory host, writes no row, cursor or job, and every existing `omp-driver` and `omp-memory` test passes unchanged.
2. S1. With it on, a main session retains at the interval and at session end, under its journal key. Resuming the same journal in a new process neither duplicates nor loses turns. A fork starts at the fork point. `/new` retains under the new key. Retained text is redacted and excludes thinking and tool results. `turn_end` observation never blocks. Child, `--no-session`, `/btw`, tan and non-opted-in compositions retain nothing. A print-mode session stores its window at shutdown.
3. S2. With `none`, the upstream English vectors (`upstream:packages/mnemopi/test/extraction.test.ts:93-103,205-227`) yield the stated triples end to end. With `smol` no job and no rule fact appear. Replays are idempotent. Eviction removes rule facts. A job left by a crashed process is drained when that journal is next attached. Other keys' jobs are untouched. Stale jobs are pruned. Adversarial inputs stay within the cap.
4. S3. Paired English and Spanish vectors for identity, preference, versions, dependencies and decisions pass. The negatives yield nothing. Upstream's version regressions pass.
5. S4. With `sv_memory_fact_expiry` off, the bank's schema and `user_version` are unchanged and recall does not filter. With it on: no version bump, one column under concurrent opens, embeddings survive, older statements work, `valid_until == now` is expired, expired facts are hidden ahead of `LIMIT`, read-by-id returns them, and the dated English and Spanish vectors store the independently computed instant.
6. All slices. No identifier, message or convar in new code uses `pi` as a product name. Every new convar is `sv_memory_*`. No error variant carries a string payload. No prompt text is built in code. No Unicode utility crate is added.

## Status in omp

**Status: Not started.**

- The seam exists unwired: `ExtractionRequest`, `ExtractionLane`, `parse_facts` and `extract_and_store` (`extract.rs:19-136`); the queue (`store.rs:571-605,712-760`); the retention gate `llm_mode != None` (`session.rs:151,227`); and the top-level handle with no caller (`crates/envd/src/memory.rs:100-102`).
- Nothing calls retention, drains the queue, implements a lane, or controls `llm_mode` (research note 2.2-2.4). No slice is implemented.

## Upstream origin and license

Draws from `upstream:packages/mnemopi/src/core/extraction.ts:264-315` (the English first-person patterns and the clause split) and `upstream:packages/mnemopi/src/core/beam/consolidate.ts:66-88` (the version guard). Ports the vectors in `upstream:packages/mnemopi/test/extraction.test.ts:93-103,205-227` and `upstream:packages/mnemopi/test/beam-consolidate-unit.test.ts:424-490`. The Spanish rules, the dependency, decision and bound rules, the triple shape, fact expiry, journal-keyed retention and the hook host are original work.

License: MIT. `upstream:LICENSE:1-4` (Copyright 2025 Mario Zechner; 2025-2026 Can Bölük), `upstream:packages/mnemopi/LICENSE:1-4` (2025-2026 Can Bölük; 2026 Stencil Labs, Inc.), `upstream:packages/mnemopi/package.json:11`. There is no NOTICE file. omp2 is MIT, Copyright 2026 Stencil Labs, Inc. (`LICENSE:3`), and records ported lineages in `THIRD-PARTY-NOTICES.txt` (the `omp-shell-builtins` notice starts at `:1498`; ADR 0054 does the same, `docs/adr/0054-shell-builtins.md:106`). S2, which lands the ported patterns, adds an `omp-memory` attribution notice naming the upstream files and both copyright lines.

## References

ADR 0006 (policy on the host; the rules execute nothing and read no files); ADR 0012 (convars); ADR 0015 (Directors; why retention is not one); ADR 0023 (tiny local model; relevant to the follow-up's lanes); ADR 0024 (no tool or roster change); ADR 0035 (typed errors); ADR 0036 (Python extensions; not used); ADR 0049 (no vendor code in this record); ADR 0054 (gate and notice precedent); ADR 0068 (its extraction Director also observes main-agent prompts and uses the same redactor, but writes decision files, not the Mnemopi bank); `docs/research/mnemopi-fact-extraction.md`; `crates/driver/src/plugin_hooks.rs` (native hook host precedent).
