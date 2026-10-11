# 0044. Proposed presentation, surface, and inter-agent boundaries

Status: proposed; requires owner review
Date: 2026-10-11
Area: interface/runtime

## Context

OMP² already has a journal-first session model, typed DOM and cards, a retained TUI renderer, and a Tern Surface Protocol adapter path. Workpool and peer traffic use typed `IrcTraffic` envelopes with producer/owner authentication and journal replay. This research found no evidence that the current design is a real IRC wire protocol, that Tern is an agent communication bus, or that a universal Surface IR or adaptive transport layer is currently required.

## Proposed direction

1. Preserve `omp-journal` as the durable event authority and `omp-session` as its materialized fold.
2. Preserve `omp-chat` semantic projections and typed `CardView`; extract shared presentation views only where measurements demonstrate duplicated work across surfaces.
3. Preserve `omp-tui` as the terminal owner and final ANSI materializer. TSP remains a presentation adapter with cell fallback, not a journal, tool-authority, or inter-agent transport.
4. Treat `IrcTraffic` as an internal typed message/event envelope. Do not claim RFC IRC compatibility or redesign its transport without a concrete process/machine boundary.
5. If a future boundary requires IPC or remote delivery, define one versioned semantic message contract and add narrow transport adapters. Preserve authentication, journal-before-ack, ordering, cancellation, backpressure, replay, and worker-result provenance.
6. Defer a new Surface IR, WebGPU backend, adaptive transport router, generic card schema, and ABI redesign until reproducible measurements or a concrete deployment requirement justify them.

## Alternatives considered

- No change beyond benchmark/conformance work: remains the default until evidence exists.
- New canonical Surface IR: deferred; existing projections and TSP hooks cover the demonstrated surfaces and a second tree risks copying and replay divergence.
- Tern-centered architecture: rejected; TSP is presentation-only and must not grant execution authority.
- One adaptive transport system: deferred; no current cross-process/machine agent requirement justifies its operational cost.
- WebGPU-first presentation: deferred; no measured workload or accessibility/text proof supports it.

## Required evidence before acceptance

- Current TSP phase status and fake-Tern/PTY conformance results.
- Deterministic replay/render benchmarks across transcript sizes and streamed/workpool updates.
- Mailbox pressure, cancellation, ordering, and journal-before-ack measurements.
- A concrete approved process/machine boundary if a new transport is proposed.
- Security review showing surface clients cannot gain tool or workpool authority by subscribing to presentation events.

## Implementation gate

This ADR does not authorize production refactoring. Implementation may begin only after owner review accepts the minimal direction and each proposed change has a benchmark or concrete compatibility requirement.
