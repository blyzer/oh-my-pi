# ADR 0075 S0: transports for host credential placement in a remote Environment

Status: research note for ADR 0075 slice S0. Nothing here is implemented or authorized. No code or ADR was changed.
Date: 2026-11-25
Baseline: `origin/omp2` at `2f2f0ef331`. ADR 0075 is not on that baseline. It was read on branch `docs/adr-0075-owner-decisions` at `56604b8605` (`docs/adr/0075-extension-credential-host-placement.md`: D8 at line 51, S0 at lines 102-104).

Evidence keys:

- **OBSERVED**: read in source at the baseline. Citations are `path:line`.
- **INFERENCE**: the author's reasoning from observed code.
- **UNVERIFIED**: no run or test confirmed it.

No build, test or run was made for this note.

## Question

ADR 0075 D8 says a placed request in a remote Environment fails with "placement unavailable", and that the key must never cross the Environment wire. S0 asks: can a remote Environment get placement without the key crossing its wire, by a host-side proxy or by the remote-facade pattern? The ADR asks for one answer per Environment kind: a named transport, or `none`.

## Answer in short

- Every Environment kind that exists in the code is on the same machine as the session. The code uses "remote" for the project daemon behind an owner-only Unix socket (`crates/env/src/partition.rs:112-120`, `crates/envd/src/lib.rs:1158-1168`). There is no network or SSH Environment.
- For embedded Environments, placement needs no wire. The credential authority is an in-process trait object.
- For the attached composition, extension HTTP does not travel to the daemon. It ends on the local session host. Placement there needs no wire either, but the session host refuses HTTP frames today. That is a gap to close, not a transport problem.
- A request that does reach the daemon has no credential authority there. The only wire-safe transport is a relay on the existing wire, shaped like the reflection relay. No HTTP relay exists, so the answer is `none` today, with one named design.
- For SSH and any real network Environment, the answer is `none`.
- The remote-facade pattern exists, but only for inference, and its request type cannot carry an arbitrary HTTP call.

## What "Environment kind" means in the code

OBSERVED. The Environment protocol is `omp.env.v1`. `crates/env/src/lib.rs:1-6` says the client crate has no resources and that they "live behind the environment service in both in-process and remote deployments". The shipped client constructors are:

- `EnvClient::in_process` (`crates/env/src/client.rs:1049`),
- `EnvClient::from_channels` (`crates/env/src/client.rs:976`),
- `EnvClient::connect_uds` (`crates/env/src/client.rs:2611`),
- owner-pipe equivalents on Windows (`crates/env/src/windows.rs:126,168`).

`EnvServer::connect_owner_uds_frames` refuses a socket that is not owner-only and owned by the current user (`crates/envd/src/server/session.rs:1121-1130`). The server side is `serve_uds` (`crates/envd/src/server/session.rs:1616`). `EnvServer::serve_io` accepts any byte stream (`crates/envd/src/server/session.rs:1549`), but its production callers are the Unix socket (`session.rs:1798`) and the Windows pipe (`crates/envd/src/windows.rs:103`).

`ProjectEnvironment` composes these kinds (`crates/envd/src/lib.rs:684`):

| Kind | Composition | Where |
| --- | --- | --- |
| Embedded | `EnvServer::open_local` served in-process; the fallback when no daemon is joinable | `lib.rs:944-973`, `977`, `1015`; `session.rs:238` |
| Isolated | Same in-process host, no extension hosts (`worker_config(.., &[], &[], ..)`) | `lib.rs:1253-1275` |
| Attached | A session-only local host (`open_session_host`) plus the project daemon (`open_project`), joined by a partition router | `lib.rs:838-941`, `1109-1168`; `session.rs:495`, `842`; `crates/e2e/src/baseline/topology.rs:4-7` |
| Document environment | A separate document-protocol server, not `omp.env.v1` | `crates/envd/src/docserver/environment.rs:265`; `session.rs:258-275` |

Adjacent, and not Environments: `ExecutionBackend`s (`LocalBackend`, `ReapiBackend`, `SshBackend`) and the inference gateway. They are covered below because they are the closest existing examples of the two patterns in the question.

## Where an extension's HTTP request ends up

OBSERVED. This decides most verdicts.

- Python extensions connect to a per-extension data socket (`crates/py/src/bindings.rs:4113`, `crates/envd/src/lib.rs:1687`). That socket is served by the session host: `spawn_extension_data_servers(&server, ..)` at `lib.rs:1074` (embedded), `1218` (attached) and `1310` (isolated). In the attached composition `server` is the session-only host. The e2e note says the local host carries "session tools and Python extensions" (`topology.rs:4-7`).
- The partition router sends `Worker`, `Site`, `Resource` and `Mcp` data to the local side. It sends `HttpRequest` to the remote (daemon) side (`partition.rs:434`, `route_data` at `462-487`).
- Each `EnvServer` constructs its own `HttpEgressHost::new()` (`session.rs:186`; `crates/envd/src/http_egress.rs:28-33`), which takes no authority.
- The session-only host is built with no environment authorities (`session.rs:1082-1085`, `Self::new(identity, None, ..)`). `dispatch.rs:281-288` answers any frame in `requires_environment_host` with "environment authority frame reached a session-only host". `HttpRequest` is in that list (`dispatch.rs:116-143`, entry at `136`). The handler is at `dispatch.rs:1052`.

INFERENCE. Taken together, an extension's `omp.env.http_post` (`crates/py/src/env_backend.rs:85-98`) in the attached composition reaches a session-only host that refuses it. I found no test that covers HTTP egress in the attached topology (only `crates/env/tests/extension_client.rs` mentions HTTP). This is UNVERIFIED by any run.

## Verdicts per Environment kind

### Embedded and isolated: in-process credential authority

Transport: a trait object bound into `HttpEgressHost` at composition, with no wire at all. The code already has this shape for forge credentials: `ForgeCredentialBridge` holds a `OnceLock<Arc<dyn CredentialAuthority>>` and `bind` installs it (`crates/envd/src/forge_url.rs:181-186`, `261-266`). This is ADR 0075 D4.

Evidence that it fits: the host that owns the credential store and the host that runs egress are the same process, and extensions reach that egress through `lib.rs:1074`. Isolated Environments have no extension hosts (`lib.rs:1275`), so only internal callers could use it.

Verdict: works. The key does not cross any Environment wire.

### Attached, extension-originated requests (session-only host)

Transport: the same in-process authority, bound on the session host. The credential store lives in the session process (the production stack and its `credential_authority`, `crates/driver/src/registry.rs:1635-1636`). The unix socket to the daemon carries no part of the request.

Gap: the session-only host rejects `HttpRequest` today (`dispatch.rs:281-288`, `116-143`). So this verdict is "would work, after a code change that gives the session host an egress path". The code does not do it now.

Verdict: a named transport (in-process, on the session host), conditional on that change. UNVERIFIED at run time.

### Attached, requests that are routed to the daemon

Facts. `partition.rs:434` routes a client-issued `HttpRequest` to the daemon. The daemon (`omp envd`) is composed with `omp_driver::bridges::builtin(..)` and a default `InferenceBridge` (`crates/app/src/cli.rs:3236-3245`). I found no call that binds a credential authority there. The only non-test `ForgeCredentialBridge::bind` calls are in `crates/app/src/chat_services/misc.rs:293` and `crates/app/src/grievances_cmd.rs:110`, and both bind a fresh bridge, not the Environment's. So the daemon holds no credential authority, and ADR 0075 D8 forbids sending it the key over the socket.

Transport that would work: a server-initiated relay on the existing owner-socket wire, shaped like the reflection relay. The daemon does not own the model, so it sends a typed `ReflectionQuery` on the issuing request, and the session answers it with a `ReflectionAnswer` (`crates/envd/src/reflection_relay.rs:1-25`; capability `REFLECTION_RELAY_CAPABILITY`, `crates/env/src/client.rs:206`; messages at `crates/proto/proto/omp/env/v1/env.proto:2072-2106`; answer routing in `partition.rs:391`). The approval and edit-repair relays follow the same pattern (`client.rs:191,198`). For HTTP, the daemon would send the request description and provider id, the session host would add the credential and perform the HTTP call itself, and only the response would return. The key stays on the session side.

Today: none exists. The relay messages are typed per feature, and no HTTP relay is defined. This transport is INFERENCE from the reflection relay, not a design that exists.

Verdict: `none` today; the reflection-shaped relay is the one named candidate. Cost: a new proto message pair, a new capability and a new e2e test.

### Network or SSH Environment

Verdict: `none`. No such Environment exists.

- The Environment wire has only in-process, Unix socket and Windows pipe transports (see "What 'Environment kind' means").
- A grep of production code in `crates/env`, `crates/envd`, `crates/driver`, `crates/app` and `crates/sdk` for `TcpStream::connect` and `TcpListener::bind` found no Environment use (only the browser relay, the gateway listener at `crates/app/src/daemon.rs:418`, and a reachability probe).
- `omp_ssh::SshService` offers `exec`, `open_interactive`, and `local_forward` (`crates/ssh/src/lib.rs:838,866,902`). `local_forward` binds a loopback listener on the host and connects out through SSH (`lib.rs:879`). That lets the host reach a remote service. It does not let a remote process reach the host. I found no reverse-forward or Unix-socket forward in the crate (grep for `tcpip_forward`, `streamlocal`, `reverse`, `forwarded_tcpip` found no match).
- `SshBackend` runs argv only, with no environment and no header channel (`crates/exec-ssh/src/lib.rs:76`; `ssh/src/lib.rs:902-934`). It is an `ExecutionBackend`, not an Environment.

If a network Environment were built, the two candidates would be a relay as above, carried over whatever byte stream it uses, or a host-side service the remote dials. Neither exists.

### Document environment

Verdict: `none`, not applicable. It speaks the document protocol and has no HTTP egress.

## Adjacent patterns, checked against the question

### Host-side proxy

- `crates/envd/src/sandbox_proxy.rs` is a session-owned forward proxy for sandboxed commands. It enforces host and port grants and tunnels `CONNECT` (`sandbox_proxy.rs:1`, `109-141`, `905`). It runs on the Environment host, not on the credential host, and a `CONNECT` tunnel carries TLS, so it cannot add a header. A grep found no credential injection in it. Verdict: not a placement transport. (Read by grep, not in full.)
- `ReapiBackend` shows host-side placement without any Environment: the host process dials the REAPI endpoint and sets the key as gRPC metadata (`crates/exec-reapi/src/execute.rs:302`, source in `crates/driver/src/exec_composer.rs:640-676`). No Environment wire carries the key. It is an existing example of the guarantee, not an Environment kind.
- One direction to keep straight: a `!command` credential source runs inside the Environment (`crates/driver/src/bridges.rs:181-196`; `exec_composer.rs:660`). So a secret already travels Environment to host for that source. That is the opposite direction to placement, and a remote Environment would make it cross a wire too. ADR 0047 C1 should account for it.

### Remote-facade pattern

- It exists for inference. `SearchBridgeHost` has a `Remote(InferenceClient<Channel>)` variant and `bind_remote` (`crates/envd/src/search_backend.rs:36-39,52`). The headless composition binds it when a gateway channel is supplied (`crates/driver/src/headless/kernel.rs:2780,2827`), and then "credentials live on the gateway host" (`kernel.rs:2263`). The Environment itself stays local in that composition (`kernel.rs:2160,2201`).
- The gateway's `ForwardProxy.Forward` is credential-free by design: "Credential selection, refresh, and header/query injection remain server-side" (`crates/proto/proto/omp/gateway/v1/gateway.proto:31-40`; `crates/app/src/gateway_rpc.rs:1-45`).
- Its request type is `NativeRequest`: a model plus a closed enum of inference paths (`crates/proto/proto/omp/inference/v1/inference.proto:989-1011`). It cannot name a BuildBuddy or any other arbitrary URL. So the existing facade cannot carry ADR 0075 requests. A new service of the same shape would be needed.
- The gateway can be a TCP endpoint with bearer auth (`crates/app/src/endpoint.rs:14-19`; `crates/app/src/cli.rs:1871`). INFERENCE: that bearer is itself a credential on the wire, so a facade moves the problem to a narrower credential rather than removing it.

## Verdict table

| Environment kind | Transport | Verdict |
| --- | --- | --- |
| Embedded | In-process `CredentialAuthority` bound into `HttpEgressHost` (`forge_url.rs:261`, `session.rs:186`) | works, no wire |
| Isolated | Same; no extension hosts (`lib.rs:1275`) | works for internal callers |
| Attached, extension requests | In-process authority on the session host (`lib.rs:1218`, `registry.rs:1635`) | would work; blocked today by `dispatch.rs:281-288` |
| Attached, daemon-routed requests | Relay on the owner socket shaped like the reflection relay (`reflection_relay.rs:1-25`) | `none` today; one design named |
| Network or SSH Environment | n/a (no such kind; no reverse forward at `ssh/src/lib.rs:866-879`) | `none` |
| Document environment | n/a | `none` |
| Facade (inference gateway) | `ForwardProxy.Forward` (`gateway.proto:31-40`) | wrong request shape; new service needed |

## What is unverified

- Nothing was built or run. Every claim is from reading source at `2f2f0ef331`.
- That an extension's `http_post` is refused on the attached session host. The guard at `dispatch.rs:281-288` and the list at `116-143` say so, but no test exercises it. A cheap check is an e2e attached-topology case with a Python extension calling `omp.env.http_get`.
- That the daemon has no route to a credential store at run time. I read the composition at `cli.rs:3236-3245` and grepped for `bind` callers only.
- Windows named-pipe parity: only function signatures at `crates/env/src/windows.rs:126,168` were read, not the owner checks.
- The negative claims (no TCP or SSH Environment, no reverse forward) rest on greps over the crates named above, not every crate.
- `sandbox_proxy.rs` was read by grep, not in full.
- The reflection-shaped relay is a design inference. Its fit for streaming bodies, size limits and cancellation was not examined.
- ADR 0075 line numbers are from `56604b8605` and may move.
