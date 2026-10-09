# 0039. Remote control is a session-control projection; the factory is a leased fleet over the one job primitive

Status: proposed
Date: 2026-09-29
Area: runtime

## Context

0001 makes four operating modes the acceptance test for every subsystem: multiplexed workspace,
remote driver (the owner now calls it **remote control**), spectator, and Factorio (the autonomous
**factory**). An audit of omp2 found only the multiplexed workspace complete. This record plans the
two modes the owner asked to include: remote control and the factory. The spectator mode is in
scope only as far as both depend on it (a verified relay path, headless hosting). Windows is out
of scope: named-pipe endpoints, AppContainer placement and Windows workers are not designed here.

### Inventory: remote control (audit of `origin/omp2`)

| Surface | State | Evidence |
| --- | --- | --- |
| `omp rpc` / `omp rpc-ui` | **existing, stdio only** | `crates/app/src/rpc_mode.rs`: NDJSON actor over `Session::subscribe()`. Verbs include `prompt`, `steer`, `follow_up`, `abort_and_prompt`, `approve`, `interrupt`/`abort`, `pause`/`resume`, `cancel`, `extension_ui_response`, `get_state`, `get_messages_page`, `get_entries`/`get_tree`, `set_event_filter`, `new_session`/`switch_session`/`open_session`/`branch`, `bash`, `login` (`crates/rpc/README.md` documents the history and event-filter verbs). It is pi-compatible JSON; private DOM snapshots never cross it (0005). Remote use needs an external pipe. |
| `omp acp` | **existing, stdio only** | `crates/app/src/acp_mode.rs`; editor I/O is planned by 0037. |
| Collaboration | **partial** | `omp-collab` (`crates/collab/*.rs`): revision-3 frames, AES-256-GCM rooms, read-only vs write-token tiers (`HostAdmission`, `host.rs:79–152`), host UI dispatch, a bounded reconnect queue (256 frames, 64/32 KiB watermarks, `relay.rs:28–37`), and payload shrinking to ≤ 1 MiB. `crates/driver/src/collab/{session,observer}.rs` own the relay session and project child agents as DOM snapshot plus events. It is wired only into the chat TUI (`crates/app/src/chat_cmd.rs:1247`, `chat_control.rs:637`). The relay is the hosted `wss://my.omp.sh` (`link.rs:15`); there is no relay server in the tree, and no test joins host → relay → guest. |
| Remote mutation admission | **partial, app-owned** | `chat_control.rs:1165–1209` applies guest prompts, aborts and agent commands, and journals only a display-name `author`. |
| `omp serve` / gateway daemon | **existing, inference only** | `crates/app/src/daemon.rs:315–421` registers `Gateway`, `ForwardProxy`, `Inference`, `Blob` and `Auth`. On a UDS it is owner-only. On TCP it uses a watched bearer-token file (`BearerAuth`, `:191–258`). There is **no TLS**: `omp_rpc::{server_tls, client_tls}` (`crates/rpc/src/tls.rs`) have no caller, and `auth-gateway serve` prints `http://{bind}` (`auth_gateway_cmd.rs:39`). |
| Interceptor-owned identity | **existing pattern** | `AuthenticatedRevealContext` (`crates/serve/src/auth.rs:93–173`) is inserted only by the server; wire fields never construct it. |
| envd | **existing, local only** | Owner UDS at mode 0600 (`crates/envd/src/server.rs:579–584`), speaking varint-framed `omp.env.v1`. `PartitionedEnvTransport` (`crates/env/src/partition.rs`) splits a client between the session-local host and the owner daemon; both are local. |
| `auth-broker` | **placeholder** | `serve` starts the same `DaemonHandle` on a local endpoint (`auth_broker_cmd.rs:63–68`). `token` writes `auth-broker.token`, but no listener reads it; `status` only checks that the file exists (`:89–104`, `:498–508`). `login --via` tunnels over native SSH (`omp_envd::ssh`). v1 had a real HTTP credential vault with snapshot/refresh-by-id (`origin/main:docs/auth-broker-gateway.md`). |
| Network session control | **missing** | There is no endpoint to list, attach, steer, interrupt or approve a live session over a network. |
| Session vocabulary in `omp.control.v1` | **dead** | `SessionSnapshotRequest`/`SessionDeltaRequest`/`SessionIngestRequest` (`control.proto:255–317`, `ControlRequest` fields 11–13) have no Rust or Python implementer. They ship raw journal bytes, which 0004 rule 2 prohibits for clients. |
| Principals | **partial** | `omp_core::Principal` (a person) and `RemotePrincipal` (a collab peer: peer id, name, tier, room, token digest; ≤ 16 B guard) live in `crates/core/src/principal.rs`. `ApprovalDecision.decided_by` exists (`crates/agent/src/approvals.rs:159–172`), but the stdio `approve` sets `decided_by: None` (`rpc_mode.rs:3179–3219`). |

### Inventory: factory

| Surface | State | Evidence |
| --- | --- | --- |
| `omp print` | **existing** | Single shot with `--max-time` (`crates/app/src/print_mode.rs`). |
| ADW | **partial** | `omp-adw` is a pure, replayable state machine: phases, attempts, input versions, and posture `WriteScope`/`NetworkScope`/`ApprovalScope` (`crates/adw/src/{run,profile}.rs`). `crates/driver/src/adw/{mod,production}.rs` runs agent and review phases as restricted child kernels and code phases as processes. Transitions are **not journaled**, and there is **no resume**; `omp adw run` exits 0/1/130 (`crates/app/src/adw_cmd.rs`). |
| Job board | **existing** | `crates/agent/src/jobs.rs`: a disposable index over `<meta><jobs>`. It rebuilds, adopts or orphan-settles after a restart, spills output to CAS, and has `JobKind::{Tool,Subagent,Process}` (0010). Proof P3. |
| Cancellation tree | **existing, process-local** | `crates/agent/src/cancel.rs`: session → turn → tool scope, with commit vs interrupt tokens; the 0011 ladder is TERM → grace → KILL. |
| Workpool | **existing, single process** | `crates/driver/src/subagent/workpool_scheduler.rs`: queue, batches, dead-worker requeue, fresh-worker policy, cancellation, and a replayable `workpool_state` snapshot. Workers start through the `WorkpoolLauncher` seam (`:201–208`). |
| Durable schedules | **partial, disarmed** | `crates/envd/src/schedules.rs`: an append-only SQLite journal, `(schedule_id, scheduled_at_ms)` idempotency keys, generation fences, and `BudgetReservation`/`budget_allows` (`schedule_plan.rs:43–60`). `ScheduleDeliveryBackend` (`:187–205`) has only a test implementer. `ProjectEnvironment::bind_schedule_delivery` has no production caller, so the clock never arms (`journal_runtime.rs`). |
| Workspace generations | **existing** | `crates/envd/src/workspace/operations.rs`: content-addressed generations, copy-on-write worktrees, and `merge_worktree` → manifest-diff artifact plus typed `WorkspaceConflict`. Proof P9. |
| CAS transfer | **existing, one host** | SHA-256 blob store with put-before-journal ordering (`crates/journal/src/blob.rs`); `omp.blob.v1.Blob` Stat/Get/Put/Delete; verified outcome replication with retries (`crates/driver/src/headless/kernel.rs:479–540`); envd verdict delivery leases (`crates/envd/src/blobs.rs:283+`). |
| Sandbox | **existing** | `omp-sandbox`: Seatbelt, Bubblewrap, Landlock+seccomp, gVisor, Docker(+runsc). `Plan::enforced` is authoritative. |
| Remote inference | **existing** | `--gateway` hands the kernel an inference channel instead of local credentials (`crates/driver/src/headless/kernel.rs:1996`). |
| Budgets | **partial, two units** | `omp.control.v1.Budget{tokens, cost_nanos_usd, wall_ms, turns, descendants}` vs schedule `cost_micros`. Neither is enforced across processes. |
| Crash resume | **existing** | P6: a killed writer loses only its torn tail. |
| Fleet | **missing** | There is no worker registry, lease, placement, cross-host cancellation, fleet budget, or multi-host observability. |

**Defects found during the audit.** These are recorded here because the plan touches them:

1. `crates/driver/src/collab/{remote_admission,host_bridge}.rs` are orphan files. `collab/mod.rs`
   declares only `observer` and `session`. `remote_admission.rs` imports
   `omp_agent::{remote_principal_interrupt, InterruptClass, MailboxSender}`, which do not exist.
2. `Dom::subscribe` hands every actor an **unbounded** `flume` channel (`crates/dom/src/txn.rs:476–481`).
   That is correct for in-process actors but unsafe for a network consumer.
3. The TCP gateway runs bearer authentication over plaintext.
4. External approvals are unattributed (`decided_by: None`).

`PLAN.md`, named by `AGENTS.md`, is local-only (gitignored), so this record cannot cite a
defect-ledger entry.

### v1 behaviour (`origin/main`, read-only)

- **Collab** (`docs/collab.md`). This is the spectator/guest path omp2 ported:
  - Hub topology; the host is authoritative. E2E-encrypted rooms on a content-blind relay. A full
    link (key + write token) vs a view-only link.
  - Guests may prompt, interrupt, drive subagents and answer dialogs. Everything that mutates the
    host machine (`/model`, `/resume`, `!` bash) is host-only.
  - A local host registry (`omp collab list/link`) over owner-only IPC endpoints, never TCP, with
    generation-bound link requests.
- **Auth broker/gateway** (`docs/auth-broker-gateway.md`). HTTP services:
  - The broker is the only writer of refresh tokens and serves redacted snapshots.
  - The gateway injects credentials. "Transport security … is delegated to the operator
    (Tailscale / Wireguard / reverse proxy + TLS)."
- **Factory** (`docs/factory-greenfield-audit.md`, `docs/swarm-factory-integration-audit.md`,
  `docs/adw/factory-*.md`). No fleet existed. The durable minimum it names, which must persist
  before each side effect:
  - workflow identity and definition digest;
  - phase state and attempt number;
  - child ids;
  - selected input versions;
  - gate reports;
  - integration status `prepared → applying → integrated | rejected`;
  - human decisions with a nonce.

  On replay, `running` becomes `pending` unless an idempotent result is proven, and `integrated`
  is never applied twice.

### Constraints this design must satisfy

- **0003 / 0004.** The journal is the only durable authority. Replication is subscription to the
  patch stream. Clients never parse the journal.
- **0005.** Every remote surface is an actor. None holds a mutable session handle.
- **0006.** The host owns session state, inference, policy, approval and journaling. Execution
  sits behind a bounded, obedient protocol. There are no duplex gateways from untrusted execution
  into host authority.
- **0010 / 0011.** One job primitive. Cancellation has a kill boundary.
- **Single owner** (`docs/py/00-overview.md` "Principal identity"): "One OS user per daemon", and
  "a second attached client observes, it does not arbitrate" (the session owner's approval policy
  applies).
- **`AGENTS.md`.** Typed prost wire structs. `flume` mailboxes. No per-message boxing. Errors
  through `thiserror`. Host changes go to `omp-envd`, client protocol to `omp-env`, and driver
  composes while app presents.

### Options considered

Remote control: transport and home

- **T0: expose envd (UDS → TCP).** This would put effect frames (`ArgsCommitted`, documents,
  exec) on the network. It bypasses the kernel, policy and journal, and violates 0006. **Prohibited.**
- **T1: carry the stdio RPC over SSH/`socat`.** Zero new code, but it is pi-shaped JSON, one
  client per process, and cannot detach, resume or authorize per verb. It remains an escape hatch
  only.
- **T2: extend the collab relay into the control plane.** The relay is content-blind and
  browser-compatible, and its grammar is frozen to revision 3 for `collab-web`. Its credential is
  a room link, not a device, and it has no per-verb scopes or resume cursors. It is kept for
  spectators and browser guests; it is not the control plane.
- **T3: a gRPC `SessionControl` service projected by `omp-serve`, backed by a driver-owned
  session host.** It reuses tonic/HTTP-2 flow control, `omp-rpc` Hello capability negotiation, the
  UDS/TCP listener assembly in `daemon.rs`, and the interceptor-identity pattern. **Chosen.**
- **T4: WebSocket JSON for browsers.** A second wire for one client class. Deferred (open question 1).

Remote control: wire projection

- **W1: DOM snapshot plus ordered events** (`omp_dom::{Snapshot, Event}`), the same contract as
  every local actor (0004, 0005). **Chosen.**
- **W2: collab transcript-v4 records.** Lossy and browser-shaped; it stays on the relay.
- **W3: pi RPC events.** A compatibility projection that stays on stdio.

Remote control: authentication

- **A1: a shared bearer file** (as the gateway does today). No device identity, no scopes, no
  revocation per device. Kept only for inference gateway clients.
- **A2: Ed25519 device keys, enrolled by a one-time pairing code, with a per-connection
  challenge** bound to the TLS session. Scoped and revocable, with no new crypto dependency (`ring`
  already verifies Ed25519 in `crates/ext/src/trust.rs`). **Chosen for direct network listeners.**
- **A3: mTLS client certificates.** `omp_rpc::TlsConfig.client_ca` supports it, but it needs a CA
  workflow. It is an allowed alternative authenticator, not the default.
- **A4: WebAuthn passkeys.** Only meaningful for a browser client, which does not exist yet.
  Deferred.
- **A5: an SSH-forwarded owner socket.** The SSH key is the authenticator and the principal is
  the owner. The cheapest first network path (Phase R3a).

Factory: where the item's kernel runs

- **K1: kernel on the coordinator, envd on the worker.** Every tool frame crosses the WAN, and
  envd's protocol would have to leave the machine. Rejected (see T0).
- **K2: kernel, envd and sandbox on the worker, with inference through the coordinator's
  gateway.** Credentials stay central, tool latency stays local, and the item's session journal is
  authoritative on the worker until it is shipped home as CAS. **Chosen.**
- **K3: ship the whole session to wherever capacity exists and migrate mid-turn.** Needs live
  journal migration. Deferred.

Factory: ledger

- **L1: the coordinator's own session.** Work items are job elements under `<meta><jobs>`, driven
  by `JobBoard`, with leases and results as props and CAS refs. **Chosen.**
- **L2: an SQLite append-only journal, as for schedules.** A second durable format for the same
  concept. Rejected unless L1's size bound fails (open question 7).
- **L3: an external database.** Rejected: lock-in, and a parallel source of truth.

## Decision

### Part A — Remote control

**A1. Roles and homes.**

- `omp-driver` gains a **`SessionHost`**. It is the one component that holds live kernels'
  mailboxes and session subscriptions for remote actors. It has two deployments:
  - **In-process**: every `omp chat/rpc/print` process owns one for its own sessions and exposes it
    only on an owner-only UDS under the data dir's `run/` directory (0700 directory, 0600 socket).
    Sessions are discoverable through a metadata-only local registry, as v1's collab host registry
    was.
  - **Daemon**: `omp serve --sessions` owns headless kernels (via `compose_kernel`) that outlive
    any client. This is what makes *detach* meaningful.
- `omp-serve` gains the gRPC projection `session::SessionControlRpc`. `omp-rpc` gains TLS listener
  wiring and the device-challenge primitives. `app/src/daemon.rs` remains the one tonic assembly
  point. `omp-collab` keeps the relay and browser path. The transport-neutral parts of its admission
  (principal, visibility classes, mutation classification) are reused by `SessionHost` instead of
  being re-implemented.
- envd is never reachable from the network. `omp.env.v1`, `omp.document.v1`, `omp.toolhost.v1` and
  extension CONTROL MUST NOT be bound to TCP or tunnelled.

**A2. Transport and versioning.**

- A new package `omp.session.v1` in `crates/proto/proto/omp/session/v1/session.proto`, served over
  gRPC.
- Negotiation uses the existing `omp.gateway.v1.Gateway.Hello` with capability
  `session.control.v1`; an unknown major version is refused.
- Listeners:
  - an owner UDS (no further authentication; the filesystem is the authenticator);
  - loopback TCP (device authentication required);
  - non-loopback TCP (**TLS mandatory** plus device authentication).
- The same TLS rule applies retroactively to the existing TCP gateway: non-loopback plaintext
  listeners become a startup error.

```proto
service SessionControl {
  rpc ListSessions(ListSessionsRequest) returns (SessionList);
  rpc Attach(AttachRequest) returns (stream AttachFrame);   // welcome, snapshot chunks, events, resync, heartbeat
  rpc Submit(ControlCommand) returns (CommandReceipt);      // prompt | steer | queue | interrupt | abort_tools | pause | cancel
  rpc Approve(ApproveRequest) returns (CommandReceipt);     // ticket_id, approved, scope, reason
  rpc AnswerUi(UiAnswer) returns (CommandReceipt);          // correlated host select/editor request
  rpc Open(OpenSessionRequest) returns (SessionRef);        // daemon host only: new | resume | fork
}
// Every mutating message carries: session_id, idempotency_key (client ULID), expected_epoch (optional).
// AttachFrame.oneof { Welcome{header, epoch, head_seq, granted_scopes},
//   SnapshotChunk{epoch, seq, index, bytes, final}, Event{epoch, seq, bytes},
//   Resync{reason}, Heartbeat{head_seq} }
```

- **DOM payloads.** They travel as canonical `omp_dom` serde bytes inside typed envelopes; the
  receiver decodes them with `Snapshot::from_bytes` and the typed `Event`/`Patch`. There is no
  `Value` traversal. A full protobuf `Op` vocabulary is open question 5.
- **Dead vocabulary.** The unused `omp.control.v1` session snapshot/delta/ingest messages are
  deleted and their field numbers reserved, in the same change that lands `omp.session.v1`.

**A3. Authentication.**

- Identity is established by the transport:
  - **owner UDS** → owner principal, full scopes;
  - **SSH-forwarded owner UDS** → owner principal (the SSH key authenticated it);
  - **TCP** → an enrolled device.
- **Enrolment** is local-only (`admin` scope):
  1. `omp control pair --scopes … [--project <root> | --session <id>] [--expires 30d]` prints a
     one-time 128-bit pairing code (5-minute TTL) and the listener's TLS certificate fingerprint.
  2. `omp control enroll <host> <code>` on the device generates an Ed25519 key (`ring`) and sends
     the public key with an HMAC of the code over TLS.
  3. The host appends a grant to `<data>/control/devices.toml` through the atomic owner-only writer
     already used by `omp-ext` grants.
- **Per connection:** a server nonce → the device signs `nonce ‖ TLS exporter ‖ host fingerprint`
  → the server mints an in-memory control token (default TTL 15 minutes, renewed by re-signing)
  bound to (device key id, scopes, session set).
- An interceptor verifies the token (`omp_core::ct_eq`) and the revocation list on **every** call,
  then inserts a `ControlContext{principal, scopes, sessions}` into request extensions. Handlers read
  identity only from there; request fields never construct it.
- `omp control devices|revoke` manage grants. Revocation closes live streams within one heartbeat.
- mTLS (A3 in the options) MAY replace the device challenge when configured. The bearer-file (A1)
  remains for inference-gateway clients only.

**A4. Authorization.** `ControlScope` is a strum-derived closed enum:

| Scope | Allows |
| --- | --- |
| `observe` | `ListSessions`, `Attach` with host-local props redacted |
| `observe_host_local` | unredacted snapshots/events (cwd, env facts, advisor subtrees, raw provider metadata) |
| `prompt` | `Submit{prompt, steer, queue}` |
| `interrupt` | `Submit{interrupt, abort_tools, pause}` |
| `approve` | `Approve` with scope ≤ `session` |
| `approve_persist` | `Approve` with scope `persist` (default: never granted to a device) |
| `control` | `Submit{cancel}`, `Open`, model/thinking switches |
| `worker` | fleet worker protocol only (Part B); no session verbs |
| `admin` | enrol/revoke/pair; **local UDS only**, never grantable |

- Grants are ceilings. A session set (project root or ids) further restricts every scope.
- Redaction reuses collab's `VisibilityClass`. `HostLocal` props are blanked **in place**, so
  handles and indexes stay aligned (the collab `JournalRecord` omission-marker precedent). Nodes are
  never pruned.
- Every principal is a delegate of the host's single OS user. Multi-user daemons stay refused.

**A5. Session verbs.** Every verb maps onto an existing mailbox path. No verb reaches envd.

| Verb | Kernel path | Notes |
| --- | --- | --- |
| prompt (idle) | `TurnRequest::Authored` / `Session::user_authored` | same path as the collab prompt |
| steer | `Up::Steer{…, author}` | `SteerAuthored` is folded into `Steer{author: Option<…>}` (clean cutover) |
| queue (follow-up) | `Up::Queue{…, author}` | gains the same author field |
| interrupt | `Up::Interrupt` | |
| abort tools | `Up::AbortTools` | scoped; siblings continue (0011) |
| pause / resume | `Up::Pause` / `set_paused` | journal-derived gate |
| cancel | `Up::Cancel` | `control` scope |
| approve | `Up::Approve{ApprovalSource::External, decided_by = principal id}` | tickets are idempotent; first answer wins |
| answer UI | correlated `HostUiDispatcher`-style table | first writable answer wins; others get `ui_request_end` |
| open / resume / fork | daemon `SessionHost` | `control` scope |

- **Idempotency.** Every mutation carries a client `idempotency_key`. The key is journaled on the
  entry it creates (a typed field in `MsgUser` and the steer/queue records). On retry, the host looks
  it up in the DOM, so exactly-once-as-observed survives a host restart without a side table.
- **Mailbox pressure.** A full mailbox returns `RESOURCE_EXHAUSTED`. The host never waits.
- **Multiple controllers.** Inputs serialize through the one mailbox. Approval policy is the owner's.
  Concurrent answers resolve first-wins, exactly as for collab guests (open question 3).
- There is **no remote shell verb**. The stdio `bash`, `login` and `export_html` stay local-only.
  Remote principals change the machine only through the agent's admitted tools.

**A6. Event streaming, resume cursors, backpressure.**

1. **Bridge.** Per attached session, a host task drains the kernel's in-process DOM subscription
   (still unbounded, still local) into a **bounded replay ring**. The ring is bounded by
   `sv_control_replay_events` (proposed default 4096) and `sv_control_replay_bytes` (16 MiB). The
   task never blocks the kernel actor.
2. **Cursor.** `(session_id, epoch, seq)`:
   - `seq` is a host-assigned monotonic number per bridged event. `Event::cause` is not unique;
     for example, `session_switch` patches reuse the head, so `cause` cannot serve as the cursor.
   - `epoch` is a random id minted when a bridge starts (host start or session reopen).
   - DOM `Reset` events (branch navigation) are ordinary ring entries.
3. **Resume.** `Attach` with a cursor in the current epoch and ring replays `(seq, head]`. Any other
   cursor gets `Resync` followed by fresh snapshot chunks at the current `seq`. The authority is
   always the journal: after a host restart the snapshot comes from `Session::open`'s fold (P6).
4. **Backpressure.** Each subscriber has a bounded outbound queue (`sv_control_subscriber_bytes`,
   proposed 4 MiB) on top of HTTP/2 flow control.
   - While queued, consecutive `Stream Append` events for one `sid` are merged. This is lossless,
     because appends concatenate.
   - Beyond the bound the subscriber is marked lagging: its queue is dropped, then it receives
     `Resync` + snapshot.
   - A slow client can never stall the kernel or grow host memory without bound, and it never sees
     a silent gap.
5. **Heartbeats** carry `head_seq` (convar `sv_control_heartbeat_ms`). Snapshots are chunked
   (≤ 1 MiB per frame) and bounded by `SNAPSHOT_HIGH_WATER_LIMIT`. Tonic decode limits bound every
   inbound message.

**A7. Composition with envd, approvals and ACP.**

- Remote verbs enter through the kernel mailbox. Effects still cross envd admission at the
  `ArgsCommitted` boundary with the session's `ApprovalMode` and per-tool policy. A remote principal
  can never widen the policy (0006).
- Approval tickets are journaled `<prompt>` elements. A remote `Approve` is one more answer source.
  `require_human` tickets accept only principals holding `approve`. `persist` scope needs
  `approve_persist`, and by default it is decidable only locally.
- ACP editor I/O (0037) belongs to the controlling ACP connection. Session control never serves
  `fs/*` and never binds an editor backend.

**A8. Local-only, permanently:**

- envd, docserver, extension-host CONTROL, worker DATA and eval sockets;
- device enrolment and revocation;
- credential reveal (`AuthenticatedRevealContext`);
- `persist` approvals (unless granted);
- stdio `bash`/`login`/`export_html`;
- cfg archive writes;
- ACP `fs/*`.

**A9. Audit.**

- **Session-affecting actions** are attributed in the session journal. Author display name,
  principal key id or room token digest, idempotency key, and `decided_by` are recorded in the same
  entry as the action. `RemotePrincipal` gains an origin (`Room{room_id, token_digest}` or
  `Device{key_id}`) as a clean cutover. The ≤ 16 B size guard stays.
- **Connection-level events** (attach, auth failure, enrol, revoke, lagging resync) append to a
  host-level audit journal `<data>/control/audit.oms`, using the same crash-tolerant `omp-journal`
  primitive, and are traced through `omp-observability`.

### Part B — Factory

**B1. Roles.**

- A **coordinator** is a `SessionHost` (daemon deployment) that owns a long-lived **fleet session**
  plus the canonical project environment (its envd).
- **Workers** run `omp serve --worker <coordinator>`. Each worker runs its own `SessionHost`, envd
  and sandbox.
- **Single host is the degenerate fleet.** Coordinator and worker share one process. The worker is
  the existing `KernelWorkpoolLauncher` path with local copy-on-write worktrees. The ledger, leases
  and cancellation are the same code and data with an in-process transport. Every factory feature
  MUST work at this size first (Phase F1/F2).

**B2. Ledger = the fleet session.**

- **Work items.** Each item is a job element in the fleet session's `<meta><jobs>`: a new
  `JobKind::Work`, or `Subagent` with a placement attribute (open question 8).
- **Item props:**
  - spec digest;
  - ADW run/phase/attempt;
  - base workspace-generation digest;
  - budget;
  - lease `{worker_id, lease_gen, deadline}`;
  - status;
  - result CAS refs;
  - integration status `prepared | applying | integrated | rejected`.
- **Durability.** Every transition commits before the side effect it licenses. `JobBoard::rebuild`
  after a coordinator restart adopts or orphan-settles exactly as it does for local jobs (0010).
- **In-memory state.** Schedulers and lease tables are disposable indices derived from the DOM
  (the workpool precedent).
- **ADW runs** journal their domain transitions into the same session. `omp adw run` gains
  `omp adw resume <run>`, which replays the pure `omp-adw` domain from those records.
- **Scheduled runs.** A production `ScheduleDeliveryBackend` submits runs, arming the dormant envd
  scheduler. Its `idempotency_key` becomes the run's submission key.

**B3. Leases and fencing.**

- **Pull model.** Workers pull: `Ready{free_slots}` → `LeaseGrant{item, attempt, lease_gen, ttl,
  spec, base_generation, budget, gateway_token, trace_context}`.
- **Clocks.** Deadlines are coordinator-monotonic TTLs. Workers renew by heartbeat and never
  compare wall clocks.
- **Fencing.**
  - At most one live lease per item, enforced by the pure domain.
  - A result, renewal or artifact claim with a stale `lease_gen` is refused.
  - Idempotency key = `(item, attempt)`. A result for an attempt already settled is acknowledged
    and discarded.
- **Expiry.** An expired lease requeues the item at the **same** attempt with a new `lease_gen`,
  unless a result under that key is already in CAS and verified.

**B4. Worker registration.**

- Workers dial out: `Fleet.Join` is a bidirectional stream opened by the worker. The coordinator
  needs no inbound path to workers.
- They authenticate with an enrolled device key that holds only the `worker` scope (A3/A4).
- **Hello facts:**
  - build id and capabilities;
  - OS/arch;
  - the enforced sandbox capabilities (from `Plan::enforced`, never self-described intentions);
  - slots, memory and disk watermarks;
  - locally cached workspace generations and repository identities.
- **Version skew** is refused at Hello (open question 9).

**B5. Placement.** One worktree has exactly one owning envd at a time.

1. **Admissibility.** The worker's enforced sandbox satisfies the phase posture (`omp-adw`
   `Requirement` vs resolved `Posture`), with OS match and free slots. An item no worker can admit
   is `Blocked` with a typed reason, never placed optimistically.
2. **Score.** Affinity for workers that already hold the base generation's manifest, then the
   project concurrency quota, then load.
3. **Ownership.** The lease records `placed_on`. That worker's envd materializes a copy-on-write
   worktree from the generation (fetching missing blobs by digest) and owns it until settle or
   expiry. The coordinator's envd alone owns the canonical project root.

**B6. Execution on the worker (K2).**

- The item runs as an ordinary session on the worker `SessionHost`. It is seeded from the spec
  through cfg (0013), with policy frozen from the spec and inference through the coordinator's
  gateway using the lease's `gateway_token` (`--gateway` path).
- No provider credentials or refresh tokens reach workers.
- Hostile repository input is contained by the worker's sandbox (0006). Approvals needing a human
  follow the ticket's `unreachable`/timeout policy or escalate to operators through session control
  on the coordinator (A5). An autonomous item never waits unbounded.
- On settle, the worker uploads to the coordinator's CAS:
  - the item's `.oms` journal;
  - its blobs;
  - the manifest-diff artifact;
  - gate reports.

  The job element references them by digest, so the coordinator can render, replay or rewind the
  child without the worker.

**B7. Artifacts and CAS.**

- Everything crosses hosts as SHA-256-addressed blobs (`artifact://sha256/<hex>`) over
  `omp.blob.v1.Blob`: `Stat` before `Put`, streamed chunks, and `BlobStore::verify` on receipt.
- Coordinator retention is journal-rooted (existing GC). Worker copies are caches, released after
  the coordinator acknowledges the result, using the envd verdict delivery-lease pattern.
- Output bounding stays central (0009).

**B8. Budgets and quotas.**

- **Hierarchy.** fleet → principal → run → item → attempt, in `omp.control.v1.Budget` units. The
  schedule `cost_micros` is converted at its boundary.
- **Reservation** is taken at lease grant (the `BudgetReservation`/`budget_allows` pattern). The
  actual receipt settles at completion.
- **Enforcement point** is the coordinator's gateway. Every worker inference call presents the
  lease's token, so the gateway meters it and refuses with a typed budget-exhausted verdict at the
  limit. Wall time is enforced on the worker (`max_time`), and turns/descendants by the kernel.
- **Quotas:** worker slots, per-project concurrency, and provider limits (existing account
  routing/blocks).

**B9. Cancellation across hosts.**

- The coordinator keeps a fleet `CancelTree`: run → item.
- **Cancelling an item** sends `CancelLease` on the worker's Join stream. The worker maps it to
  `Up::Cancel` on the item's kernel, and the local 0011 ladder applies (TERM → grace → KILL,
  `EffectsUnknown` journaled).
- **Partition.** When the worker is unreachable, the kill boundary is enforced from both sides:
  - Coordinator side: the lease is fenced (results refused), and its gateway token is revoked
    immediately, so the partitioned worker can no longer spend.
  - Worker side: a worker that has not renewed within the TTL self-cancels its items (dead-man
    switch) and discards its worktrees after uploading nothing.

**B10. Failure handling.**

| Failure | Handling |
| --- | --- |
| Worker crash | Lease expiry → requeue at the same attempt; a verified CAS result under `(item, attempt)` is adopted instead |
| Coordinator crash | P6 fold + `JobBoard::rebuild`; workers reconnect and re-present live leases (renew) or results (settle); unknown leases are cancelled |
| Partition | B9 fencing + token revocation + self-cancel |
| Poison item | ADW attempt budget → `Halted`; the item is quarantined with its evidence |
| Integration conflict | Typed `WorkspaceConflict` → phase rejected with the conflicts as correction feedback (`OnReject::Correct`) |
| Upload failure | Retry with backoff under the lease; expiry → requeue |
| Budget exhausted | Gateway refusal → the item settles `budget_refused`; the run halts or continues per policy |

**B11. Observability.**

- The fleet session **is** the dashboard: operators attach to it via session control or spectate
  it via collab (0005: views are projections).
- Leases carry OTel trace context, so worker spans nest under the coordinator's run span.
- Attributes are `omp.fleet.*` through the observability `vocab!`/strum vocabulary.
- **Metrics:** queue depth, lease age, renewal latency, slots, budget burn and resync count.
- `omp fleet status` is a projection of the DOM, never a separate store.
- Attaching to a running item is proxied by the coordinator to the owning worker's `SessionHost`.
  Operators need one endpoint.

**B12. Integration.**

- Only the coordinator integrates. It applies accepted manifest-diffs to the canonical project
  through its envd (`merge_worktree` / document authority, P1 rebase semantics), **serially**.
- The status is journaled `prepared → applying → integrated | rejected`. `applying` found at
  restart triggers reconciliation against the recorded pre- and post-generation digests before any
  retry. `integrated` is never re-applied.
- Workers never write the canonical tree and never push. Git push or PR creation is a separate,
  explicitly gated phase kind (open question 10).

### Part C — Security rules (summary)

1. No network listener or tunnel carries envd, docserver, toolhost or extension CONTROL frames.
2. Non-loopback TCP requires TLS. Every TCP request carries a verified device token checked against
   revocation. Identity lives only in server-inserted request extensions.
3. Scopes are closed, typed and least-privilege. `admin` is local-only. A remote principal cannot
   widen approval policy.
4. Workers hold `worker` scope only, no provider credentials, and per-lease revocable gateway tokens.
5. Every admitted remote mutation and every connection event is attributed in a journal.
6. Inbound messages, snapshots, subscriber queues and replay rings are bounded, and overflow resyncs.

## Consequences

- **0001 rows:**
  - **Multiplexed workspace.** Unchanged. A local TUI can additionally attach to daemon sessions.
  - **Remote control.** Part A. A device with `prompt+interrupt+approve` drives a session with the
    same actor contract as the TUI, and detaches without stopping it.
  - **Spectator.** Untrusted viewers stay on collab read-only links: E2E-encrypted, content-blind
    relay, with viewer input never reaching policy. Owner devices can observe with redaction.
    Headless hosting makes factory runs spectatable.
  - **Factory.** Part B. Durable, leased, budgeted, cancellable across hosts, and correct on one
    host first.
- **Easy:**
  - dashboards and alternative clients are actors over one typed stream;
  - resume after network loss is a cursor, and after host loss a fold;
  - any machine with `omp` becomes a worker by pairing it.
- **Prohibited:**
  - envd on the network;
  - plaintext non-loopback listeners;
  - identity from wire fields;
  - client-side journal parsing (hence the deletion of the `omp.control.v1` session messages);
  - a durable fleet store outside journal + CAS;
  - workers writing the canonical tree or holding refresh tokens;
  - a remote shell verb;
  - optimistic placement that cannot enforce the posture.
- **Costs accepted:**
  - a bridge task and replay ring per attached session;
  - one extra hop for proxied item attach;
  - uploading item journals to the coordinator;
  - key management (pairing, revocation);
  - the coordinator is a single point of scheduling. Its failure pauses dispatch but loses nothing
    (fold + rebuild).

## Status in omp

**Status: Partially implemented (phase S0 done in PR #129; no R1-R4 or F1-F4 deliverable found).** Verified
2026-10-04 against `omp2` at `083b38fe7d`. What exists, is partial, or is missing is listed in the two
inventories in Context, which describe the tree before S0. New components still absent:
`omp.session.v1`, `omp.fleet.v1`, driver `SessionHost` and `fleet`, `omp-serve` `session`/`fleet`
projections, `omp-rpc` TLS + device challenge, and the proposed pure crate `omp-fleet`.

Phase S0 (spectator proof and headless hosting) landed in commit `38c4e04ff3`: guest-mutation
admission moved into `crates/driver/src/collab/admission.rs`; `host.rs` (`HeadlessRoom`) and
`registry.rs` were added; the orphan `remote_admission.rs` and `host_bridge.rs` were deleted (Context
defect 1 is fixed); `crates/app/src/chat_control.rs` was changed in the same commit to use them. Proof P11-a is
`crates/e2e/tests/p11_collab_spectator.rs` over the in-process `omp_collab::test_relay`
(`crates/collab/test_relay.rs`), gated by `just e2e-p11` and `.github/workflows/ci.yml`.

Checked as still open: no `omp.session.v1` or `omp.fleet.v1` proto, no `crates/fleet`, no
`omp attach`, and `omp_rpc::server_tls` has no caller outside `crates/rpc`.

### Implementation plan (PR-sized, value first)

The order front-loads verified value: first spectating, then read-only monitoring, steering,
network reach and detach, and only then the fleet. F1 is independent of the R phases and can run in
parallel. F2 needs R1. F3 needs R3b.

- **S0 — Spectator proof and headless hosting**
  - Crates: `crates/driver`, `crates/app`, `crates/e2e`.
  - Move guest-mutation admission from `chat_control.rs` into `driver::collab` against real APIs,
    and delete the orphan `remote_admission.rs`/`host_bridge.rs` (clean cutover). Headless
    compositions (`print`, `rpc`, the future daemon) can then host a room.
  - Add an in-test content-blind relay (`tokio-tungstenite` server side) and proof P11-a.
- **R1 — Read-only attach over the owner UDS**
  - Crates: `crates/proto`, `crates/driver`, `crates/serve`, `crates/app`.
  - `omp.session.v1` (`ListSessions`, `Attach`); in-process `SessionHost`; bridge, ring, cursor and
    resync; redaction; the local registry.
  - `omp attach --observe <session>` renders with the existing chat actor from snapshot + events.
  - Delete the `omp.control.v1` session snapshot/delta/ingest messages.
- **R2 — Steering and approvals over the UDS**
  - Crates: `crates/agent`, `crates/session`, `crates/journal`, `crates/driver`, `crates/serve`,
    `crates/core`, `crates/app`.
  - `Submit`/`Approve`/`AnswerUi`. `Steer`/`Queue` gain `author` (clean cutover of `SteerAuthored`).
  - The idempotency key is journaled; `decided_by` is always set, including by stdio `approve`;
    `RemotePrincipal` gains an origin; audit journal.
- **R3a — SSH-forwarded attach**
  - Crates: `crates/envd` (ssh), `crates/app`.
  - `omp attach --via <ssh-alias>` over native SSH to the remote owner UDS, or to a remote
    `omp control bridge` over an exec channel if stream-local forwarding is unavailable. There is no
    new authenticator.
- **R3b — Direct network listener**
  - Crates: `crates/rpc`, `crates/serve`, `crates/driver`, `crates/app`.
  - Wire `server_tls`; pairing/enrolment/challenge/tokens/revocation and the `ControlScope`
    ceilings.
  - `omp serve --listen <addr> --tls-cert/--tls-key`. Non-loopback plaintext becomes a startup
    error, for the gateway too.
- **R4 — Detach and daemon sessions**
  - Crates: `crates/driver`, `crates/app`.
  - `omp serve --sessions` hosts headless kernels; `Open`; `omp chat --attach <endpoint>`; idle and
    retirement policy; the scheduled-delivery backend binds here for session-scoped schedules.
- **F1 — Durable single-host factory**
  - Crates: `crates/driver` (adw), `crates/agent`, `crates/envd`, `crates/app`.
  - ADW transitions journaled; `omp adw resume`; work items as jobs; budget reservation/settle;
    production `ScheduleDeliveryBackend` → runs; serial integration journal.
- **F2 — Fleet domain and loopback worker**
  - Crates: new `crates/fleet` (`omp-fleet`, a pure state machine for leases, fencing, placement
    scoring and budgets), `crates/proto` (`omp.fleet.v1`), `crates/driver`, `crates/serve`.
  - Coordinator over the fleet session; `RemoteWorkerLauncher` implementing `WorkpoolLauncher`; an
    in-process worker over the real protocol.
  - The single host runs the network code path.
- **F3 — Remote workers**
  - Crates: `crates/driver`, `crates/envd`, `crates/serve`, `crates/app`.
  - `omp serve --worker`; worker keys; placement by posture and affinity; CAS fetch/upload;
    per-lease gateway tokens and metering; `CancelLease`; fencing; self-cancel; result adoption.
- **F4 — Operator surfaces and proofs**
  - Crates: `crates/driver`, `crates/observability`, `crates/app`, `crates/e2e`, CI.
  - `omp fleet submit|status|cancel`; proxied item attach; OTel propagation and `omp.fleet.*`
    metrics; P12 two-worker proof on the Linux CI job.
- **Bookkeeping.** New `just` recipes `e2e-p11`/`e2e-p12`, added to `just e2e` and
  `.github/workflows/ci.yml`. One `area/*` entry in `.github/labeler.yml` for `crates/fleet`.
  READMEs for every touched crate.

### Test plan (owning seams)

- **`crates/core`**
  - `RemotePrincipal` origin round-trip and the ≤ 16 B guard.
  - `ControlScope` strum parse/emit and the rejection of unknown scopes.
- **`crates/rpc`**
  - The TLS config rejects a missing key.
  - Challenge verification fails for a wrong nonce, the wrong TLS exporter, a revoked key or an
    expired token.
  - The token compare is constant-time (`ct_eq`).
  - A proptest over token/grant parsing.
- **`crates/serve`**
  - `SessionControlRpc` maps typed driver faults to stable status codes.
  - Identity comes only from extensions: a forged principal field is ignored.
  - Oversize messages are refused before decode.
- **`crates/driver`**
  - Bridge table tests: resume inside the ring → exact tail; an old epoch or an evicted `seq` →
    `Resync` + snapshot; `Reset` is carried in order.
  - Append coalescing is byte-identical to the unmerged stream (proptest).
  - A lagging subscriber is resynced while the kernel is never blocked (a slow consumer beside a
    streaming kernel).
  - Redaction keeps handles aligned.
  - Idempotent `Submit` after a host restart creates one entry.
  - Mailbox full → `RESOURCE_EXHAUSTED`.
  - Remote `Approve` sets `decided_by`; `persist` is refused without `approve_persist`.
  - Collab admission moved from app keeps its read-only rejections.
  - Coordinator: lease expiry requeues at the same attempt; stale `lease_gen` is refused; `rebuild`
    adopts a verified result; integration `applying` reconciles on restart.
- **`crates/fleet`** (pure; proptest):
  - never two live leases per item;
  - reservations never exceed any budget level;
  - identical transition logs replay to identical state;
  - placement never selects a worker whose enforced posture is looser than required.
- **`crates/envd`**
  - Worktree materialization from a generation whose blobs arrive by digest (a corrupt blob is
    refused by `verify`).
  - The production schedule delivery backend deduplicates by idempotency key across a restart
    (extend `tests/durable_schedules.rs`).
- **`crates/app`**
  - CLI parsing for `attach`/`control`/`serve --listen|--sessions|--worker`/`fleet`.
  - The non-loopback plaintext listener is refused.
  - `omp attach --observe` on a real PTY (P7 harness pattern).

### E2E proofs to add (`crates/e2e`)

- **P11 — remote view and control** (macOS and the Linux CI job):
  - (a) a headless host, an in-test relay and a read-only collab guest: snapshot + live events;
    mutations rejected; reconnect resyncs;
  - (b) `Attach` over the UDS, SIGKILL of the client, re-attach with the cursor → exact tail;
    SIGKILL of the host → new epoch → snapshot equal to the `Session::open` fold;
  - (c) remote prompt/steer/approve/interrupt through the real kernel, journaled with principal and
    idempotency key, with a retried `Submit` deduplicated;
  - (d) a stalled subscriber beside a fast streaming turn: the kernel's turn completes on time, and
    the subscriber gets `Resync`;
  - (e) the TCP listener with TLS: no token / revoked device / out-of-scope verb are refused, and
    no envd endpoint listens on any TCP port.
- **P12 — factory leases** (Linux CI job):
  - (a) a single-host ADW run SIGKILLed during `applying` resumes and integrates exactly once;
  - (b) a coordinator plus two `omp serve --worker` processes over loopback TLS: SIGKILL of one
    worker → lease expiry → the item completes on the other at the same attempt;
  - (c) a simulated partition: the stale worker's result is fenced, its gateway token is refused,
    and it self-cancels;
  - (d) `omp fleet cancel` reaches the worker's process tree (TERM → KILL) with `EffectsUnknown`
    journaled;
  - (e) budget exhaustion is refused at the gateway, and the item settles `budget_refused`.

### Risks

- **Fleet ledger size.** Thousands of job elements in one DOM may strain snapshot size and
  `SNAPSHOT_HIGH_WATER_LIMIT` (1 048 576 handles). Mitigation: bounded item props, results in CAS,
  a per-run child session if needed (open question 7).
- **Many subscribers.** Each attach costs a bridge and a queue. Cap subscribers per session with
  `sv_control_max_subscribers`.
- **Key UX.** Pairing, TLS certificates (no self-signed generator in the tree; `rcgen` would be a
  new dependency that must defend its seat), and revocation latency.
- **Sandbox heterogeneity.** Seatbelt vs Bubblewrap posture strength across workers can leave items
  `Blocked` rather than placed. This is honest, but it is surprising to operators.
- **Workspace shipping cost** for large repositories. Affinity helps; cold workers pay a full
  generation fetch.
- **Coordinator as a scheduling single point.** It is durable but not highly available (K3/HA is
  out of scope).
- **Two budget units** (`cost_micros` vs `cost_nanos_usd`) risk conversion bugs until they are
  unified.
- **Hosted relay dependency** for spectators. Production `my.omp.sh` is not self-hostable (v1
  docs), so CI cannot exercise it; P11-a uses the in-test relay.

### Not verified

- Whether every DOM `Event` maps 1:1 to a journal entry. The design uses an opaque host `seq` for
  that reason.
- Whether `omp_envd::ssh` (russh) supports `direct-streamlocal` forwarding to a remote UDS. R3a
  falls back to an exec-channel bridge.
- Tonic TLS through `tls-ring` with operator PEMs end to end (the builders exist; nothing calls
  them).
- Whether `WorkpoolLauncher`'s contract (the child registered in a shared `SessionAuthority` before
  `spawn` returns) can be met across hosts without a proxy authority.
- Collab hosting outside the chat TUI (only chat wiring was observed).
- The production relay's behaviour.
- Any macOS runtime behaviour (nothing was built or run for this record, by instruction).
- Performance: no measurements were taken; all bounds above are proposed defaults.

### Open questions for the owner

1. gRPC-only session control, with browsers staying on the collab relay (proposed)? Or also a
   WebSocket/JSON or gRPC-web surface?
2. Device authentication: Ed25519 pairing (proposed), mTLS by default, or WebAuthn passkeys once a
   browser client exists? May an E2E-encrypted relay carry session control for NAT traversal?
3. Multiple controllers: mailbox-serialized inputs and first-wins approvals (proposed), or an
   explicit single "take control" lease per session?
4. Keep `persist`-scope approvals local-only unless `approve_persist` is granted (proposed)?
5. Wire DOM payloads: canonical `omp_dom` serde bytes in typed envelopes (proposed), or a full
   protobuf `Op`/`Value` vocabulary now?
6. Kernel placement K2 (worker runs the kernel, inference through the coordinator's gateway):
   confirm. Or should workers use a completed auth-broker (v1-style remote credential store)?
   Should the broker be finished before F3 at all?
7. Fleet ledger as the coordinator's session DOM (proposed), or an SQLite journal like schedules
   if the size bound fails?
8. A new pure crate `omp-fleet` (proposed, mirroring `omp-adw`), or a module inside `omp-adw`? A
   new `JobKind::Work`, or `Subagent` with placement?
9. Worker version skew: require an identical build id, or a compatible `fleet.v1` capability range?
10. Integration: coordinator-only merge into its canonical tree (proposed). Are git push and PR
    creation a gated phase kind, and who holds the forge credentials?
11. Confirm deleting the unused `omp.control.v1` session snapshot/delta/ingest messages.
12. Confirm making TLS mandatory for every non-loopback listener, including today's plaintext
    `auth-gateway`. This is a breaking change for anyone using it.

## References

- 0001 (four modes), 0003 (journal authority), 0004 (replication is subscription), 0005
  (controller/actor), 0006 (host policy, sandbox stub), 0007 (CoW views), 0009 (central bounding),
  0010 (one job primitive), 0011 (kill boundary), 0013 (seeding by cfg), 0037 (ACP editor I/O)
- `AGENTS.md` (Architecture; Locked Deviations; Allocation, Async and Channel discipline);
  `PHILOSOPHY.md` ("The same RPC boundary makes local, VM, remote, and headless-fleet deployments one
  topology")
- `docs/architecture/processes.md`; `docs/py/00-overview.md` (Principal identity; Idempotency and
  generation fencing); `docs/py/04-placement.md` (Remote-first users table; `omp_remote` authkey
  defect)
- Remote control: `crates/app/src/{rpc_mode,acp_mode,daemon,auth_broker_cmd,auth_gateway_cmd,chat_control,endpoint}.rs`,
  `crates/collab/{host,relay,link,crypto,replication}.rs`,
  `crates/driver/src/collab/{session,observer,remote_admission,host_bridge}.rs`,
  `crates/serve/src/{lib,auth,blob}.rs`, `crates/rpc/src/{tls,hello,uds}.rs`,
  `crates/core/src/principal.rs`, `crates/agent/src/{steering,approvals}.rs`,
  `crates/dom/src/{txn,subscribe,snapshot}.rs`, `crates/session/src/session.rs`,
  `crates/envd/src/server.rs`, `crates/env/src/partition.rs`, `crates/ext/src/trust.rs`
- Factory: `crates/adw/src/*.rs`, `crates/driver/src/adw/*.rs`, `crates/app/src/adw_cmd.rs`,
  `crates/agent/src/{jobs,cancel}.rs`, `crates/driver/src/subagent/workpool*.rs`,
  `crates/envd/src/{schedules,schedule_plan,journal_runtime,blobs}.rs`,
  `crates/envd/src/workspace/operations.rs`, `crates/journal/src/blob.rs`,
  `crates/driver/src/headless/kernel.rs`, `crates/sandbox/README.md`
- Protocols: `crates/proto/proto/omp/{collab,control,blob,gateway,env}/v1/*.proto`
- Proofs: `crates/e2e/tests/{p3_detached_jobs,p6_crash_resume,p9_isolation}.rs`
- v1 (`origin/main`): `docs/collab.md`, `docs/auth-broker-gateway.md`,
  `docs/factory-greenfield-audit.md`, `docs/swarm-factory-integration-audit.md`,
  `docs/adw/factory-{discovery,inventory,cutover}.md`
