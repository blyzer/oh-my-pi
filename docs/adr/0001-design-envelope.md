# 0001. Four operating modes are the architecture tests

Status: accepted
Date: 2026-09-02
Area: foundations

## Context

A harness that only ever runs as one local interactive TUI drifts into a specific shape: the
controller lives inside the TUI, state lives in closures, extensions execute in the engine
process, and a human is assumed to be present to recover from an unbounded call. omp v1 and pi both
took that shape. Every later feature (remote clients, subagents, autonomous jobs, spectators) then
had to be bolted on against those assumptions.

Four products were chosen as the envelope every subsystem must survive. They are not personas; they
vary the dimensions that separate a harness from a chat loop:

| Test | Local or remote | Interactive or autonomous | Trust boundary | Concurrency |
| --- | --- | --- | --- | --- |
| Multiplexed workspace | local | interactive | mostly trusted | many agents, one workspace |
| Remote driver | remote | interactive | split host/client | one or many agents |
| Spectator | remote view | observational | untrusted presentation input | many viewers |
| Factorio (software factory) | remote or fleet | autonomous | hostile repository and tool input | many jobs |

## Decision

Every subsystem design MUST be checked against all four modes before it is accepted. A design that
only satisfies the first is rejected.

Five consequences follow and bind the rest of the records:

1. **One authoritative session.** Rewind, fork, resume, replication, and inspection MUST derive
   from the same journaled state (0003, 0004).
2. **A trusted control plane.** Policy and session ownership stay on the host; sandboxes receive
   only bounded execution requests (0006).
3. **Bounded work.** Tool calls, subagents, and background jobs are cancellable streams with
   central limits and observability (0009, 0010, 0011).
4. **Explicit compatibility.** Model and provider quirks are structured knowledge, not branches
   scattered through call sites (0017).
5. **Views are projections.** The TUI, web client, remote client, and subagent inspector render
   the same state; none becomes an additional authority (0005).

## Consequences

- Any feature proposal MUST state how it behaves under the Spectator (untrusted presentation input)
  and Factorio (hostile tool input, no human) rows. "Works locally" is not an acceptance criterion.
- Later subsystems — the session DOM, convars, Directors, the sandbox stub, the component renderer —
  are each justified by one of the five consequences, not introduced for their own sake.
- Cost accepted: local-only shortcuts (in-process extensions, unbounded tool output, controller
  state in the UI) are prohibited even when they would be faster to ship.

## Status in omp

**Status: Partially implemented.** The four-mode rule is a design gate with no code of its own; the architecture meets the multiplexed-workspace and spectator modes, the remote-driver and factory modes only in part. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Multiplexed workspace: journal-first composition in `crates/driver/src/headless/kernel.rs` (`compose_kernel`); joined-system proofs `crates/e2e/tests/p1_doc_race.rs` through `p10_lift_idempotence.rs`.
- Spectator: `crates/collab` plus `crates/driver/src/collab/{session,observer,admission,registry}.rs`; `crates/e2e/tests/p11_collab_spectator.rs` proves convergence and no viewer mutation through the in-process `omp_collab::test_relay`. The production relay is the hosted one; no relay server ships in the tree.
- Remote driver: only stdio `omp rpc` / `omp acp` exist. No `omp.session.v1` proto, no `omp attach`, and `omp_rpc::server_tls` has no caller outside `crates/rpc` (0039 phases R1-R4 open).
- Factory: no `crates/fleet`; `crates/driver/src/adw` has no journal references and `crates/adw/src/lib.rs` leaves durability to the caller, consistent with the 0039 inventory (transitions not journaled, no resume).
- The prior note cited `PLAN.md` for the final P7 rerun. `PLAN.md` is gitignored and absent from a clean clone, so that claim cannot be checked here.

## References

- The Harness Playbook, "The design envelope"
- `AGENTS.md` — Architecture, Locked Deviations from pi
- 0002 for the complexity-ownership rule that these constraints enforce
