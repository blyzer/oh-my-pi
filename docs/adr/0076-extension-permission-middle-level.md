# 0076. A middle level for extension permissions: scoped capability grants, and an explicit scope where the worker profile widens today

Status: proposed
Date: 2026-11-27
Area: control-plane

Read against `omp2` at `dcd9c7894c`.

**Review status.** Proposed. Not owner-approved. One author read the code and the design documents named below, and one read-only machine review (the `reviewer` agent, on its configured model) audited every citation and attacked the design. Its findings are applied: the first draft had five material errors, listed under "Corrections from review". Every claim cites a file and line in that tree. "Not verified" lists what I did not check. The decision is gated on O1: the design documents and the shipped code disagree about whether extensions are a security boundary, and only the owner can say which one stands.

## Context

An extension runs in one of two places: inside a fixed kernel sandbox, or on the whole host. This record asks whether something should sit between them, and what. The evidence changes the usual framing. The sandboxed worker starts with almost no reach, so the gaps are not "the worker needs more access". They are two: the capabilities an extension is granted are coarse names, and the one place the worker's profile widens is implicit and coarse too.

### What exists

1. **Two tiers, and one of them is barely reachable.** `TrustTier` is `Sandboxed` or `Trusted` (`crates/ext/src/lib.rs:47`). A native extension is admitted as `sandboxed` unconditionally (`crates/driver/src/discovery/native.rs:806`). At runtime the only code that builds a `trusted` host key is the `--trusted-extension` flag, which is hidden from `--help` (`crates/app/src/cli.rs:366`, `:3856`; the tests assert it is absent, `:5023`, `:5035`). `omp ext install --tier trusted` is gated by consent that names the tier: `--yes` alone is refused and `OMP_EXT_GRANT` must carry `tier=trusted` (`crates/app/src/ext_cli/mod.rs:3313-3320`; `docs/py/14-deploy.md:2127`). I did not trace where a tier is written to `grants.toml`, and I found no runtime path that reads a recorded tier to spawn a trusted host. A trusted host has no sandbox (`crates/envd/src/exthost/spawn.rs:578`) and its tools default to the `exec` effect tier (`crates/envd/src/worker.rs:148-160`).

2. **The sandboxed worker's profile.** `spawn.rs:553-578` builds it: read the executable, its runtime images, the extension's Python site and entry file; write the environment socket; network disabled; degradation rejected (`spawn.rs:571`), so a host with no backend refuses the worker. The baseline has **no workspace access**. One manifest-driven widening exists: a `telemetry` or `telemetry_subscription` declaration row adds read and write on the whole workspace root (`worker.rs:1892-1899`, then `spawn.rs:565-567`; field doc `spawn.rs:96`). In the tree, `sidebar` is the one extension that triggers it (`extensions/sidebar/omp.toml:15`, capabilities `ui.slots` only). The widening is invisible in any capability name.

3. **What the backends can express.** The worker uses `Runner::native_command()`: Seatbelt on macOS, Bubblewrap on Linux, `UnsupportedHost` anywhere else (`crates/sandbox/src/runner.rs:1136-1142`). On Windows no sandboxed worker can start. Seatbelt and Bubblewrap express filesystem read and write scope, deny lists, network and environment scrubbing. Neither has `ProcNoExec` or resource limits (`crates/sandbox/src/capability.rs:320-344`). Those exist on gVisor and AppContainer, which the worker does not use. The D12 proof has the worker run `subprocess` to start `omp token` and `omp ext grant` (`crates/e2e/tests/p9_extension_isolation.rs:48`, `:83`), so the profile limits what a process can reach, not whether it can start one.

4. **A capability is a name, granted by declaring it, and a wrong name grants nothing.** The native admission path sets `spec.data_grants = Grants::supported(projection.capabilities)` (`native.rs:808`), which keeps only names in `CAPABILITIES` (`crates/envd/src/policy.rs:37`). That list has `env.net` and no `env.net.fetch`, so `env.net.fetch` is dropped there. It is the ADR 0050 D4 first-cut spelling for URL-scheme effects (`worker.rs:1521-1525`; `crates/envd/src/tool_url/scheme_generation.rs:46`), and `scripts/check-extension-conformance.py:14-18` and `:84-87` require it of network packages "until host grants align on `env.net` alone". When this record was written no manifest in the tree declared `env.net`; `buildbuddy` and `remote-approve` do since PR #756. The DATA gate for an HTTP request requires `env.net` (`crates/envd/src/server/forward.rs:2688-2695`), and a production endpoint carries only the manifest-derived grants (`crates/envd/src/server/identity.rs:128-159`, where the constructor that grants every name is test-only).

   **Executed, 2026-11-27.** I ran a throwaway probe: a copy of `p9_credential_placement` whose manifest, in the native `omp.toml` format, declares `capabilities = ["env.net.fetch"]`, driven through the real admission (`operator.admit`) and the production environment server. Result: admission produced **no DATA grants** (`[]`); `omp.env.http_post(url, credential="buildbuddy")` was denied with `Denied: effect envelope does not grant env.net; escalation is not re-prompted`; **0 requests reached the endpoint**. The same test with `capabilities = ["env.net"]` is the one that passes in CI (`p9_credential_placement`). So a manifest that declares only `env.net.fetch` gets no network grant, and the cause is the capability name, not the credential placement.

   **What that does not show, corrected 2026-11-27.** An earlier version of this record said the two in-tree extensions that call a DATA HTTP operation with only `env.net.fetch` declared, `buildbuddy` (`client.py:85`, `http_post`) and `remote-approve` (`device.py:39`, `http_get`), "are both affected". That was an inference, and it is not established. I then admitted the real `extensions/buildbuddy` through `admit_native_extensions` and it was rejected before any grant: `EManifestParse ... invalid type: map, expected a sequence` at its `[requires]` table. All seventeen in-tree manifests write `[requires]` as a table (`python = ">=3.14"`), so none of them reaches the DATA gate by the native `omp.toml` route. The other route that builds a host spec, the hidden `--trusted-extension` flag, assigns no DATA grants (`crates/app/src/cli.rs:3851-3894`), and I did not trace what the gate does for such a host. Which path loads them was then established; see the next paragraph.

   **Resolved, 2026-11-27.** The native route is the one that loads them: `load_manifest` (`native.rs:607-645`) reads a root's `omp.toml` with `DeploymentManifest::parse`, directly, and falls back to `pyproject.toml` `[tool.omp]` only when there is none. I found no packaging step that lifts the shorthand `[requires]` and `[settings]` forms: `extensions/buildbuddy/pyproject.toml` builds its wheel from `src/` alone (I read that one of the seventeen), and a search of `crates/`, `scripts/` and `.github/` found no code that rewrites them. `crates/e2e/tests/p9_delegated_route.rs:75-78` independently records that production admission rejects the shipped manifest and strips `[requires]` by hand. A comment in `crates/ext/src/config.rs` (`bundled_extension_capabilities_load`) calls the shorthand forms the layout of the bundled manifests, and its test projects only `id`, `entry` and `capabilities`, so it does not exercise the parser. PR #754 fixed the shape: 14 of the 17 manifests now parse and validate (`crates/ext/tests/first_party_manifests.rs`). `green-loop`, `pr-review` and `welcome-chrome` still do not, because their settings default to a table or a list and `SettingType` has only `string`, `number`, `boolean` and `enum`.

   With the shape fixed, I admitted the real `extensions/buildbuddy` and `extensions/remote-approve` directories through `admit_native_extensions`. Both were admitted with **no DATA grants** (`[]`), because each declared only `env.net.fetch`. The path from there to a denial is: `native.rs:808` sets the host's data grants, `worker.rs:1840` registers them with the authority table, and `forward.rs:2688-2695` maps every `HttpRequest` to `env.net`; the unit test `network_data_authority_requires_host_and_effect_grants` (`crates/envd/src/policy.rs`) shows a host without `env.net` is denied. I read that path and ran the admission test. I did not send a request from the real extension through the gate, so the denial is inferred from those pieces, not observed. PR #756 declares `env.net` next to `env.net.fetch` in both manifests, and `in_tree_extensions_that_call_http_operations_hold_env_net` (`crates/driver/src/discovery/native.rs`) fails if an in-tree extension that calls `omp.env.http_get`, `http_post` or `http_put` lacks it.

   **Corrected after an independent review, 2026-11-27.** The paragraphs above, and PR #756, treat declaring `env.net` as what makes the real `buildbuddy` and `remote-approve` requests succeed. It is only the host half. The DATA gate needs `env.net` in the host's grants **and** in the invocation's effect envelope: `authorize` (`crates/envd/src/policy.rs`) stores `host.grants ∩ envelope`, `validate` denies unless `invocation.envelope.contains(capability)`, and the unit test `network_data_authority_requires_host_and_effect_grants` says the same. An envelope carries `env.net` only from `exec.network` or `fetch` (`Grants::from_effect_envelope`), and a tool that declares no effects on a `sandboxed` host gets documents read plus write only (`undeclared_effects`, `crates/envd/src/worker.rs`). Neither extension declares effects. `buildbuddy` registers its device with `omp.devices.parent("exec", ...)`, which takes only `name`, `family`, `rev` and `place` (`crates/py/python/omp/devices.py:982-989`), so it cannot declare them today; `p9_credential_placement` passes because its fixture tool declares `Effects(fetch=FetchEffects(credentials=True))`, which the real extensions lack. `remote-approve` has no `[[declarations]]` rows and its entry module imports only `decisions`, never `device.py` where `http_get` is, so its approver and hooks are not registered from that entry. I read each of those places. I did not read how a device leaf's maximum effects are derived from `declared_or_undeclared_effects` (`worker.rs`, around the route-effects construction), which is the reviewer's trace, and I did not run either extension through the gate. So by static trace the `buildbuddy` request is still denied after PR #756, and `remote-approve`'s HTTP path is not reached at all.

   **Updated after PRs #758 and #759, 2026-11-27.** The sentence above about `buildbuddy` no longer holds. PR #758 fixed two defects that stopped its registration from sealing: the manifest key for the `exec` parent, and the projection of any dynamic device parent into a worker row (`buildbuddy`, `ci` and `forge` all register one). PR #759 added `effects=` to `omp.devices.parent` and made `buildbuddy`'s `exec` parent declare `Effects(fetch=FetchEffects(credentials=True))`, a fetch effect and not an exec effect, because every leaf is a remote call to the Run API that presents the `buildbuddy` placement credential. The test `dynamic_parent_projection_contract` shows that effect reaches the projected CONTROL row, and fails if the declaration, the projection or the type check is removed. What it does not show is the DATA gate accepting a request: the `exec/*` leaves are not invocable from the client protocol (`p9_delegated_route` pins `NotFound`), so the chain from the row to `env.net` in the envelope (`worker.rs`, `devices.rs`, `policy.rs`) is read, not run. A fetch effect also goes through dynamic admission, which can ask for approval depending on the mode; I did not run that path. `remote-approve` was unchanged by those two. PR #761 then fixed its registration: its entry module now imports `device.py`, so the approver and the two hooks register, and its manifest declares the `extension_activate/observe` and `turn_start/observe` hook rows. `omp.approver(...)` has no `effects` parameter, so the approver's effect envelope is still not declarable and is untraced. The test `remote_approve_registration_contract` seals the real registry in Python; it does not start a worker or run a ticket through the approver.

5. **Consent is exact by name, with two escapes.** A grant request must match the declared set (`validate_grant_request`, `crates/ext/src/trust.rs:446`), but a `"*"` request is accepted (`:451`), and `--yes` consents for sandboxed installs. Operator records in `grants.toml` are read for CONTROL names (`policy.rs:57`) and for credential grants, and only for user-layer roots (`native.rs:819-898`). A workspace or explicit root has no carrier for an operator record. CONTROL is withheld when no kernel sandbox is active (`native.rs:845`). The one grant that carries a scope today is the credential grant of ADR 0075 (origin and operation).

6. **The scope vocabulary one level down is thinner than it looks.** A tool declares an `EffectEnvelope` (`crates/proto/proto/omp/policy/v1/policy.proto:190`). `DocEffects.read` is a bool and `write_globs` a list (`:122`). `ExecEffects` has named commands and a `network` bool (`:128`). `FetchEffects` has only a `credentials` bool (`:153`). So the envelope has exec names and write globs, and has **no read globs and no host list**. `Grants::from_effect_envelope` (`policy.rs:115`) collapses `commands` into the single name `env.exec` (`:132-139`). `Grants` is a set of exact names (`policy.rs:62`, `authorize` at `:1652`) and the DATA gate checks the name only (`crates/envd/src/server/forward.rs:2719`). `pr-review` declares `ExecEffects(commands=("git", "gh"))` (`extensions/pr-review/src/omp_pr_review/device.py:330`) while its manifest asks for plain `env.exec` (`extensions/pr-review/omp.toml:7`). The scope is written and then lost at the extension's grant.

7. **First-party evidence.** Seventeen manifests in `extensions/`; none sets `tier`. When this record was written all seventeen wrote `[requires]` as a table, a shape the native admission path rejects (item 4), so the files were evidence of what the extensions asked for, not of what ran. Since PR #754, 14 of them parse. Declared capabilities: `env.net.fetch` in 7, `env.net` in 2 (`buildbuddy` and `remote-approve`, since PR #756), `env.doc.read` in 3, `env.exec` in 2 (`pr-review`, `green-loop`), `env.blob` in 2, and one each of `env.doc.write`, `env.search`, `sessions.read`; the rest are UI, placement or schedule names. `green-loop` runs a command taken from settings (`extensions/green-loop/src/*/device.py:122-127`), so it has no static ceiling. `pr-review` is the only extension with a scoped effect in source. No in-tree extension needs the whole host. What the evidence supports is two needs: narrow what the mediated names grant, and replace the one coarse implicit widening with an explicit scope.

8. **What an `exec` scope can and cannot mean.** `env.exec` runs in the agent's exec sandbox, outside the worker profile. A program-name scope authorizes `git` and `gh`. It does not confine what `git` or `gh` run on their own.

9. **Network has one path and no allowlist.** The worker has no network, so the host's egress is its only way out. I found no host allowlist in `crates/envd/src/http_egress.rs` (searched for allowlist and domain terms). A `net` host scope is new code.

10. **The design documents describe the middle level, with other names, and the code does not implement it.**
    - `docs/py/14-deploy.md:1583-1598` (§3.9.2) defines scoped capabilities `fs.read`, `fs.write`, `exec`, `net`, `secrets`, with a bare `exec = true` refused. The code's names are `env.exec`, `env.net`, `env.fs.*`, `env.doc.*`.
    - `docs/py/11-env.md:149-158` and `:1444-1456` say an untrusted host runs under an OS profile compiled from the manifest, graded by a `SandboxEnforcement` receipt. That type is the receipt of policy-plane sandbox sessions (`policy.rs:585`, `:698`; `backends.rs:312`). Worker spawn emits none. The same passages say the DATA socket alone does not stop `import os`.

11. **The documents and the code disagree about the sandbox itself.** Owner rulings dated 2026-08-19 say: ship no sandbox enforcement, extensions are not a security boundary, isolation waits for VM-grade work (`docs/py/00-overview.md:1325`, `:1328`; `docs/py/06-policy.md:2626` for macOS). The worker has been spawned under a kernel profile since 2026-08-26, and ADR 0075 D12, accepted 2026-11-25, depends on it. D12 is proved on macOS Seatbelt only.

12. **Built and not wired.** `trusted.direct-filesystem` (`crates/envd/src/exthost/params.rs:24`, `control.rs:191`) is an audited, grant-bound, host-performed file operation on an absolute path. The worker's `direct_filesystem` slot (`worker.rs:294`) is `None` in every constructor (`:1183`, `:1299`, `:3996`), and `EnvdHostOwnerBackends` (`backends.rs:934`) has no caller in `crates`. The Python SDK side exists (`crates/py/python/omp/_host.py:560`, `env.py:557-578`).

13. **The barrier this sits on.** The grant store, the credential store and the reveal command are closed to the worker (ADR 0075 D12) and to the session shell, and no approval prompt is offered for them (PR #748). Any change here keeps that: the operator writes grants, the agent never does.

## Decision (proposed)

**D1. No third named tier.** The middle level is a scope on a capability and an explicit scope where the profile widens, not a new bucket. A named middle tier needs a fixed bundle, and item 7 shows the needs differ per capability. `sandboxed` and `trusted` stay as the two ceilings.

**D2. A capability grant can carry a scope, in the code's names.**
- `env.exec`: program names. No bare unscoped form for a new manifest after the window in O2.
- `env.net`: `host[:port]` entries. This is new vocabulary (items 6 and 9).
- `env.fs.read`, `env.fs.write`, `env.doc.*`: workspace-relative globs. Read globs are new vocabulary (item 6).
- Credential scopes keep the ADR 0075 shape.
- Names with no natural scope (`env.blob`, `env.search`, `env.process`, `env.lsp`, `env.dap.*`) stay plain names, and the consent text says so.

An unscoped capability is shown at consent in a distinct tier ("run any program", "reach any host"), as `docs/py/14-deploy.md:1594` prescribes for `net = true`. `env.net.fetch` is reconciled with `env.net` first (S0, O9).

**D3. The manifest declares the ceiling; the operator narrows and never widens.**
- A recorded scope must be a subset of what the manifest declares. A manifest that declares a capability unscoped can be narrowed, which is how `green-loop`, whose commands are settings, gets a ceiling.
- A manifest change moves the capability digest and re-prompts, as for names. The digest covers scope: today it hashes names only (`crates/ext/src/config.rs:1829`).
- The `"*"` consent and `--yes` are scoped out: a request for a scoped capability must name the scope.
- The agent writes none of it: the `ext/` store stays closed and non-amendable.
- Records exist only for user-layer roots today (item 5). What a workspace or explicit root gets is O8.

**D4. Two enforcement layers, with the claim each one can honestly make.**
- *Environment side.* The DATA gate checks the granted scope per call, so `Grants` and `authorize_data_operation` change. That is a core-type change and S2 sizes it. A `net` scope is enforceable because the worker has no other way out (item 9). An `exec` scope is authorization by program name and nothing stronger (item 8). Consent text says "run `git`", never "confined to `git`".
- *Kernel side.* The baseline worker already has no workspace access, so the kernel work is not "compile a profile". It is to replace the implicit telemetry rule with an explicit `env.fs` scope, so the widening is visible and narrower than the whole root. The worker keeps network disabled. `ProcNoExec` and resource limits are not promised: Seatbelt and Bubblewrap do not have them (item 3).
- *No receipt promised.* Worker spawn emits no `SandboxEnforcement` today (item 10). S4 records the profile that was applied. Whether to build the graded receipt is O3.

**D5. `trusted` stays, and a scoping need is never a reason to grant it.** The install route is gated by a consent that names the tier, and I found no runtime reader of a recorded tier (item 1). S5 settles it: either the recorded tier is wired, or the route is removed. It must not leave a tier that the operator believes they granted and that does nothing.

**D6. Unscoped manifests keep working for a window the owner sets (O2).** They render as unscoped at consent. First-party manifests migrate first.

**D7. `trusted.direct-filesystem` is not the middle level.** It is per-operation host mediation, not a scope on the worker. Wiring or removing it, SDK side included, is O7.

## Alternatives considered

| Alternative | For | Against |
|---|---|---|
| Status quo, two tiers | Nothing to build | `exec` and `net` stay unscoped; telemetry keeps a whole-workspace grant nobody sees |
| A named third tier with a fixed bundle | One more word to learn | The bundle is arbitrary; item 7 shows needs differ per capability |
| A hand-authored sandbox profile per extension (`SandboxProfile`, `docs/py/06-policy.md`) | Maximum power | The operator consents to code, not to a reviewable list |
| VM or container isolation (vibevmm, isobox), deferred by the 2026-08-19 rulings | A real boundary, `import os` included | Not available; this ADR composes with it later |
| Flags on `trusted` (`trusted` plus `allow-net`) | Small change | Still ambient trust, and `trusted` has almost no runtime route (item 1) |

## Consequences

- Consent becomes concrete: "run `git`, `gh`; reach `api.github.com:443`" instead of "may run programs".
- The telemetry widening becomes an explicit, narrower grant.
- `pr-review`'s declared envelope starts to be enforced instead of discarded.
- The manifest schema, the digest and a core type (`Grants`) change. Existing grants for plain names need a read path.
- An `exec` scope is authorization, not containment. A reader must not take this ADR as claiming otherwise.
- Windows is out of reach: no sandboxed worker starts there (item 3). Nothing here fixes that.
- Scoped-profile work leans on `sandbox-exec`, which ADR 0070 treats as a risk (its own D4 has no worker fallback).
- New Seatbelt-only proofs must be added to the macOS objective's e2e list (`scripts/ci-macos-objective.sh`), or they run nowhere: that is how #747 first shipped unproved.
- Proofs must type each command masked and bare against a client that approves every prompt: that is how #748 found the approval hole.

## Implementation sequence

Each slice is a PR with its own proof. None starts before O1 is answered.

- **S0. Reconcile.** Mark the 2026-08-19 rulings superseded for the extension worker. Align the document names with the code's. The manifest shape is fixed (PR #754). PR #756 declared `env.net` in `buildbuddy` and `remote-approve`, which is the host half of what the DATA gate needs, and keeps `env.net.fetch` because the conformance rule requires it of any network package; the effect half is open (item 4, corrected). Three things remain: the three manifests that still do not parse (item 4), which needs a decision on `SettingType` or on those extensions' settings; the effect half of the network grant for `remote-approve`'s approver, which still has no way to declare it (`omp.approver` has no `effects` parameter; `buildbuddy`'s device declares it since PR #759, untested through the gate; `remote-approve` registers its approver and hooks since PR #761); and a trace of what the gate does for a host started by `--trusted-extension`. It is independent of the rest of this record and goes first.
- **S1. Data model.** Scoped capabilities in the manifest and the digest, consent display, the `"*"` and `--yes` rule, and a read path for existing plain-name grants. First-party manifests migrated. Proof: the digest moves with a scope; unscoped renders distinctly; a scoped request without a scope is refused.
- **S2. Environment-side enforcement,** `net` host scope first, then `exec` names. Proof for `net`: a fetch to an undeclared host is refused and one to a declared host passes. Proof for `exec`: direct invocation only, stated as such.
- **S3. Operator narrowing.** `omp ext grant <id> --scope ...`, through the writer ADR 0075 added. Proof: widening is refused; the session shell and the worker cannot write the store (reuse the D12 proofs).
- **S4. Kernel side.** Replace the telemetry rule with an explicit `env.fs` scope, and record the applied profile. Proof, in the macOS objective: a worker granted `env.fs.read = ["docs/**"]` reads inside and not outside, plus a mutation check.
- **S5. Close the loose ends.** The direct-filesystem machinery (host and SDK), the recorded-but-unread `trusted` tier, non-user roots (O8), and the end of the unscoped window.

## Open questions for the owner

- **O1. Do the 2026-08-19 rulings stand for extension workers?** The code ships a kernel profile and ADR 0075 D12 depends on it. Recommendation: superseded for the worker, and the documents say so (S0). Nothing else here can be accepted without an answer.
- **O2. Unscoped capabilities.** Keep, deprecate after a window, or refuse an unscoped `env.exec` and `env.net` in new manifests. Recommendation: deprecate. The length of the window is yours: I did not find a release-versioning scheme that would make "one minor version" meaningful.
- **O3. A graded receipt for workers.** Build one, or keep the current rule that a host with no backend refuses the worker. Recommendation: keep refusing, record the applied profile (S4), and defer the graded receipt.
- **O4. Who may add scope.** Narrowing only, including narrowing a manifest that declares a capability unscoped. Recommendation: narrowing only.
- **O5. First slice for enforcement.** `net` or `exec`. Recommendation: `net`. The worker has no network, so the host's egress is the only path and the scope is enforceable there. An `exec` scope only authorizes a program name (item 8). Two manifests declare `env.exec`. Two declare `env.net`, `buildbuddy` and `remote-approve`, since PR #756, and they are the two that call an HTTP operation.
- **O6. Is there a real extension that needs `trusted`?** None in the tree does, and I found no runtime reader of a recorded tier. Recommendation: do not design for `trusted` until one is shown.
- **O7. `trusted.direct-filesystem`.** Wire it with a scope, or delete it including the SDK side (`_host.py:560`, `env.py:557-578`). Recommendation: delete unless O6 produces a case.
- **O8. Workspace and explicit roots.** They have no carrier for an operator record. Keep admitting their declared names unscoped as today and record the gap, or refuse an unscoped `env.exec` and `env.net` from them. Recommendation: keep today's behavior for now, and decide with O2.
- **O9. `env.net.fetch` against `env.net`.** Which spelling stands once host grants align (the conformance script says "until host grants align on `env.net` alone"). Recommendation: keep both in manifests for now, as the conformance rule requires, and drop `env.net.fetch` only when the host reads it for URL schemes under `env.net`. Do not alias it in `Grants::supported`: a typo that grants nothing is the failure this record found, and an alias hides the next one.

## Corrections from review

The first draft of this record said, and was wrong to say:
- that nothing in the worker profile depends on the manifest (the telemetry row does);
- that seven of sixteen extensions need network, counting `env.net.fetch`, which is not a DATA capability, and counting sixteen manifests when there are seventeen;
- that the backends could express `ProcNoExec` and resource limits for the worker (Seatbelt and Bubblewrap cannot, and Windows cannot start a worker);
- that `trusted` is reached by install at runtime (only the hidden flag builds the key);
- that the envelope carries a scope vocabulary (it has no read globs and no host list), and that the receipt exists for workers (it does not).

It also recommended `exec` before `net`. The evidence reverses that order (O5).

After the record merged, a second correction. It said the in-tree `buildbuddy` and `remote-approve` were denied today because they declare only `env.net.fetch`, and the status table said the request was denied. What was executed is narrower: a native-format manifest declaring only `env.net.fetch` gets no grant. The real manifests are not admitted by that route (their `[requires]` table is rejected), and I had not run them. The text above now says so. The question that left open was then answered; see the "Resolved" paragraph in item 4.

A third correction came from an independent review the same day. This record, and PR #756, treated `env.net` in the manifest as what makes the real `buildbuddy` and `remote-approve` requests succeed. It is the host half only; see the "Corrected after an independent review" paragraph in item 4. The first draft of that PR's own description also pointed at a different PR, and the PR landed by a direct push to `omp2` that bypassed the ruleset instead of a merge commit.

A fourth update came with PRs #758 and #759: `buildbuddy` can now declare effects, and its registration seals. The claim in item 4 that it cannot declare them was true when written and is superseded; see the "Updated after PRs #758 and #759" paragraph there.

## Not verified

- That a request from the real `buildbuddy` or `remote-approve` reaches the DATA gate and is denied without `env.net`. I read the path and ran the admission test, and I did not send the request, so the denial is inferred. The probe in item 4 used a native-format fixture manifest and a test fixture's tool.
- That the real `buildbuddy` and `remote-approve` requests are accepted once both halves are declared. `buildbuddy` declares both since PR #759, `remote-approve` only the host half. Nothing has run either through the gate; the traces in item 4 are static.
- Which path runs `remote-approve`'s approver and hooks. Since PR #761 they register and seal, but nothing has started a worker, offered a ticket or run a hook through the host, and the new hooks now run on load (a session schedule and a per-turn poll). Other loaders were not checked, and the reviewer's report that hook dispatch builds its DATA authority with `data: None` was seen at some sites, not all.
- What the gate does for a host started by the hidden `--trusted-extension` flag, which assigns no DATA grants (`crates/app/src/cli.rs:3851-3894`). I did not trace it.
- The three manifests that still do not parse (`green-loop`, `pr-review`, `welcome-chrome`; item 4).
- The Linux worker profile end to end. Only macOS Seatbelt is exercised.
- What third-party extensions need. There is no data; the evidence is the seventeen in-tree manifests.
- How `HttpEgressHost` constrains the hosts of an extension's non-credential requests beyond the credential grant of ADR 0075. `env.net` gates the request (`forward.rs`), and I did not map the egress policy behind it.
- Which approvals and which sandbox apply to a command an extension runs through `env.exec` (ADR 0028).
- Whether the `"*"` request path and `--yes` are reachable in the shipped CLI for a sandboxed install. I read the function, not a run.

## References

- ADR 0075, extension credential host placement (D12, S2).
- ADR 0028, effect tiers and the in-process interpreter.
- ADR 0070, platform sandbox CI. It is itself proposed and not owner-approved; it is cited only for the no-fallback rule on the worker.
- PRs #747 and #748, the shell-side proofs and the non-amendable protected locations.
- `docs/py/14-deploy.md` §3.9, `docs/py/11-env.md`, `docs/py/06-policy.md`, `docs/py/00-overview.md`.
