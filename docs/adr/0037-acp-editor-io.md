# 0037. ACP editors supply the document base; writes commit through the authority, then sync back

Status: accepted
Date: 2026-09-29
Area: interface

## Context

In ACP mode the editor is the remote driver of 0001: the user sees and edits the editor's buffers,
not the files on disk. An unsaved buffer can differ from disk for minutes. An agent that reads and
edits disk works on a file the user is not looking at. Its edit then lands under a dirty buffer, and
the editor either reports a conflict or the user's next save silently overwrites the agent's work.

**What ACP offers.** The Agent Client Protocol has client-side methods that the agent calls on the
editor. The shapes below come from v1's typed reimplementation of the SDK surface
(`origin/main:packages/utils/src/acp/protocol.ts`, described in its header as a "behavior-compatible
reimplementation of @agentclientprotocol/sdk's used protocol surface"), from its connection
(`packages/utils/src/acp/connection.ts`), and from its bridge
(`packages/coding-agent/src/modes/acp/acp-client-bridge.ts`). No ACP schema is vendored in either
tree, so these shapes are verified against v1 only, not against the upstream schema.

| Method / capability | Shape (v1) | Notes |
| --- | --- | --- |
| `initialize.params.clientCapabilities.fs.readTextFile` | `bool` | gates `fs/read_text_file` |
| `initialize.params.clientCapabilities.fs.writeTextFile` | `bool` | gates `fs/write_text_file` |
| `initialize.params.clientCapabilities.terminal` | `bool` | gates the `terminal/*` family |
| `fs/read_text_file` | `{sessionId, path, line?, limit?}` → `{content}` | `line` base (0 or 1) **unverified**; omp uses whole-file reads only |
| `fs/write_text_file` | `{sessionId, path, content}` → `{}` | no version or expected-content field, so **no CAS**; whether the client also saves to disk is client-defined (**unverified**) |
| `terminal/create` | `{sessionId, command, args?, env?: [{name,value}], cwd?, outputByteLimit?}` → `{terminalId}` | the client spawns `command` with `args` and no implicit shell (v1 `bash.ts` cites agentclientprotocol.com/protocol/v1/terminals) |
| `terminal/output` | `{sessionId, terminalId}` → `{output, truncated, exitStatus?}` | |
| `terminal/wait_for_exit` | `{sessionId, terminalId}` → `{exitCode?, signal?}` | |
| `terminal/kill`, `terminal/release` | `{sessionId, terminalId}` → `{}` | |
| tool-call content `{type:"terminal", terminalId}` | `ToolTerminal` | embeds a client terminal in a tool card |
| `$/cancel_request` | v1's agent-side dispatcher accepts it and ignores it | whether clients honor it for `fs/*` is **unverified**; this design never relies on it |

The canonical wire names are all `snake_case`. `ReadTextFileRequest.line`/`limit` and
`additionalDirectories` on session setup exist in v1's types, but their upstream semantics are
unverified, so nothing below depends on them.

**v1 behaviour** (`packages/coding-agent/src/tools/{read.ts,read-summary.ts,acp-bridge.ts,bash.ts}`):
- Reads went through the editor first ("the editor's in-memory buffer is source of truth") and fell
  back to disk on error.
- Writes called `fs/write_text_file` in place of the disk write, then read the file back from disk
  to detect client format-on-save drift (`BridgeWriteResult.driftedFromRequest`, which it notes is
  best-effort).
- `bash` wrapped the command line in `$SHELL -c` and ran it in the client's terminal through
  `terminal/create`.

The last two are prohibited in omp by the locked deviations: edits go through the envd document
authority, and Bash is an in-process interpreter (0028).

**omp today (audit):**
- **ACP adapter.** `crates/app/src/acp_mode.rs` reads `clientCapabilities` only for
  `/auth/terminal` (`initialize_transport` at lines 105–109, and re-initialize at lines 303–306). It
  never sends an `fs/*` or `terminal/*` request. The only outbound client request is
  `session/request_permission`, correlated by `PermissionRequests`.
- **Unbound envd seam.** envd already holds a dormant ACP seam that nothing binds in production.
  `grep bind_acp_` finds callers only inside `crates/envd` and `crates/env`. The seam is:
  - `AcpDocumentBackend` (`crates/envd/src/docs.rs:48`) and `AcpExecBackend`
    (`crates/envd/src/tool_shell.rs:55`);
  - `ProjectEnvironment::bind_acp_{documents,exec}` (`crates/envd/src/lib.rs:1166–1178`);
  - the wire frames `AcpBind`/`AcpReadQuery`/`AcpWriteQuery`/`AcpDocumentAnswer`/`AcpExec*`
    (`crates/proto/proto/omp/env/v1/env.proto:1710–1762`);
  - a hard-coded 30 s `ACP_QUERY_TIMEOUT` (`crates/envd/src/server.rs:8042`).
- **The seam's shape violates the locked decisions**, and it becomes live the moment anything binds
  it:
  - `write_plain` (`crates/envd/src/tool_document.rs:1760–1782`) returns early after
    `write_acp_text`. The editor write *replaces* the authority commit, so the file changes with no
    revision, no CAS and no rebase.
  - `ReadSources::open` (`crates/envd/src/tool_read_sources.rs:718–737`) invents an
    `acp:<sha256>` pseudo-revision that the authority does not know. A later hashline `Edit` then
    prepares against the disk head (`tool_document.rs:311–405`), so the model's snapshot and the
    commit base disagree.
  - `ShellExecHost::run` (`tool_shell.rs:608–648`) sends the raw command string to a client
    terminal under convar `sv_acp_routing` (default `auto`, `crates/envd/src/exec_settings/acp.rs`).
    That is v1's shell escape.
- **Parts that already fit.**
  - The authority's rebase is already three-way. `Edit` carries `authored_bytes` (the snapshot the
    model read) and `base_bytes` (the current head) and rebases one onto the other
    (`crates/tools/src/edit.rs:862–900`, `docserver/rebase.rs::rebase_content`).
  - Commits are `TextMutation{base_revision, proposed_content, REBASE_NON_OVERLAPPING}`.
  - Conflicts come back typed: `DocumentConflict.conflicting_ranges` in base coordinates, and
    `ClientFormatDrift` for format drift (`omp/document/v1/document.proto:733–788`).
  - `crates/e2e/tests/p1_doc_race.rs` proves that two concurrent stale writers serialize.

### Options considered

Documents:

- **D0 — status quo (disk only).** Simple, but the agent edits a file the user is not looking at.
  Rejected; this is the defect.
- **D1 — v1 shape: the editor is the authority.** Read and write only through `fs/*`. This bypasses
  versioned CAS, rebase and typed conflicts, and removes the disk-state guarantees P1 proves.
  Prohibited by the locked deviations.
- **D2 — buffer becomes an authority head.** Install buffer bytes as a committed `DocumentHead` in
  the docserver actor. This breaks the actor's invariant that the head equals persisted disk bytes
  with an exact fingerprint (`docserver/actor.rs` "present cached head has an exact disk
  fingerprint"; proto header: provisional state "never exposed as a DocumentHead"). The watcher and
  the rename recheck would then compare against a head that was never persisted. Rejected.
- **D3 — buffer is the transaction base; the authority commits; write-back follows.** The editor
  buffer, merged with disk, becomes the base the tool reads and edits. The commit is an ordinary
  authority transaction against the disk revision, and `fs/write_text_file` then pushes the committed
  bytes to the editor. **Chosen (owner decision).**
- **D4 — commit only the agent's delta to disk; push buffer+delta to the editor.** This keeps the
  user's unsaved edits unsaved. It conflicts exactly when the agent works on what the user is typing,
  and clients that save on `fs/write_text_file` (reported for Zed, unverified) persist the buffer
  anyway. Rejected (resolved question 1).

Terminal:

- **T0 — v1 shape.** Run Bash in the client terminal via `terminal/create` + `$SHELL -c`. This is a
  shell escape and bypasses interpretation-time capability approval (0028). Prohibited.
- **T1 — leave `terminal/*` unused.** Bash stays in-process. Its output already streams to the
  editor as `tool_call_update` content (`crates/app/src/acp_events.rs::tool_content`). No client
  process is spawned.
- **T2 — mirror into an editor terminal.** `terminal/create` would spawn a display-only viewer (the
  `omp` binary tailing the job's output), never a shell. Every `terminal/create` is still a
  client-side spawn of an executable the client resolves: the host would have to expose a job-output
  endpoint and pass a credential through argv/env, and on a remote driver the client may not have a
  matching `omp`. It adds a second execution path with no policy value and only a cosmetic gain.
- **T3 — display-only terminal without spawn.** Some clients reportedly accept agent-pushed terminal
  output through `_meta` on tool-call updates (**unverified**). It spawns nothing, so it would be
  compatible with 0028.

## Decision

### 1. Capabilities

1. The ACP adapter MUST parse `clientCapabilities` into typed serde structs, not `Value` pointers:
   `fs.readTextFile`, `fs.writeTextFile`, `terminal`, and `auth.terminal`. Unknown fields are
   ignored. Capabilities are fixed per `initialize` and apply to every session on that connection.
2. When `fs.readTextFile` or `fs.writeTextFile` is advertised, the driver binds an editor document
   backend into the environment (`ProjectEnvironment::bind_acp_documents`) for the live session. It
   rebinds on session switch and unbinds on `session/close`, EOF and `shutdown`. It binds only when
   the connection is eligible (§2): the live session's `cwd` matched the project root and convar
   `sv_acp_fs` is not `off`. `terminal` is parsed and ignored (§6). omp advertises nothing new in
   `agentCapabilities`.
3. All outbound client requests (`session/request_permission`, `fs/*`) share one correlation table
   keyed by JSON-RPC id. Each `fs/*` request carries a deadline of convar `sv_acp_fs_timeout`
   (default 5 s). One convar covers reads and writes, and it replaces `ACP_QUERY_TIMEOUT`.
   `session/request_permission` waits for a human decision and carries no deadline: a 5 s bound
   would reject every prompt the user takes longer than that to answer. A response whose id is not
   pending is dropped. A pending request is retired when its caller is cancelled, and every pending
   request fails when the transport closes. Inbound frames get a length bound no smaller than the
   largest document we will accept (§4).
4. Convar `sv_acp_fs` (`auto` | `off`, default `auto`) is the operator's switch for editor I/O.
   `auto` uses `fs/*` whenever the capability and eligibility rules allow it. `off` never sends
   `fs/*`, whatever the client advertises; it is the escape hatch for a remote editor whose path
   namespace differs from the host's (resolved question 5).

### 2. Eligibility (security boundary)

Editor I/O is used for a path only when all of the following hold. Otherwise the path uses disk
through the authority exactly as today.

- Convar `sv_acp_fs` is not `off`.
- The live ACP session's `cwd` matched the host project root. `validate_session_cwd` rejects a
  `cwd` that does not match, but it accepts a request with no `cwd`, and the session a connection
  starts with was never given one. So eligibility requires a `cwd` that was supplied and matched on
  the `session/new`, `session/load`, `session/resume` or `session/fork` that made the session
  live. Equality is necessary but not sufficient for a shared path namespace; `sv_acp_fs off` covers
  the rest.
- The path canonicalizes through the authority's root resolution (`resolve_existing` /
  `resolve_target`) to a location inside a project root of this session.
- It is a text document (`DocumentKind::Text`, valid UTF-8) at or under the snapshot size cap
  (`omp_tools::read::SNAPSHOT_MAX_BYTES`, 4 MiB).
- It is not an internal resource (`artifact://`, `agent://`, `history://`, `local://`, …) and not
  inside a subagent's copy-on-write view (0007). Subagents never consult the editor.

The path sent to the client is always the canonical absolute path. Paths outside the roots are
never sent to the client, not even to read. Buffer content is untrusted input data, with the same
standing as file bytes: it is size-checked, never interpreted, and prompt-injection exposure is
unchanged. Editor I/O adds **no** permission prompt. A read inside the roots is already free, and
write-back happens only after an authority commit that policy (0006/0028) already approved via
`session/request_permission`.

### 3. Base selection (read-through)

envd's `DocumentHost` keeps a per-session, in-memory **anchor** table: `path → K`. K is the last
editor content known to be incorporated into the authority's lineage. K is set when:

- (a) a buffer read equals disk;
- (b) a commit used buffer content as its base (K = that buffer);
- (c) a write-back succeeded (K = the bytes written).

Anchors are not durable. After resume a path is unanchored.

For an eligible path with `readTextFile`, every Read, and every Edit `prepare`, issues one whole-file
`fs/read_text_file` and computes the **effective base E** against the disk head D (revision Rd):

| Buffer B | Meaning | E | Anchor |
| --- | --- | --- | --- |
| B ≡ D | clean | D | K := D |
| B ≡ K, or B ≡ a retained authority revision | stale-clean (editor has not reloaded) | D (disk wins) | unchanged |
| otherwise, K present | dirty, anchored | `rebase_content(K, D, B)`: the user's delta K→B replayed onto D | K := B on commit |
| otherwise, no K | dirty, unanchored (first contact) | B (buffer wins wholesale) + `<diag severity=warn kind=editor_buffer_unanchored>`; no user prompt | K := B on commit |

- **Equality.** "≡" is byte equality after line-ending/BOM normalization: B is re-encoded to D's
  dominant EOL and BOM before comparing and merging, because editors may expose normalized text
  (client behaviour **unverified**). Disk encoding wins: a buffer that differs from disk only in
  line endings or BOM is clean, and every E, merge and write-back uses D's EOL and BOM (resolved
  question 4).
- **Conflict.** A conflict in the dirty-anchored merge rejects the operation before any effect. It
  returns the existing typed `RebaseConflict` ranges in K's coordinate space (the same convention as
  `DocumentConflict.conflicting_ranges`) as a `<diag kind=editor_buffer_conflict>`, with line ranges
  projected into B and D for presentation.
- **Read.** Read returns E with its hashline tag recorded in the snapshot store as usual. The tool
  element records provenance: `source=editor-buffer` and E's content hash (0008). The
  `acp:<hash>` pseudo-revision is deleted, and the lease's revision is Rd.
- **Edit.** Edit `prepare` sets `base_bytes = E` and keeps `base_revision = Rd`. The existing
  authored-snapshot → base rebase then applies unchanged.
- **Buffer read failure.** An error, a timeout, a malformed or non-UTF-8 answer, or an oversize
  buffer falls back to E = D with `<diag severity=warn kind=editor_buffer_unavailable>`. It never
  fails the tool, not even when `writeTextFile` is advertised (resolved question 2).

### 4. Writes commit through the authority; write-back follows

1. Every agent write (Write, Edit, and the lift of a subagent diff into the project scope) MUST
   commit as an authority transaction with `base_revision = Rd`, the proposed bytes, and
   `STALE_POLICY_REBASE_NON_OVERLAPPING`, exactly as today. If disk moved after prepare (Bash,
   another host, a watcher-adopted save), the authority rebases or rejects with typed
   `DocumentConflict`s. The editor path NEVER writes disk and NEVER replaces the commit. The
   `write_plain` early return is deleted.
2. When the proposed bytes were derived from a dirty buffer, the commit persists the user's unsaved
   buffer edits together with the agent's change. This is the consequence of the buffer being the
   base (see Consequences).
3. **Write-back.** After `TransactionCommitted` (or, for `TransactionPartiallyCommitted`, only for
   the committed operations) and when `writeTextFile` is advertised, the host enqueues one
   write-back per committed text path with the committed bytes R. Queues are per path and FIFO. A
   newer committed revision supersedes a queued one that has not been sent: the latest wins, which
   is safe because R is whole-file. For each write-back:
   - If `readTextFile` is advertised, re-read the buffer B′. If B′ is not B (the base used), send
     `rebase_content(B, R, B′)`. This keeps keystrokes typed during the turn. If that merge
     conflicts, skip the write-back and emit `editor_sync_conflict` with ranges; the editor's own
     dirty-vs-disk handling takes over.
   - Send `fs/write_text_file{sessionId, path, content}`.
   - If `readTextFile` is advertised, read back once. A difference from what was sent is reported as
     `ClientFormatDrift{client_formatted: true, bytes_changed_after_client_format: true}` in the
     tool element. It does not trigger a new commit; if the client saves, the watcher adopts the
     formatted bytes as an external revision.
   - K := the bytes sent.
   - The window between the pre-write re-read and the write stays open, because ACP has no
     conditional write. This is accepted and documented: one round trip.
4. **Ordering.** Commit durable → the tool element carries the committed result → write-back →
   the tool settles. A later tool call in the same turn therefore observes a synced editor. Parallel
   tool calls on different paths proceed concurrently. Deletes and moves have no ACP method; the
   editor learns about them from its file watcher. Creates are written back like any other write.
5. **Failure.** A write-back error or timeout never un-commits and never fails the tool. The tool
   settles `ok` with `<diag severity=warn kind=editor_sync_failed revision=…>`. K becomes the base
   buffer B, so the next read sees the stale buffer as stale-clean and resolves to disk.
6. **Cancellation** (0011).
   - Before the commit, an interrupt abandons pending `fs/read_text_file` requests: their ids are
     retired and late answers are dropped. The call settles never-started, with no write-back.
   - After the commit the effect is certain. The tool settles immediately with the committed result
     and `editor_sync_pending`, and the write-back continues on the session's write-back queue.
   - The queue is drained, bounded by `sv_acp_fs_timeout`, on `session/close`, session switch and
     graceful `shutdown`. It is dropped on EOF.
7. In-process Bash writes are not pushed to the editor. They reach disk through the interpreter's
   policy path, and the editor observes disk like any external change.

### 5. Journal and projections

Durable state stays in the journal (0003). The tool element records `source`, buffer hash, conflict
ranges, and editor-sync state/diags as props of the same element (0008). The bytes the model saw
are already in the result. Replay, spectators and rewind never call the client. Anchors and queues
are session runtime state and are never a second source of truth.

### 6. Terminal: `terminal/*` stays unused (T1)

omp MUST NOT call `terminal/create` or any `terminal/*` method, and MUST NOT route shell execution
to the client. The dormant shell-escape seam is removed in a clean cutover:

- `AcpExecBackend`/`AcpExecSlot`/`SelectedShellRunKind::Acp`;
- `AcpExecQuery`/`AcpExecEvent`/`AcpExecCancel` and `AcpBind.exec` (field numbers `reserved`);
- `ProjectEnvironment::bind_acp_exec`;
- the `sv_acp_routing` convar and `AcpRouting`;
- the "ACP routing" prompt text in `crates/tools/src/shell.rs`;
- the `terminalId` → `{type:"terminal"}` mapping in `acp_events.rs`.

Bash output continues as `tool_call_update` content. T2 is rejected. T3 is not pursued now; it may
be proposed in a later record once a client's display-only mechanism is verified (resolved question
7).

## Consequences

- The model reads and edits what the user sees. The authority stays the only writer of disk, so P1's
  guarantees carry over.
  - Editor divergence enters as the *proposed content* of an ordinary `TextMutation` against a disk
    revision. That is the same path P1 proves for two concurrent hosts, with the same
    `rebase_content` machinery and the same typed conflicts.
  - The docserver actor's invariants are untouched: head equals persisted bytes, provisional state
    is never a head, and the fingerprint is rechecked before rename.
- 0001 modes:
  - *Multiplexed workspace*: TUI, no client, so disk as today.
  - *Remote driver*: this record. It is only valid while the client and host share a path namespace
    (resolved question 5). `sv_acp_fs off` disables editor I/O when it is not.
  - *Spectator*: views are projections of the journaled element. Only the controlling ACP
    connection serves `fs/*`.
  - *Factorio*: no editor, so the capability is absent and the behaviour is unchanged.
- Cost accepted: an agent commit on a dirty buffer saves the user's unsaved edits to disk.
- Cost accepted: one extra client round trip per Read/Edit on eligible paths, and up to three per
  write-back (re-read, write, read-back).
- Cost accepted: a residual lost-keystroke window of one round trip, because ACP has no conditional
  write.
- Cost accepted: first-contact dirty buffers win over disk and can revert an external change made
  before the first read. This is surfaced as a diag.
- Prohibited: editor writes replacing authority commits; `acp:`-style revisions unknown to the
  authority; `terminal/create` for execution or mirroring; `fs/*` outside project roots or inside
  subagent views; a second permission prompt for write-back.

## Status in omp

**Partially implemented (plan steps 1–3).** Step 1: `crates/app/src/acp_client.rs` (typed
capabilities, one request table, gated `fs/*`). Step 2: the base selection of §3 lives in
`crates/envd/src/editor_base.rs`; the connection binds its editor through
`ProjectEnvironment::editor_documents` (driver: `ComposedInference::editor_documents`), one anchor
table per `AcpBind` of that environment connection, with `AcpBind.fs_timeout_ms` carrying
`sv_acp_fs_timeout` in place of `ACP_QUERY_TIMEOUT`. The `write_plain` bypass and the `acp:`
pseudo-revision are deleted. Two refinements over §3 as written: a first-contact (unanchored) read
also sets K := D, so a disk change made after that read is merged or conflicts instead of being
reverted by the buffer; and an agent Write anchors the buffer it superseded (K := B, read before the
commit). A conflicting merge rejects an Edit with typed ranges; a Read falls back to D with
`editor_buffer_conflict`. Notebooks (`.ipynb`) are not eligible.

Step 3 (write-back, §4.3–§4.6): after a durable commit, the Edit and Write hosts in
`crates/envd/src/tool_document.rs` start one write-back per committed text section
(`editor_base::start_write_backs`) and hand the tool an `omp_tools::editor_sync::EditorSync`; the
tool builds its result from the commit, then waits for the write-backs before it settles. How it
was built, and where it differs from the text above:

- **The queue lives with the editor's connection, not in the daemon's `DocumentHost`.** The daemon
  sends one `AcpWriteQuery{path, content = R, base = B}` per committed path and awaits an
  `AcpWriteBack` outcome (`written{merged?, read_back?}` | `conflict{ranges}` | `superseded`). The
  per-path FIFO, supersession, pre-write re-read and merge, `fs/write_text_file` and read-back run
  in `omp_envd::editor_sync::EditorBackend`, which the ACP adapter creates per binding (its
  `EditorFiles` are the connection's gated `fs/*` requests, each naming the session the binding was
  made for). The daemon's per-invocation editor route dies with the invocation, while §4.6 needs a
  write-back that outlives an interrupted call and is drained or dropped at the adapter's own
  lifecycle events; owning the queue there gives both. The daemon bounds its wait for one
  write-back at six editor deadlines (one write-back ahead of it on the path, plus its own three
  round trips).
- **Wire.** `AcpBind` gains `read_text`/`write_text` (the editor's capabilities),
  `AcpWriteQuery` gains `optional base`, and `AcpDocumentAnswer` gains the `write_back` outcome
  (`AcpWriteBack`, `AcpWritten`, `AcpWriteConflict`, `AcpWriteSuperseded`).
- **Binding (§1.2).** The adapter now binds when `readTextFile` *or* `writeTextFile` is advertised
  (step 2 bound on `readTextFile` only). A write-only editor is never read: disk is the base and R is
  written as is, with no re-read and no read-back.
- **Anchors.** K := B still happens at the commit, so a failed or skipped write-back leaves K = B
  (§4.5); a successful one sets K := the bytes sent, and a write-back that finishes late never
  overrides a newer one's anchor. A queued write-back whose base is a buffer the previous
  write-back of the same path replaced has that base swapped for what the previous write-back left,
  so the queue's own write is never mistaken for user typing.
- **What is written back.** Written sections of an Edit and plain Writes (creates included, written
  as is). Moves and deletes are not (no ACP method). A commit whose pre-commit buffer read failed
  is not written back: with no B known, a blind write could overwrite typing the environment never
  saw. Conflict splices (`conflict://`), archive members and other special writes are not written
  back; omp has no subagent-diff lift into the project scope yet.
- **Partial commits.** The Edit host compensates a `TransactionPartiallyCommitted` by rolling the
  landed operations back, so no committed operation is left to write back; the write-back is
  selected per committed section, and a rolled-back partial commit writes nothing
  (`production_host_rolls_back_landed_prefix_and_keeps_registers_unpublished`).
- **Notices.** `editor_sync_failed` and `editor_sync_conflict` (ranges in B's normalized byte
  coordinates, with the lines in B and R) are warnings; `editor_sync_pending` is informational and
  is also what a call reports when the daemon stops waiting because the editor's connection went
  away. Client format drift is the informational diag `client_format_drift`, whose text states
  `client_formatted=true bytes_changed_after_client_format=true`; no `ClientFormatDrift` message is
  added to the tool element.
- **Drain.** Before `session/close`, a session switch and graceful `shutdown` the adapter waits for
  the queue, bounded by `sv_acp_fs_timeout`; meanwhile it keeps routing client responses (approvals
  included) and defers any request to after the switch, in order. EOF fails every pending request,
  which drops the queue. On `shutdown` the drain runs before the transport is closed and before the
  active turn is awaited.

Step 4 (terminal cutover) is open.

### Implementation plan (PR-sized)

1. **Capability negotiation + types** (`crates/app`). Typed `ClientCapabilities` and `fs/*`
   request/response structs. A unified outbound request table (subsuming `PermissionRequests`) with
   the `sv_acp_fs_timeout` deadline for `fs/*`. The `sv_acp_fs` and `sv_acp_fs_timeout` convars and
   the per-session eligibility state (§2). An inbound frame length bound. No binding yet, so no
   behaviour change.
2. **Read-through as document base** (`crates/envd`, `crates/driver`, `crates/app`). Anchor table
   and effective base E in `DocumentHost`, Read provenance, Edit `prepare` using E with
   `base_revision = Rd`. Delete the `acp:` pseudo-revision *and* the `write_plain` early return, so
   binding never activates the bypass. Bind documents in ACP mode when `readTextFile` is
   advertised and the session is eligible (§2).
3. **Write-back propagation** (`crates/envd`, `crates/app`). Per-path FIFO queue with supersession,
   pre-write re-read and merge, `fs/write_text_file`, read-back drift → `ClientFormatDrift`,
   ordering before settle, the post-commit cancellation handoff, and drain on close/switch/shutdown.
   Enabled by `writeTextFile`.
4. **Terminal cutover** (`crates/envd`, `crates/env`, `crates/proto`, `crates/tools`, `crates/app`).
   Remove the exec seam and convar listed in §6 and reserve the proto fields. `terminal` stays
   parsed and ignored. This step does not depend on steps 1–3 and lands as its own change.

### Test plan (owning seams)

- **`crates/app/tests/acp_spine.rs`**, with a fake client over the existing NDJSON harness that
  advertises the caps and answers `fs/*` from an in-memory buffer map:
  - Without caps, no `fs/*` request is ever sent, even with `terminal: true`.
  - With caps, `sv_acp_fs off` or a session without a matched `cwd` sends no `fs/*` request.
  - Response correlation works alongside `session/request_permission`.
  - A read that is never answered times out within the bound.
  - Unknown or late response ids are dropped.
  - After `session/close` or a switch, no request names the old session.
  - `terminal/*` is never emitted.
- **`crates/envd`** (fake `AcpDocumentBackend`; `lib.rs`'s `FormattingDocuments` is the precedent):
  - Table-driven base selection: clean, stale-clean via K, stale-clean via retained revision,
    dirty-anchored merge, dirty-anchored conflict → typed ranges and no effect, unanchored + diag,
    EOL/BOM-only difference ≡ clean, oversize/non-UTF-8/timeout → disk + diag.
  - Commits go through the authority: disk bytes equal the committed revision and the watcher sees
    no spurious external event.
  - A regression test that Write with a bound editor still commits a revision.
  - Write-back runs after commit and before settle; partial commits write back only committed ops;
    supersession; a user edit during the turn is merged by the pre-write re-read; drift is reported;
    write-back failure → `ok` + diag; cancellation before commit → no write, after commit → queued
    write completes.
  - proptest: B ≡ K ⇒ E = D; D = K ⇒ E = B.
- **`crates/e2e`**: add a P1 editor-buffer case beside `p1_doc_race.rs`. It runs a real docserver
  and the real `omp acp` binary with a fake editor and a scripted model.
  - Dirty buffer, a concurrent non-overlapping disk edit from a second host, and an agent Edit:
    final disk = user delta + host delta + agent delta, the editor received exactly those bytes,
    and ordering is commit → write → settle.
  - The overlapping variant is rejected with typed ranges, with no disk or editor write.

### Resolved questions

The owner resolved the proposal's open questions on 2026-09-29. The Decision above states each
outcome as a rule; this list keeps the reasoning.

1. **Dirty buffer on commit: D3.** An agent commit on a file with unsaved editor changes also
   saves those changes to disk. The buffer merged with disk is the base, and the commit goes
   through the authority. D4 would keep them unsaved, but it conflicts exactly when the agent works
   on what the user is typing, and clients that save on `fs/write_text_file` persist the buffer
   anyway.
2. **Buffer-read failure: fall back to disk** with `<diag severity=warn
   kind=editor_buffer_unavailable>` (§3), even when `writeTextFile` is advertised. Failing closed
   would let a slow or broken client block every Read and Edit, and the authority commit is safe
   against disk either way.
3. **First-contact dirty buffer: the buffer wins** with `<diag severity=warn
   kind=editor_buffer_unanchored>` and no user prompt. The buffer is what the user sees, and a
   prompt per file would add a second permission surface that §2 excludes.
4. **Encoding: disk wins.** A difference only in line endings or BOM counts as equal, and E, merges
   and write-backs use D's EOL and BOM. Editors may normalize the text they expose; re-encoding to
   disk keeps a line-ending-only difference from rewriting the file.
5. **Remote-editor path namespace.** Editor `fs/*` is used only when the ACP session's `cwd`
   matched the project root and convar `sv_acp_fs` (`auto` | `off`, default `auto`) is not `off`.
   `cwd` equality is necessary but cannot prove the client and the host share a filesystem, so
   `off` is the operator's escape hatch for a remote editor (§1, §2).
6. **Timeout: one convar.** `sv_acp_fs_timeout` (default 5 s) bounds reads and writes alike.
   It stays one knob until a client shows that reads and writes need different bounds.
   `session/request_permission` is not bounded by it, because it waits for a human (§1).
7. **Terminal: T1.** The dormant `AcpExec*` seam and `sv_acp_routing` are deleted (§6, plan step
   4). T3 is not pursued now; a later record may propose it once a client's display-only mechanism
   is verified.

## References

- 0001 (remote-driver and spectator rows), 0003 (journal authority), 0006 (host policy), 0007
  (subagent CoW views), 0008 (one element; diags), 0009 (central bounding), 0011 (cancellation),
  0028 (in-process Bash)
- `AGENTS.md` — Locked Deviations from pi (document authority, in-process shell)
- `crates/app/src/acp_mode.rs`, `crates/app/src/acp_events.rs`, `crates/app/tests/acp_spine.rs`
- `crates/envd/src/docs.rs`, `crates/envd/src/tool_document.rs`,
  `crates/envd/src/tool_read_sources.rs`, `crates/envd/src/tool_shell.rs`,
  `crates/envd/src/exec_settings/acp.rs`, `crates/envd/src/server.rs`,
  `crates/envd/src/docserver/{rebase.rs,actor.rs}`,
  `crates/proto/proto/omp/{document/v1/document.proto,env/v1/env.proto}`,
  `crates/e2e/tests/p1_doc_race.rs`
- v1 (`origin/main`): `packages/utils/src/acp/{protocol.ts,connection.ts}`,
  `packages/coding-agent/src/session/client-bridge.ts`,
  `packages/coding-agent/src/modes/acp/acp-client-bridge.ts`,
  `packages/coding-agent/src/tools/{acp-bridge.ts,read.ts,read-summary.ts,bash.ts}`
- Agent Client Protocol, agentclientprotocol.com (file system and terminal sections; not vendored)
