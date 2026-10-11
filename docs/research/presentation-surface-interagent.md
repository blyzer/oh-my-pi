# OMP² Presentation, Surface, and Inter-Agent Communication Research

Status: research; production refactor **not authorized**
Date: 2026-10-11

## Executive conclusion

The current architecture already separates most durable semantics from presentation:

- `omp-journal` owns durable typed records and replay data.
- `omp-session` folds journal entries into the materialized DOM.
- `omp-chat` projects session state into semantic blocks/cards.
- `omp-tui` owns retained rendering, terminal policy, and final ANSI materialization.
- TSP is an adapter/presentation path, not an agent bus or journal replacement.
- Workpool/IRC traffic is an internal typed envelope, not demonstrated Internet Relay Chat compatibility.

The evidence supports a **minimal evolutionary path**: preserve the journal/session/TUI boundaries, extract only repeated semantic card/projection policies where measurements show duplication, and keep TSP as a surface adapter. Do not introduce a new canonical Surface IR, WebGPU renderer, adaptive transport router, or ABI layer during this research cycle.

Implementation remains blocked pending review of this report and measured baselines.

## Evidence map

### Durable state and replay

- `crates/journal/src/data.rs`: `IrcTraffic`, `IrcDirection`, and typed `WorkpoolObservation` are serializable journal data. `WorkpoolObservation::try_from` rejects the wrong direction and missing/invalid fields.
- `crates/agent/src/dispatch.rs`: `EnvEvent::IrcTraffic` is appended through `append_irc_traffic`; the journal entry is the durable event source.
- `crates/agent/src/env.rs`: the event carries an `Arc<IrcTraffic>` so mailbox fan-out does not copy the payload; `append_irc_traffic` stores typed raw JSON in the DOM transaction.
- `crates/chat/src/project.rs`: journal/session state is projected into `RenderedBlock` values with stable keys, block kinds, stream text, and finalization state.
- `crates/chat/src/transcript.rs` and `crates/tui/src/slots.rs`: the transcript/slot model preserves exactly-once committed history while allowing mutable or append-only active blocks.
- `docs/adr/0034-transcript-is-a-protocol.md`: the journal is canonical; presentation state is not a second transcript store.

### Rendering

- `crates/chat/src/notices/irc.rs`: `traffic()` parses a typed DOM notice; `traffic_text()` creates compact text; `traffic_card()` selects the workpool transition/result card or generic IRC notice card.
- `crates/chat/src/notices/workpool.rs`: specialized cards intentionally reject untyped prose as a worker result. This is an authentication/correctness boundary, not mere formatting duplication.
- `crates/chat/src/cards/mod.rs`: typed `CardView` consumes journaled outcomes and faults; cards do not treat arbitrary projection JSON as authoritative typed results.
- `crates/tui/src/rich.rs`, `crates/tui/src/slots.rs`: styled runs are pushed through `RichSink`; ANSI is decomposed at external-text ingress and re-emitted at final terminal materialization.
- `docs/adr/0030-one-pass-rendering-pipeline.md`: the one-pass sink architecture replaces the old string-array/reparse pipeline and is marked implemented.
- `docs/adr/0031-typed-component-model.md`: `dom!`/`view!` provide typed semantic construction; the ADR itself records that runtime-markup control flow and a web projection are not proven.
- `docs/adr/0032-presentation-policy-in-the-renderer.md`: theme, charset, icons, truncation, and pacing belong to renderer context, not providers/cards.

### TSP and surfaces

- `docs/adr/0041-native-surfaces-over-tsp.md`: TSP is a second presentation of the same projection. Cell rendering remains the fallback. TSP wire/probe/input handling stays in `omp-tui`; the presenter lives beside `Host` in `omp-chat`.
- `crates/tui/src/input.rs` and `crates/tui/src/graphics.rs`: APC/OSC responses and probing already enter the single terminal mailbox.
- `docs/adr/0041-native-surfaces-over-tsp.md` records the current phase-0 wire implementation and explicitly says no presenter existed at the recorded revision. Later implementation status MUST be rechecked before changing the decision.

### Inter-agent/workpool communication

- `crates/driver/src/hub.rs`: peer delivery emits typed incoming traffic and optional display-only relay traffic; the ordinary peer message and the journal observation are separate legs.
- `crates/driver/src/workpool.rs`: `WorkpoolRegistry` binds a pool to a live owner endpoint; `WorkpoolProducer` authenticates targets, receipt ownership, pool lifecycle, and terminal states.
- `crates/driver/src/workpool_scheduler.rs`: scheduler observations are staged and delivered through the owner mailbox with bounded retry/backpressure and cancellation.
- `crates/driver/tests/workpool_producer.rs` and `crates/driver/tests/hub_sessions.rs`: tests cover duplicate pools, foreign replies, stale owners, mailbox pressure, ordering, relay suppression, typed replay, and authenticated worker results.
- The current evidence shows in-process mailbox/session communication. No source inspection in this pass establishes that `IrcTraffic` itself is a TCP, IRC, QUIC, or cross-machine protocol.

## What “IRC” means here

Observed behavior supports classification as **IRC-inspired internal message/event semantics**. The code uses `IrcDirection` and `IrcTraffic` as a typed domain envelope for incoming messages, autoreplies, relays, and workpool observations. It does not establish RFC IRC wire compatibility, server negotiation, IRC authentication, or an IRC transport. Naming should not be used to justify a network transport rewrite.

## Presentation alternatives

| Alternative | Evidence-based assessment | Decision status |
|---|---|---|
| A. Preserve current architecture; extract only measured duplication | Retains journal authority, one-pass rendering, typed cards, and TUI ownership | **Recommended baseline** |
| B. Extend `omp_dom` with more semantic nodes/cards | Already compatible with `dom!`, typed `CardView`, and TSP projection hooks | Candidate incremental work |
| C. Add a new renderer-independent Surface IR | No demonstrated gap: `RenderedBlock`, `CardView`, DOM, and TSP projection already provide seams | Defer/reject until benchmark proves need |
| D. Make Tern canonical | Conflicts with cell fallback, journal ownership, and terminal independence | Reject |
| E. Hybrid native paths plus semantic adapters | Already the direction of ADR 0041: semantic projection first, component lowering, then `rows` fallback | Preserve and complete incrementally |

### Reusable components

Centralize only behavior that is genuinely policy-independent: semantic status/tone mapping, stable block identity, bounded result projection, and shared metadata formatting where multiple cards have identical invariants. Keep workpool transition/result cards specialized because their parsing and authentication rules are correctness-sensitive. Do not replace typed cards with a generic schema solely to reduce function count.

### Presentation model

A `WorkpoolPresentation` wrapper is justified only if two or more surfaces need the same semantic projection and the wrapper avoids copying large payloads. Current `RenderedBlock` + `CardView` already supplies most of this role. A second aggregate tree would add allocation, ownership, and replay complexity. Prefer borrowed views or enums over a cloned universal presentation model.

## Surface and Tern evaluation

Official TSP documentation describes a live semantic node tree inside a terminal pane, atomic frame updates, stable node IDs, node kinds/props, input events, and handshake/capability negotiation. Repository ADR 0041 records APC framing, credits/ack flow, `settle`, `open/close/adopt`, and fallback to `rows`/cell rendering. These facts support:

- TSP as a **presentation renderer/remote presentation protocol**.
- TSP as unsuitable as the durable journal, agent message bus, tool authorization channel, or remote execution protocol.
- One-way transcript/event presentation and interactive surface events as distinct from agent communication.

TSP should consume projections and return scoped UI actions. A TSP client MUST NOT receive tool authority merely by receiving surface frames. The existing single terminal mailbox and session/controller authorization boundary should remain the authority path.

### WebGPU

No implementation evidence justifies WebGPU now. It would add text shaping/font fallback, input/accessibility, clipboard/selection, GPU resource, packaging, and headless testing requirements. A GPU backend is a later surface experiment, not a replacement for `omp-tui` or TSP. First require a representative workload and a measurable latency/CPU/memory win under equivalent accessibility and text correctness.

## Communication alternatives

| Option | Assessment |
|---|---|
| Existing typed in-process mailbox/session routing | Preserve. Current tests cover identity, ordering, backpressure, cancellation, and result authentication. |
| One semantic protocol with multiple transports | Potential future direction only if a real process/machine boundary is added. The semantic envelope should be separated from transport at that seam, not preemptively. |
| Tokio channels everywhere | Not aligned with repository rule: `flume` mailbox plus `tokio::watch` priority signals. No need to introduce a parallel queue. |
| Unix sockets | Appropriate for a future local daemon boundary, but not a substitute for current in-process authority. Needs framed protocol, auth, reconnect, and replay tests. |
| QUIC/Iroh/gRPC/NATS | No demonstrated current workload justifies operational/dependency cost. Evaluate only with a concrete remote-agent requirement. |
| Stable C ABI | Separate native extension ABI concern; not a message transport. Do not use it to solve agent communication. |
| Shared memory/mmap | Not justified for current typed, durable, modest control/event messages; artifact/blob CAS already handles large payloads. |

## Delivery and journal ordering

The safe invariant is: typed event is validated, journal commitment establishes durable identity, then delivery/acknowledgment reports the result to the sender. The existing `committed: flume::Sender<bool>` and `WorkpoolProducer` tests indicate that this distinction is intentional. Any future transport must preserve:

- stable event/message identity;
- producer/owner authentication;
- deterministic ordering for replay;
- explicit acknowledgment and cancellation;
- bounded backpressure;
- idempotent retry or deduplication;
- no conversion of scheduling observations into model-visible worker results;
- no presentation-only event becoming durable domain history by accident.

No evidence supports parallel independent delivery and journal persistence for durable events without a transaction/ack protocol. Do not decouple those operations merely for conceptual layering.

## Performance status and benchmark plan

Existing ADRs provide historical evidence for the one-pass rendering design, but not a current apples-to-apples benchmark of the proposed alternatives. Conclusions about a new IR or transport are therefore unverified.

Minimal reproducible benchmark plan:

1. Generate deterministic journal fixtures containing 10, 100, 1,000, and 10,000 cards; include streamed text, workpool transitions, large results, collapsible cards, and malformed/untrusted text.
2. Measure cold replay-to-first-frame, incremental update latency, allocations, peak RSS, and CPU for current DOM → chat projection → TUI rendering.
3. Measure TSP projection separately: frame bytes, append frequency, credits exhausted, and time to first semantic node. Do not compare TSP bytes to ANSI without recording semantic-equivalence and client layout work.
4. Measure in-process traffic latency/throughput under mailbox pressure and worker cancellation; include journal append and replay correctness.
5. Only then prototype a borrowed presentation view or transport adapter. Require equal journal/replay/security behavior.
6. Record environment, toolchain, fixture digest, and results under the existing P8/non-gating measurement conventions. Do not invent a pass threshold before baseline data exists.

## Classification

- **Preserve:** journal-first session state, typed `IrcTraffic`, workpool authentication, one mailbox, `flume` + `watch` priority, retained TUI, one-pass RichSink, semantic `dom!`/CardView, TSP fallback behavior.
- **Optimize:** measure and reduce repeated metadata/card projection work; add benchmark fixtures and instrumentation; improve TSP presenter only behind existing phase gates.
- **Extract:** only shared semantic policies proven duplicated across cards/surfaces.
- **Decouple:** introduce a transport seam only at a real process/machine boundary; keep semantic messages typed and static where possible.
- **Defer:** WebGPU, universal Surface IR, adaptive transport routing, ABI redesign, generic card schema.
- **Reject:** Tern as journal/agent bus, real IRC rewrite based on naming, and presentation protocol granting execution authority.
- **Replace:** none supported by current evidence.

## Review-gated roadmap

1. Add/verify deterministic presentation and communication benchmark fixtures; no production behavior change.
2. Verify current ADR 0041 phase status and complete only the approved TSP presenter phases with PTY/fake-Tern tests.
3. Extract a borrowed semantic presentation view only if benchmark data demonstrates repeated conversion/allocation across TUI and TSP.
4. If a real same-host or remote agent boundary is approved, specify one versioned semantic message contract and one transport adapter; preserve journal-before-ack and authorization semantics.
5. Re-run replay, workpool-authentication, cancellation, TSP fallback, and real-PTY suites.
6. Propose a new accepted ADR only after measurements and owner review. Until then, this report is research evidence, not authorization to refactor.

## Unresolved questions

- Current `omp2` phase-1 TSP presenter status after the merged phase work must be checked against the working tree before implementation planning.
- The exact Tern protocol version/capability set supported by the target client must be pinned in conformance fixtures.
- No current benchmark establishes a benefit for a new Surface IR, WebGPU, or remote transport.
- No current requirement establishes cross-process or cross-machine agent messaging beyond existing remote execution work.
