# 0009. Output is bounded centrally; full results become artifacts

Status: accepted
Date: 2026-09-02
Area: runtime

## Context

A raw tool callback in pi and omp v1 returns an arbitrary amount of text. Both codebases grew
helpers around that fact — a truncation utility, a `maxBytes` default, a formatted notice — and left
them optional. The result was a distribution of near-duplicates: each tool author who remembered
wrote a slightly different notice, in prose, appended to the data:

```ts
text += `\n${theme.fg("warning", `[Truncated: ${truncation.outputLines} lines shown (${formatSize(truncation.maxBytes ?? DEFAULT_MAX_BYTES)} limit)]`)}`;
```

Three failures recur from optional bounding:

1. Tool authors each invent a truncation notice; the model learns none of them reliably.
2. The notice is mixed into the data. The model has to guess where tool output ends and harness
   commentary begins; a themed ANSI string is not a contract.
3. Code mode and `Eval` wrap tool results in another layer that also truncates, so the same result
   passes through N+1 independent limits with N+1 notices — and the innermost one may already
   have discarded what the outer consumer needed.

The sandbox boundary (0006) sharpens the requirement: a misused `Read` on the untrusted side can
stream 2 GB back, and nothing on that side can be trusted to stop it.

## Decision

- The runtime MUST retain the complete output of a call as an artifact and derive bounded
  projections for the model, the transcript, and remote clients. Consumers receive a projection
  plus an address; nobody receives the full result inline by default.
- The default limit MUST belong to the library. Tool authors NEVER implement truncation. A tool
  that emits more than the inline threshold is spilled by the call-outcome path, not by the tool.
- An explicit `notrunc` escape hatch MAY exist for a caller that needs the whole result inline. It
  is a caller decision recorded on the call, not a tool-level default.
- Truncation and other harness notices MUST be structured (`<diag severity="…">` in the call element,
  0008) and NEVER interpolated into the result body.
- Bounding MUST happen exactly once per result. Code mode, `Eval`, and any other wrapper consume the
  already-bounded projection and its artifact address; stacking a second limit is prohibited.
- Bounding MUST be enforced on the host side of the trust boundary, before the untrusted stream can
  exhaust host memory or context.

## Consequences

- Every tool gets correct, uniform truncation for free; the model sees one notice format.
- Full output is never lost: `Read` of the artifact address recovers it (0027), for the agent, the
  user, or a test.
- Prohibited: `maxBytes`-style parameters implemented per tool; string-slicing in tool bodies;
  prose notices; wrappers that re-truncate.
- Cost accepted: artifact storage and a content-addressed blob path become mandatory infrastructure
  for even the simplest deployment.

## Status in omp

**Status: Implemented.** Bounding happens once, centrally, on the host side, with full results retained as `artifact://` blobs. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Central bound and spill: `crates/agent/src/dispatch.rs` writing to the project/session blob store in `crates/journal/src/blob.rs`; `notrunc` is a recorded caller request capped at 8 MiB.
- Host-side bound before untrusted bytes enter host memory: `crates/envd/src/exec.rs` (64 KiB ordinary, 8 MiB complete-request limits) and verified `OutputProjection` receipts over `env/v1`.
- Notices are structured: the spill notice is `Diag::info(DiagKind::OutputBounded)` with `artifact` and `omitted`; per-tool footers moved to `Ev::Diag`.
- Journal-derived GC roots keep spilled results alive: `crates/journal/src/gc.rs`.
- Environment-run tools (2026-10-08): a native tool's `env/v1 Verdict` carries the tool's own model-facing parts. `send_native_outcome` in `crates/envd/src/server.rs` (called by `forward_native_event` on a native `Done`) projects the verdict through the environment registry (`Registry::project_verdict`, the unbounded caps `Dispatcher::finish` uses), converts the parts to thread parts (JSON as text; media as its alternative text, then the blob, retained for the invocation's delivery; a blob that cannot be retained is left out with a warning and its alternative text stays), bounds them by the output request and takes `is_error` from the projection; a verdict the tool cannot project is published as an effects-unknown abort. Before this, native verdicts published no parts and the driver guessed them from a top-level `parts` field of the payload, so every environment-run tool except `read` (bash, write, edit, …) reached the model as an empty tool message. The driver now fills parts only for harness-owned aborts, policy denials and rejected arguments (`harness_parts` in `crates/driver/src/headless/kernel.rs`). Open: the environment does not send source-visibility spans, so `authorize_visibility` never runs for environment-run tools.

### Implementation notes (carried over)

Primary implementation: `crates/agent/src/dispatch.rs`. Central output and line bounds spill full results to the same project/session `omp_journal::blob` store used by provider media, user attachments, and compaction summaries. Journal-derived GC roots the complete retained result across transcript, model, debug, and remote projections; a put-before-journal grace window and namespace lock prevent premature collection. `notrunc` is a caller request for complete inline output up to a fixed 8 MiB host security ceiling; it never disables host memory bounds, and larger results carry the same typed artifact-backed projection receipt as ordinary bounded calls. Source-backed projections carry typed byte spans through `omp-tool`; after the single central bound, the dispatcher returns only fully retained source lines in a `VisibilityReceipt`. `grep` stages revision-pinned bytes with the document authority before dispatch but authorizes lines only from that receipt, so it has no local byte heuristic or truncation footer. `crates/envd/src/exec.rs` enforces 64 KiB ordinary and 8 MiB complete-request limits before shell bytes enter the bounded event channel, while staging the complete byte stream directly into the environment CAS. Native-device and process-worker verdicts retain the full structured outcome in that CAS, bound model-facing parts at the environment boundary, and publish typed `OutputProjection` facts over `env/v1` (native-device verdicts have carried model-facing parts only since 2026-10-08; see Status). The driver verifies the request, byte counts, ceiling, and artifact identity before adopting a verdict. Its cross-host replication retains one staged session-CAS write across interrupted ranges, resumes from the last persisted byte, and re-verifies the whole digest before publication. Detached-job settlement copies a runtime-spill artifact into the session CAS before the journal patch names it, so restart reconciliation cannot publish a dangling reference. The remote environment keeps durable session/invocation-scoped delivery leases until replication is acknowledged and journal-derived roots permit collection.

Harness notices are structured, not prose. The dispatcher's spill notice is
`Diag::info(DiagKind::OutputBounded)` with `artifact` and `omitted` attributes; the address is no
longer appended to the result parts, because the model projection renders the diag itself (0008).
Per-tool pagination footers, path-recovery notes, search caveats, edit warnings, scraper provenance,
and the like all moved to `Ev::Diag` with the shared `DiagKind` vocabulary
(`Pagination`/`RangeOutOfBounds`/`SummaryElided`/`LimitReached`/`PathRecovered`/`Conflicts`/…), each
carrying `continuation` and `omitted` where a next slice exists. The sloppy hashline parser no longer
recognizes read-output footers, since they cannot appear in pasted result text.

## References

- The Harness Playbook, "The runtime" — "Limits are part of the primitive", "Bound output once"
- 0002 (helpers become mandatory), 0006 (bounded streams across the trust boundary), 0008 (`<diag>`
  channel), 0027 (`Read` materializes `artifact://`), 0025 (Code mode / `Eval` as consumers)
- `crates/tool/src/lib.rs` (`CallOutcomeSpill`, `ThresholdWriter`, `Spilled`),
  `crates/agent/src/dispatch.rs` (`DispatchPolicy`, the central bound). `SubagentDisposition`
  (formerly `crates/agent/src/subagent.rs`) no longer exists; it was removed by `d98ed242f5`, and
  child settlement now lives in `crates/driver/src/subagent/`
