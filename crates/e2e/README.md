# omp-e2e

`omp-e2e` is the non-publishable joined-system acceptance suite for the journal-first OMP spine. Tests use `omp-agent`, `omp-session`, `omp-journal`, `omp-driver::headless`, and the production environment and document authorities; no test constructs the removed legacy agent, transcript store, settings host, or chat actor.

## Harness contract

Scenario bodies live in `tests/`. `src/support` owns bounded waits, RAII process and daemon lifetimes, scratch roots, production document/environment connections, canonical scripted inference, and `.oms` session reopening. Scripts replace only nondeterministic provider output. The journal, DOM fold, dispatcher, document authority, environment authority, and terminal event path remain production implementations.

Every wait is bounded. Every process, task, socket, and temporary root has an RAII owner. Test builds are LLVM (`[profile.test]` in `.cargo/config.toml`), so a failing proof's panic runs those destructors. As defense in depth for panics that run none (an abort, or a Cranelift test build, which emits no landing pads), owned process groups are also leased in a registry that a panic hook kills from, scoped to the panicking thread (`src/support/owned_groups.rs`). P7 drives the Cargo-built application on a real PTY through the debug protocol. P8 records measurements and locks their schema and arithmetic, but timing values are deliberately non-gating.

## Proofs

- P1: concurrent document leases preserve pinned reads and rebase non-overlapping stale writes.
- P2: `CancelTree` scope semantics and kernel `Up::Interrupt` / `Up::Cancel` preserve journal consistency.
- P3: dispatcher timeout settlement and `<meta><jobs>` / `JobBoard` lifecycle use one detached-job primitive.
- P4: only the live tool schema is advertised and `tool.call@1` records its selected `rev`.
- P5: Frozen, Stable, Dynamic, and Volatile prompt-band hashes preserve the provider cache prefix.
- P6: a killed mid-turn writer loses only a torn tail; `Session::open` reproduces the last committed DOM snapshot.
- P7: the production chat host handles input, streamed cards, resize, replay, and clean terminal restoration.
  Its plugin-approval case launches chat over installed plugins whose commands nobody approved and proves in-chat approval on the PTY: the blocked-launch notice, the `/plugins approve` selector (rows, command tails, `--plugin-dir` footer, arrow + Enter), persistence read back through `omp ext trust --show` while chat runs, a resize that keeps the box intact and the cursor on the selected command, the direct `/plugins approve <plugin> all` form, and clean quit.
- P8: retained-frame and journal-first kernel throughput recorder.
- P9: isolated environment worktrees and extension Director/Component registration.
- P10: historical tool lifts are idempotent and the lifted live revision executes through `Dispatcher`.
- P11: a spectator converges with a headless collaboration host through a real socket relay (`omp_collab::test_relay`). A viewer and an editor join a session with history, survive a partition, and receive the patches of a guest-driven turn; the host refuses a viewer's mutation (client API, no token, forged token); the relay only carries ciphertext; the room is listed in the local host registry while it lives; departures and room close are announced.
- P12: the production `omp envd` child (attached to a real docserver, with its home and user roots under scratch so the shipped `workspace-write` sandbox applies) relays a sandbox amendment prompt only to the connection that issued the command, on the issuing request. Another connection's forged answer decides nothing; the owner's denial denies; its approval reruns the command once, the `$$` it writes is the daemon's pid, and the daemon never withdraws the prompt that owner answered. Cancelling withdraws the prompt before the exit; closing the issuing connection ends its command without writing and frees the session at once. That close also cancels the command, so the relay failing a prompt closed on disconnect is proven by the `closing_a_connection_disconnects_its_relay` unit test in `crates/envd/src/server.rs`, not by P12. It needs Seatbelt, so CI runs it on macOS: it skips off macOS, and on macOS a failed Seatbelt probe fails it.
- `tool_sources`: production environment source routing and shared document snapshots.

`just e2e-build` compiles the suite. `just e2e` runs P1–P7, P9, P10, P11, P12, and `tool_sources`, then runs the non-gating P8 recorder test. Individual groups are available as `e2e-core`, `e2e-p7`, `e2e-p8`, `e2e-p9`, `e2e-p10`, `e2e-p11`, and `e2e-p12`.
