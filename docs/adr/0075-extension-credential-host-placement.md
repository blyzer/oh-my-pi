# 0075. Extension HTTP credentials are placed by the host; the key never reaches the extension

Status: accepted by the owner on 2026-11-25, with conditions (the answers to O1 to O5 below; enabling the credential on REST waits for the ADR 0040 amendment, which lands first)
Date: 2026-11-25
Area: control-plane

Read against `omp2` at `4f625fabe4`.

**Review status.** Accepted by the owner on 2026-11-25 with the answers to O1 to O5 recorded under "Owner answers". A read-only cross-provider review (OpenAI family, 11 findings) was applied on 2026-11-25. Its text is posted as a comment on PR #725., outside the repo. The owner chose option A (host placement) and D1 records that choice. D2 to D12 are the design that follows from D1, the spike and that review. The owner accepted the record and answered the five open questions the same day. Items still marked "proposal" below stay proposals.

## Context

An extension that calls an authenticated REST API needs a credential header on its HTTP request. The first case is the BuildBuddy extension (`extensions/buildbuddy`). It calls the BuildBuddy Run API through `omp.env.http_post` with the header `x-buildbuddy-api-key`.

### How the extension gets the key today

- On `omp2` the extension reads the key itself: the `BUILDBUDDY_API_KEY` environment variable, else `security find-generic-password -s buildbuddy-omp2-trial -w` run in the extension process (`extensions/buildbuddy/src/omp_buildbuddy/client.py:32-55`). It writes the key into the request headers (`client.py:74`).
- PR #720 (open) puts the key into the delegated-run JSON as a plain `api_key` argument. Its review rejects it (P0, confirmed). The key bypasses the credential contract, and the extension manifest declares no credentials grant.
- ADR 0040 lets the credential reach "only gRPC metadata at the backend boundary, wrapped in `SecretString`" (`0040:802-807`). `crates/exec-reapi/src/profile.rs:15-20` types it as "destined for one gRPC metadata header and nowhere else". The REST Run API path is not named. One operator credential source, `sv_exec_remote_credential` (`crates/driver/src/exec_composer.rs:79`), feeds the gRPC path and, in PR #720, the delegated Run API path.

### The credential channel is not live (verified in code at `4f625fabe4`)

1. The credentials CONTROL domain is not wired in production. `crates/app/src/chat_cmd.rs:1396` and `crates/app/src/session_runner.rs:190` bind `ExternalDomainControlFactories` with `ui` only. `crates/envd/src/server/control_factories.rs:192` denies every other domain with "no active credentials CONTROL lease authorizes this session".
2. `credential_control_grants()` (`crates/driver/src/secrets/mod.rs:25`) has no caller. It reads `credentials.allow|import|reveal` from the extension's own manifest (`:46-49`), so wiring it as written would let an extension grant itself.
3. Discovery gives native extensions `capabilities` as `{"data": [...]}` (`crates/driver/src/discovery/native.rs:669`). `capability_grants` (`crates/ext/src/config.rs:2092`) never holds `credentials.*`, so `credentials.allow` never reaches grants. `credentials.*` is also in neither the DATA list nor `CONTROL_CAPABILITIES` (`crates/envd/src/policy.rs:57`). No extension can hold a credential grant today.
4. The operator grant record is `<data>/ext/grants.toml` (`crates/ext/src/trust.rs:39`). `Grant` (`trust.rs:83`) binds extension id, publisher, layer, workspace, scope and a digest of the declared capability set. CONTROL grants are honoured only for user-layer extensions under a kernel-enforced exec sandbox (`crates/driver/src/discovery/native.rs:751-790`).

### What the host already has (read-only spike)

- `CredentialLease::apply_header(&HeaderPlacement{name, prefix}, ..)` writes the secret into a named header, marks it sensitive and zeroizes its buffers. The caller never sees plaintext (`crates/ai/src/auth/lease.rs:352`, `spec.rs:508-520`). `name = "x-buildbuddy-api-key"` with an empty prefix is expressible.
- `CredentialAuthority::provider_lease` sits behind `ForgeCredentialBridge` (`crates/envd/src/forge_url.rs:153,181`). *Correction, S4 (verified at `86497f9f6b`):* this record first said production binds it at `crates/driver/src/headless/kernel.rs:2843` and `crates/app/src/chat_services/misc.rs:293`. It does not. The bridge an `EnvServer` owns (`ProjectEnvironment::github_credentials()`, `crates/envd/src/lib.rs`) is bound by no `omp-app` or `omp-driver` code: `kernel.rs:2843` binds the inference bridge and `misc.rs:293` builds a separate share bridge. S4 therefore does not reuse it and adds its own authority (`PlacementAuthority`). Forge URL egress does lease then `apply_header` (`forge_url.rs:549`).
- Precedent: `SEARCH_HTTP`. "Core performs transport and credential placement ... the key is never revealed to Python" (`docs/py/13-inference.md:532`).
- Environment data operations know the caller. The connection identity is `extension:{layer}:{tier}:{extension}` (`crates/envd/src/server/connection.rs:326`). DATA capabilities are exact strings checked at `crates/envd/src/server/forward.rs:2741`.

### Missing, and limits

- `HttpEgressHost::new()` takes no dependencies and knows neither the caller nor credentials (`crates/envd/src/http_egress.rs:31`). `HttpRequest` has no credential field (`crates/proto/proto/omp/env/v1/env.proto:1250`). `omp.env.http_post` has no such parameter (`crates/py/python/omp/env.py:1816`). There is no catalog auth spec for BuildBuddy, and the REAPI key comes from a convar source (`sv_exec_remote_credential`), not the store. Option A is therefore not one extra parameter.
- On a cross-origin redirect, egress removes only `Authorization`, `Cookie`, `Host` and `Proxy-Authorization` (`http_egress.rs:95-100`). A custom header such as `x-buildbuddy-api-key` is forwarded, for up to 10 hops. PR #723 (draft) generalizes the removal.
- **Spike limit.** The spike holds only for an embedded Environment. The bridge is a trait object bound in the host process, and no secret transport to a remote Environment was found. Lead, not a finding: the search facade has a remote variant (`crates/envd/src/search_backend.rs:36,52`).
- ADR 0047 C1 requires a credential-containment record before a remote backend is composed by default. This record covers extension HTTP egress only. Whether it counts toward C1 is not decided here.

## Decision

- **D1 (owner decision).** The API key never leaves the host credential store. The host holds a per-extension, per-provider grant, and the HTTP egress host attaches the credential header on the extension's behalf. For a placed request the extension MUST NEVER receive the key, as an argument, environment variable or return value. The guarantee holds only for endpoints that meet D10.
- **D2. Request shape.** The extension asks for placement by provider id only: an additive `credential` field on `HttpRequest` and a keyword on `omp.env.http_post`. The header name and prefix come from the provider's catalog auth spec (`HeaderPlacement`), never from the request. A placed request that sets the placed header itself MUST be refused. Its HTTP authority is derived only from the validated URL, and a caller-supplied `Host` header MUST be refused before sending. Today egress passes any caller header, `Host` included, on the first request (`parse_headers` at `crates/envd/src/http_egress.rs:77`) and strips it only on a cross-origin redirect (`:98`).
- **D3. Grant.** Placement requires a host-held grant that names the extension identity, one exact provider id, the canonical `(scheme, host, effective port)` tuples the key may reach, and the operations (method and path prefix) that may carry it. The scheme is https. The host is lowercase ASCII with IDNA applied and the port is the effective one (443 when absent). A second port on the same host is a different origin. Globs and a default provider are NEVER allowed. The extension manifest MAY request a grant and MUST NEVER create or widen one. The grant record is `<data>/ext/grants.toml`, the file `omp ext grant` writes (owner answer O1). A project-scoped extension NEVER holds one (D13).
- **D4. Placement point.** `HttpEgressHost` receives an optional credential authority at composition, as the forge resources do. For each request it checks the grant, takes a lease and applies the header just before send. The lease's join buffer is zeroized. The `HeaderValue` that `apply_header` inserts into the caller's `HeaderMap`, and the transport's send buffers, hold plaintext and are NOT wiped: `set_sensitive(true)` changes Debug output and is not zeroization (`crates/ai/src/auth/lease.rs:352-369`). The claim is that plaintext never reaches Python and never reaches a diagnostic sink, not that memory is confined. Logs, errors, traces, the journal and the audit row MUST redact it, and a test asserts that.
- **D5. Per hop.** Placement is decided on every hop, before the hop is sent. A hop applies only to a granted `(https, host, port)` and a granted operation. A redirect to any other origin is refused before a connection is opened: zero requests reach it, and a 307 or 308 does not replay the body there. This does not depend on PR #723, which still lands for headers an extension sets itself.
- **D6. Refusal.** No grant, unknown provider, host not in the grant, non-https URL, no stored credential, or no authority bound: the request is not sent and no connection is opened. The host makes this refusal, not the sender. The typed error names which class applied. There is NEVER a fallback to the environment, the Keychain or an argument.
- **D7. Audit (proposal).** Each placed request writes a secret-free audit row: extension identity, provider, host, request id. The sink is chosen in S4. If the write fails, the request is not sent.
- **D8. Embedded only.** A request with `credential` in a remote Environment fails with a typed "placement unavailable" error. The key MUST NEVER cross the Environment wire to make it work. A later record designs the remote transport. S0 gathers the facts.
- **D9. Scope.** This record covers HTTP egress placement. It does not change `omp.creds.reveal` or `mint_scoped`, the existing `reveal` call in `extensions/search-provider/src/omp_search_provider/device.py:121`, or the Keychain import. The import stays a separate host-side step.
- **D10. Endpoint trust and operations.** Egress returns response headers, body and final URL to Python. A granted endpoint is therefore a trust decision: the operator approves it only if it does not echo request headers or bodies. A grant lists only operations that need the credential and that return no request echo. Defense in depth (proposal): egress refuses to return a response whose headers or body contain the placed value. That check is not a guarantee, because an endpoint can re-encode the value.
- **D11. Wire gate.** A sender MUST send `credential` only to a receiver that advertised a placement capability in the handshake, as `edit-repair` is (`crates/env/src/client.rs:191`). An older receiver ignores the unknown protobuf field and would send the request unplaced, so the sender fails locally when the capability is absent. The host refusal in D6 still applies when the capability is present but no authority is bound.
- **D12. Admission prerequisite.** An extension is admitted for placement only if its worker cannot reach the grant writer (`omp ext grant`, `grants.toml`), the credential store (`credentials.db`) or the operator reveal command (`omp token`). The BuildBuddy client already launches a subprocess (`extensions/buildbuddy/src/omp_buildbuddy/client.py:43`), so the sandbox must deny those paths and that command to an extension worker. Whether it does is not verified.
- **D13. Project scope (owner decision O2).** A project-scoped extension NEVER holds `credentials.*` or a placement grant, and has no access to credential storage. Only an
extension installed in the user layer can hold a grant. This does not restrict an extension from reading shell environment variables it inherits. omp does not control that,
and this record makes no claim about it.

## Alternatives considered

| Alternative | Why not |
| --- | --- |
| **B. The extension asks for the secret over the control channel** (`omp.creds.reveal`, separate `credentials.reveal` grant, per-call audit). | Plaintext enters Python and can leak through arguments, logs, tracebacks and redirects. It needs the whole credentials CONTROL domain bound in every adapter (chat, session runner, print, RPC, ACP, headless, subagents), a new `ExtHostSpec` field filled from a host-held grant, and an envd CONTROL identity filter that admits `credentials.*`. Documentation limits reveal to "the import path only" (`docs/py/13-inference.md:1790`) and ADR 0049:75 says adapters get only `mint_scoped` tokens. A revealed value is not registered for masking (`crates/driver/src/auth_backend.rs:471-494`). It also breaches "nowhere else" in ADR 0040 for the REST path unless amended. The spike found A needs the per-extension provider scope but not the credentials CONTROL domain. |
| **Plain argument, as in PR #720.** | The review confirmed it as a P0. It bypasses the credential contract and the manifest declares no grant. `.expose().to_string()` copies the secret into plain `String`s; whether they are wiped was not traced. The ambient `BUILDBUDDY_API_KEY` fallback stays. |
| **Status quo: the extension reads env or Keychain itself.** | The host has no grant, no scope and no audit. The key sits in extension memory. A custom header is forwarded on redirects. |
| **A Rust Run API client in core, key never leaves Rust.** | The 2026-11-05 amendment to ADR 0040 moves vendor integration out of core, and the orphan `omp-exec-buildbuddy` crate has left the workspace (`0040:1180-1181`). |

## Consequences

**Easier.**
- The key never enters Python for this path. Args, logs, tracebacks and extension memory do not hold it. Host buffers do (D4).
- One placement path serves any extension that needs a REST credential: provider plus host plus catalog spec.
- Redirect safety holds by construction, not by header name.
- A grant is per extension and provider, so it can be revoked without touching other extensions.

**Prohibited.**
- Passing the key to an extension as an argument, environment variable or return value for this path.
- Globs, default providers, plaintext http, and any host outside the grant.
- Placement for a remote Environment until a later record designs the transport.

**Costs accepted.**
- Embedded Environment only (D8).
- A new proto field, Python keyword and generated operation spec (additive).
- A catalog entry and a stored credential for BuildBuddy. A user whose key lives only in the Keychain item the extension reads today needs the store import first. That import is a separate step.
- A grant must exist before an extension can use placement. S2 was blocked on O1 and O2; the owner answered both (D13).
- The same key can sit in two places, the convar source for gRPC and the store for REST, until a later record unifies them.
- Each endpoint needs an operator trust decision (D10). An endpoint that reflects request headers defeats the guarantee, so such an endpoint MUST NOT be granted.

## Proposed amendment to ADR 0040

Text of the amendment. ADR 0040 is not edited by this record; slice S-amend edits it. The owner decided (O3) that the amendment lands before any other slice. Acceptance of it is also a prerequisite for enabling the credential on REST (S5 and any live grant for `buildbuddy`). When it lands it is an `## Amendment (YYYY-MM-DD)` section in ADR 0040, which keeps its number. It replaces the first bullet of "Credential rules" (`0040:803`) with:

> The credential reaches only two places. (a) gRPC metadata at the backend boundary, wrapped in `SecretString`. (b) A request header attached by the host's HTTP egress on behalf of an extension that holds a host-held grant for that provider and host (ADR 0075). No other component receives the plaintext, and an extension never receives it, whether in arguments, environment or return values. The remaining bullets (never in argv, action env, logs, telemetry or committed config; tracing redacts the header) apply to both paths. Any other hop is refused until this section names it.

Companion edits proposed for the same change, not made here: a `credential` keyword on the `http_post` signature in `docs/py/11-env.md:1380`, the page the runtime symbol spec names, and a pointer from ADR 0049:75.

## Implementation sequence

Slices are one PR each. The owner accepted this record on 2026-11-25; slices run in the order of the Implementation gate. PR #723 (cross-origin redirect hardening, merged by the owner on 2026-11-25) is a prerequisite and not a slice here. Proposal: PR #720 does not merge with the key in its arguments.

**S0. Remote Environment transport note.** Docs only, independent.
- Owns: a note under `docs/research/`. Question: can a remote Environment get placement without the key crossing its wire, by a host-side proxy or by the remote-facade pattern?
- Acceptance: per Environment kind, a named transport or "none", with `path:line`. No code changes. Tests: none (read-only).

**S1. Catalog auth spec and stored credential.** Independent.
- Owns: provider catalog data (`crates/catalog/data`), `crates/ai/src/auth`.
- Acceptance: a `buildbuddy` provider whose spec resolves for `CredentialNeed` and yields `x-buildbuddy-api-key` with an empty prefix, value marked sensitive. The credential reaches the store only through the existing store write path. Non-goals: Keychain import, any extension change.
- Tests: unit (`just test-pkg omp-ai`, `omp-catalog`).

**S2. Grant record and enforcement.** Unblocked by the owner answers to O1 and O2 (project layer: D13).
- Owns: `crates/ext/src/trust.rs`, `crates/driver/src/discovery/native.rs`, `crates/envd/src/server/connection.rs` and `forward.rs`.
- Acceptance, named tests: `manifest_request_without_host_grant_is_denied` (fails if scopes are read from the manifest), `credential_grant_refuses_glob_and_empty_provider`, `changed_credential_request_invalidates_grant`, `grant_binds_extension_identity`, `grant_binds_scheme_host_and_port`. The project-layer outcome is pinned by a test once O2 is answered. D12 admission: a negative test from an actual extension worker that fails to write the grant file, read `credentials.db` and run `omp token`. Non-goal: any placement.
- Tests: unit (`just test-pkg omp-ext`, `omp-driver`, `omp-envd`).

**S3. Wire shape, inert.** Independent of S1 and S2.
- Owns: `env.proto`, `crates/py` (`env.py`, `env_backend.rs`, bindings), `crates/tool/src/spec_generated.rs`, `docs/py/11-env.md`, the envd dispatcher (`crates/envd/src/server/forward.rs`) and the handshake capability (`crates/env/src/client.rs`).
- Acceptance: `omp.env.http_post(url, credential="p")` encodes the new field. Old requests decode unchanged. The host refuses a request that carries `credential` while no authority is bound, with the typed "placement unavailable" error (D6). The sender fails locally when the receiver lacks the capability (D11). Two named tests count requests at a listener: `unbound_host_refuses_credential_request_with_zero_requests` and `unsupported_peer_gets_zero_requests`. `credential` is documented and `scripts/check-docs-surface.py` passes.
- Tests: contract/py (`just test-pkg omp-py`), unit with a counting loopback listener (`just test-pkg omp-envd`), proto round-trip unit.

**S4. Host placement in `HttpEgressHost`.** Depends on S1, S2, S3.
- Owns: `crates/envd/src/http_egress.rs`, `server/forward.rs`, composition in `omp-driver` and `omp-app`.
- Acceptance, named tests: `placed_header_reaches_only_the_granted_host`, `same_host_other_port_receives_zero_requests` (two listeners on one hostname; the second sees no connection), `ungranted_redirect_hop_receives_zero_requests` (302 and 307 with a body; no reliance on PR #723), `caller_host_header_refused`, `plaintext_absent_from_logs_errors_and_traces`, `no_grant_means_no_send`, `non_https_refused`, `placed_header_set_by_request_refused`, `ungranted_operation_refused`, `remote_environment_refuses_placement`, `audit_failure_blocks_send`, and, if the D10 proposal is adopted, `response_containing_placed_value_is_not_returned`.
- E2E: one `crates/e2e` proof with a loopback fake server. The header arrives and nothing the extension can read holds the key, for a fake endpoint that does not echo headers (D10). A second fake that echoes them proves the D10 check, or the proof is dropped. The loopback and TLS seam is chosen in the PR, with the e2e recipe that covers extension grants.
- Gate (proposal): unreachable without a grant record, so no convar.
- Tests: unit (`just test-pkg omp-envd`), e2e.

**S5. Migrate the BuildBuddy extension.** Depends on S4 and the PR #720 outcome, and on the ADR 0040 amendment (S-amend).
- Owns: `extensions/buildbuddy`, and `crates/driver/src/delegated_backend.rs` if PR #720 lands first.
- Acceptance: the extension calls `http_post(..., credential="buildbuddy")`. No `api_key` in any argument JSON. `client.py` no longer runs `security` and reads no key from `os.environ`. On denial the migrated client sends nothing even when `BUILDBUDDY_API_KEY` is set. There is no warning period (O4): the fallback is removed, and nothing reads the variable. A not-granted error maps to a typed extension error with no key text. The client reads `response.status_code` today (`client.py:106`) and `omp.env.HttpResponse` exposes `status` (`crates/py/python/omp/env.py:1746`), so the migration fixes that too.
- Tests: extension unit tests that use the real `omp.env.HttpResponse` and fake only the transport. One success and one HTTP failure (status 400 or above). A test with the variable set and no grant asserts zero calls (`just check-extensions`).

**S-amend. ADR 0040 amendment and companion doc edits.** Docs only. Acceptance of it gates S5 and any live `buildbuddy` grant. It comes before S0 to S4 (owner answer O3).

## Owner answers (2026-11-25)

The owner answered all five on 2026-11-25. The facts stay as the evidence behind each answer; a "Proposal" line is the earlier proposal, not the decision.

- **O1. Where does the operator grant registry live, and who may write it?** Is it the same file as `omp ext grant`? Facts: that file is `<data>/ext/grants.toml`, written by `omp ext grant <id> [--yes]` (`crates/app/src/ext_cli/mod.rs:2004`) or by `OMP_EXT_GRANT` (`trust.rs:404`). `ext grant --yes` is reachable by any local process, and so is `omp token`, which prints a provider credential (`crates/app/src/token_cmd.rs:20`). D12 makes extension isolation from both an admission prerequisite. D3 fixes only that an extension cannot write its own grant. **Answer (owner, 2026-11-25).** Yes: the registry is the same file as `omp ext grant`, `<data>/ext/grants.toml`. Who may write it is unchanged: D3 (an extension cannot) and D12 (admission prerequisite). S2 is unblocked on this question.
- **O2. Can a project-scoped extension ever hold `credentials.*`?** Proposal (not adopted): never. Facts: the `.omp/extensions/buildbuddy` project link is rejected by discovery (`crates/driver/src/discovery/native.rs:520-533`) and is local and untracked, so nothing in the repo reaches the user layer today. To hold a grant the operator must install a real copy under `<config>/agent/extensions`. Whether a supported install path exists is unverified. **Answer (owner, 2026-11-25).** No. A project-scoped extension has no access to credential storage unless it is installed in the right place (the user layer). This does not remove an extension's ability to use shell environment variables; omp does not control that. Recorded as D13. S2 is unblocked on this question.
- **O3. Is ADR 0040 amended first?** Facts: its credential rule names gRPC metadata only (`0040:802-807`). The proposed text is above, and its acceptance gates REST enablement but not the inert slices. ADR 0049:75 and `docs/py/13-inference.md:1790` carry related wording that the companion edits would touch. **Answer (owner, 2026-11-25).** Yes: amend ADR 0040 first, then implement. S-amend lands before S0 to S4.
- **O4. What happens to existing `BUILDBUDDY_API_KEY` users?** Proposal (not adopted): a warning period, then removal. Whatever is chosen, after cutover the migrated client does not read the variable (S5). Facts: only the extension's fallback (`client.py:39`) is in scope. CI, `scripts/remote-verify.sh` and `crates/exec-reapi/tests/live_buildbuddy.rs` also use the variable and are not touched. The daemon already withholds secret-looking env names from children (`crates/envd/src/child_env.rs`). Keychain-only users also need the store import. **Answer (owner, 2026-11-25).** No migration period. The owner is the only user of `BUILDBUDDY_API_KEY`, so S5 may drop the fallback without a warning period.
- **O5. Does `reveal` ship in this pass, or only A?** Facts: A does not need reveal. Shipping reveal needs four things: the driver composes the credential control factory, every adapter binds `credentials`, `ExtHostSpec` carries provider scopes from a host-held grant, and the envd CONTROL identity filter admits `credentials.*`. `search-provider` already calls reveal (`extensions/search-provider/src/omp_search_provider/device.py:121`). Review input, not adopted: ship reveal only if a concrete need remains. **Answer (owner, 2026-11-25).** Only option A. `reveal` ships only if a concrete need remains (consistent with D9).

## Implementation gate

Accepted by the owner on 2026-11-25 with the answers above. Order: S-amend first (O3): the `## Amendment` to ADR 0040 lands before any slice. Then S0, S1 and S3, which are
independent and inert, and S2, unblocked by O1 and O2. S4 needs S1 to S3. S5 needs S4 and the PR #720 outcome; it needs no warning period (O4). `reveal` stays out unless a concrete
need appears (O5). Production changes are authorized only through these slices, one PR each.

## Status in omp

Status: Partially implemented. S-amend, S0, S1, S2, S3, S4 and S5 are in the tree. The BuildBuddy extension no longer reads or receives the key: it names `credential="buildbuddy"` and the host places the header. D12 is verified on macOS Seatbelt within the limits stated below, and `omp ext grant <id> --credential <provider> --origin … --operation …` writes the grant. For the delegated profile the host no longer resolves the old `sv_exec_remote_credential` value (`ExecutionSettings::needs_operator_credential`).

- **S-amend, S0, S1, S3** landed in PR #734. S-amend is the ADR 0040 amendment (`0040:2035`) and the ADR 0049 rule 7 pointer (`0049:76`). S0 is `docs/research/adr-0075-s0-remote-env-placement-transports.md`. S1 is the `buildbuddy` provider with a stored header credential and an empty prefix (`crates/catalog/tests/catalog_conformance.rs:2214`). S3 is `HttpRequest.credential` (`crates/proto/proto/omp/env/v1/env.proto:1268`) and the `credential-placement` handshake capability with the sender gate (`crates/env/src/client.rs:229,2993`).
- **S2, grant record and enforcement** (this slice; scopes come from the host record, never from the manifest):
  - Record. `CredentialGrant` (`crates/ext/src/credential_grant.rs:223`) is a `[[credential_grant]]` table in the same `<data>/ext/grants.toml` that `omp ext grant` writes (`crates/ext/src/trust.rs:182`, written field by field in `GrantsFile::write`). It binds extension id, publisher, layer, the manifest digest, one exact provider id, canonical `(https, host, port)` origins and `(method, path prefix)` operations. `validate` refuses an empty or glob provider, a non-https or glob host, port 0, a non-user layer, and empty origin or operation lists. A hand-edited row that fails validation holds nothing.
  - Persistence. `GrantsFile::persist_credential_grant`, `revoke_credential_grants` and `credential_grants_for` (`crates/ext/src/trust.rs:598,622,647`) go through the locked read-modify-write.
  - Decision. `CredentialGrants::authorize` (`crates/ext/src/credential_grant.rs:355`) is a pure per-URL decision with typed refusal classes (`NoGrant`, `ProviderNotGranted`, `NotHttps`, `OriginNotGranted`, `OperationNotGranted`). Request paths with dot segments, `%2e`, `%2f`, `%5c` or a backslash never match a prefix.
  - Admission. `admitted_credential_grants` (`crates/driver/src/discovery/native.rs:909`) fills `ExtHostSpec.credential_grants` (`crates/envd/src/worker.rs:189`) only for a user-layer extension (D13), after the protected-location checks, from a row bound to the extension's identity and manifest digest, and only under a confining exec sandbox. The sandbox is probed only when a matching row exists. A changed manifest voids the grant. `withhold_control_grants` (`native.rs:152`) also drops credential grants from caller specs and when the sandbox facts changed.
  - Manifest. `credential_control_grants` (`crates/driver/src/secrets/mod.rs:32`) no longer reads `credentials.*` from the manifest. `allow` is the host-granted provider list, and `import` and `reveal` stay empty (D9, O5).
  - Connection. The grants ride `ExtensionDataBinding::with_credential_grants` (`crates/envd/src/server/identity.rs:201`) into the connection, and `placement_refusal` (`crates/envd/src/server/forward.rs:2676`, called at `dispatch.rs:1057`) refuses a request that carries `credential` with a typed class before any connection is opened. A request the grant covers is still refused with `NoAuthority`, because nothing binds an authority until S4.
  - Tests. Unit: `credential_grant_refuses_glob_and_empty_provider`, `grant_binds_extension_identity` and `grant_binds_scheme_host_and_port` (`credential_grant.rs`); `manifest_request_without_host_grant_is_denied` (driver `native.rs` and `secrets/mod.rs`), `changed_credential_request_invalidates_grant`, `project_layer_extension_never_holds_a_credential_grant` (D13) and the sandbox, store-location and probe-count cases (`native.rs`); `grant_refusals_name_their_class_and_send_nothing` and `grant_binds_extension_identity` over a counting listener (`crates/envd/src/server/tests/protocol.rs`).
  - Done after S2: `omp ext grant <id> --credential <provider> --origin https://host[:port] --operation "METHOD /path" --yes` (`crates/app/src/ext_cli/credential.rs`) records the grant, bound to the extension identity and manifest digest, after a consent prompt that lists provider, origins and operations; it refuses a grant with no origin or operation and replaces the same provider. `--revoke --credential <provider>` withdraws one provider. D12 result, macOS Seatbelt (`crates/e2e/tests/p9_extension_isolation.rs`): the same probe code that succeeds outside the sandbox (positive control) is denied inside a real user-extension worker. Writing `grants.toml` and reading `credentials.db` fail with `EPERM`. `omp token --list buildbuddy` starts, because the sandbox does not block the `omp` binary, and stops on `EPERM` reading `credentials.key`. `omp ext grant` starts and fails before it writes anything, for a reason that test did not isolate (`failed to read project file`); the write it would end in is the one shown denied. `omp token <provider>`, which prints a secret, was not run. Only macOS Seatbelt was exercised; D12 is not verified elsewhere. The manifest declares no credential request grammar; the grant binds the manifest digest instead.
- **S4, host placement in `HttpEgressHost`** (provisional owner choices below, not yet ratified):
  - Authority. `PlacementAuthority` (`crates/envd/src/placement.rs:175`) is a `OnceLock` bound at composition, unbound by default, so an Environment composed without one (a remote one, D8) refuses with `PLACEMENT_UNAVAILABLE`. `EnvServer::placement_authority()` and `ProjectEnvironment::placement_authority()` expose it. `bind_placement` (`crates/driver/src/placement_credentials.rs:90`) binds it over the production credential stack in `crates/driver/src/headless/kernel.rs` (the `Ok(stack)` arm), with a JSON-lines audit file `<data>/placement-audit.jsonl`.
  - Per hop. `HttpEgressHost::request` (`crates/envd/src/http_egress.rs`) places or refuses a request that carries `credential`, with no fallback. Before the first hop `begin_placement` (`:150`) decides, in order: the grant (`CredentialGrants::authorize`), the catalog provider (`resolve_provider`, `placement.rs:214`: one auth spec, an API key in a header; name and prefix come from the catalog), the caller's headers (the placed header or `Host` set by the request is refused), and the bound authority. `Placed::place` (`:55`) repeats the grant check on every hop (a redirect hop that leaves the grant is refused before a connection is opened), leases the key, writes the audit row, and only then applies the header to a per-hop copy of the headers.
  - D7 adopted. The audit row (extension identity, provider, host, port, method, request id, hop) is written before the hop is sent; a failed write refuses the request (`AuditFailed`).
  - D10 adopted as defense in depth. `echoes_placed_value` (`:268`) refuses a response whose headers or body contain the placed value. It is not a guarantee: an endpoint can re-encode the value.
  - Key source. `StoredPlacementCredentials` (`crates/driver/src/placement_credentials.rs:45`) leases the provider's stored key when the provider has exactly one enabled stored account; none or several refuse as no stored credential, and no account is guessed.
  - Gate. The path is unreachable without a grant record (`ConnectionState.credential_grants`, S2) and a bound authority; there is no convar.
  - Tests. `crates/envd/src/placement_tests.rs`, over a loopback TLS server that counts accepted connections: `no_grant_means_no_send`, `placed_header_reaches_only_the_granted_host`, `same_host_other_port_receives_zero_requests`, `ungranted_redirect_hop_receives_zero_requests` (302 and 307 with a body), `caller_host_header_refused`, `placed_header_set_by_request_refused`, `non_https_refused`, `ungranted_operation_refused`, `remote_environment_refuses_placement`, `audit_failure_blocks_send`, `no_stored_credential_means_no_send`, `response_containing_placed_value_is_not_returned`, `plaintext_absent_from_logs_errors_and_traces`. Driver: `crates/driver/src/placement_credentials.rs` (one, none, several accounts). E2E: `crates/e2e/tests/p9_credential_placement.rs` (a real user extension, Seatbelt, skips off macOS).
  - Also changed. `crates/py/src/env_backend.rs` sent `omp.env.http_post` to a dispatcher that handles `omp.env.http.post` and takes `method` as a named argument, so `omp.env.http_get`, `http_post` and `http_put` raised `Unsupported` from a real extension. S4's end-to-end proof found it; the routing is fixed there.
  - Still true. The credentials CONTROL domain is unbound in production (Context 1). The BuildBuddy extension no longer passes or reads the key (S5), and `crates/driver/src/delegated_backend.rs` sends no `api_key` argument. D12 (the extension worker cannot reach the grant file, `credentials.db` or `omp token`) is unverified.

## References

- ADR 0040 (credential rules `:802-807`; amendment 2026-11-05, vendor integration leaves core), ADR 0047 (C1, G3), ADR 0049 (`:75`), ADR 0036 (Python-only extensions).
- `crates/exec-reapi/src/profile.rs`, `crates/envd/src/http_egress.rs`, `crates/envd/src/forge_url.rs`, `crates/ai/src/auth/lease.rs`, `crates/driver/src/secrets/mod.rs`, `crates/ext/src/trust.rs`, `docs/py/13-inference.md`.
- PRs #720 (key in arguments, review REJECT) and #723 (redirect header hardening, draft). Cross-provider review of this record: see Review status.
- Session evidence, not committed: the credential decision record, the final decisions, the option B design review, the option A spike and the PR #720 review.
