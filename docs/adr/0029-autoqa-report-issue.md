# 0029. Agents get a bug-report path

Status: accepted
Date: 2026-09-02
Area: tools

## Context

Products give users a way to report problems. Agents had none: when a projection hid the data a
model needed, a `--help` was wrong, or a tool result contradicted its documentation, the only
trace was a wasted turn in a transcript nobody would read. Tool design and deployed behaviour were
disconnected.

omp added `report_issue` a month into the fork, before Anthropic shipped an equivalent. It
collects, fully autonomously, what agents found useful, confusing, or erroneous about a tool.

The reports are noisy. Codex, for example, blames `Read` or the LSP tool for external file edits
when its own rename went wrong. Those misattributions are cheap to filter, and once filtered the
remainder is a large, direct signal about which operation confuses models, which projection hides
needed data, and which repair belongs in the harness.

## Decision

1. A `report_issue` path MUST be available to the agent in every session where AutoQA is enabled,
   reachable through the stable surfaces (0025), never as a permanent roster tool.
2. The prompt MUST instruct the model to report whenever a tool or device result contradicts its
   documented behaviour for the supplied parameters, and MUST state that false positives are
   acceptable — the filter is cheaper than a missed signal.
3. A report MUST carry the tool identity (`name@rev`, 0026), the session, and a structured verdict
   so reports can be grouped per contract revision.
4. Reports are reviewed with a misattribution filter first; the filtered residue feeds tool
   design.

## Consequences

- The loop between tool design and deployed behaviour closes without human transcript reading.
- Prohibited: silently dropping agent complaints; a report tool that occupies grammar (0024).
- Cost accepted: report volume and known-noisy models; the filter is part of the workflow.

## Status in omp

**Status: Partially implemented.** `report_issue@1` and the AutoQA prompt guidance exist and delivery is consent-fenced; the misattribution filter is not found. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Device: `crates/envd/src/report_issue.rs` (redacted local store, tool identity, session and structured verdict) mounted through `crates/envd/src/tools.rs` and reachable via `dyn`; card in `crates/chat/src/cards/report_issue.rs`.
- Prompt: `AUTO_QA_PROMPT_GUIDANCE` in `crates/tools/src/device.rs` ('False positives are acceptable ...'), injected as `auto_qa_guidance` in `crates/driver/src/headless/kernel.rs`.
- Delivery: `crates/driver/src/telemetry_upload.rs`, consent-only, endpoint overridable with `OMP_AUTO_QA_PUSH_URL`.
- Gap: no code or setting for a misattribution filter (grep for `misattribut` finds nothing) and no AutoQA enable convar was found.

### Implementation notes (carried over)

Primary implementation: `crates/envd/src/report_issue.rs`, mounted through
`crates/envd/src/tools.rs` and documented by `crates/tools/src/device.rs`. `report_issue@1` records
an exact session, device path, canonical revision, and bounded structured verdict in the redacted
local issue store; typed results state that delivery requires a separate user-owned consent action.
The consent-fenced delivery worker is `crates/driver/src/telemetry_upload.rs`. Gap: report-store
misattribution filtering is not proved.

## References

- The Harness Playbook, "The tool surface" — "AutoQA: give agents a bug-report path"
- 0025 (`dyn` devices), 0026 (versioned identities), 0024
- `crates/tools/src/device.rs`
- `crates/envd/src/report_issue.rs`
- `crates/cache/src/telemetry_cache.rs`
- `crates/driver/src/telemetry_upload.rs`
