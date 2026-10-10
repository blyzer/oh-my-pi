# Apple PCC next steps

Status: blocked on external Apple entitlement/access proof. PCC implementation is not started.
Source report: `docs/research/apple-pcc-integration.md`
Proposed ADR: `docs/adr/0042-apple-pcc-integration.md`

GitHub Issues are disabled for `blyzer/oh-my-pi`; these tasks use the repository's parked-work handoff convention.

## Dependencies

```text
PCC-1 access request
  -> PCC-2 signed host proof
  -> PCC-3 CLI/distribution eligibility decision
  -> PCC-4 applefm PCC ABI extension
       -> PCC-5 catalog/capability/quota/error integration
            -> PCC-6 macOS integration tests
                 -> PCC-7 documentation / release gate
```

## Tasks

### PCC-1 — Obtain managed PCC access

- **Objective:** Verify whether the project account can receive Apple's managed PCC entitlement.
- **Scope:** App Store Small Business Program status, first-time download threshold, capability request, account assignment.
- **Owner:** project owner / Apple Developer account.
- **Acceptance:** entitlement assignment or a documented denial/blocker with Apple's response.
- **Blocker:** external Apple account access; no code should be written before this is known.

### PCC-2 — Signed macOS PCC proof host

- **Objective:** Prove the current FoundationModels SDK can construct `PrivateCloudComputeLanguageModel` in a signed macOS host.
- **Scope:** minimal host only; availability, reason, quota usage, context size, one text response, streaming, cancellation.
- **Target:** separate scratch proof host; do not add it to the OMP runtime yet.
- **Acceptance:** reproducible signed run log and entitlement/device/OS matrix.
- **Depends on:** PCC-1.

### PCC-3 — CLI and distribution eligibility

- **Objective:** Establish whether an independently distributed OMP CLI can invoke PCC.
- **Scope:** compare ad hoc/TestFlight/App Store signed host behavior with direct CLI behavior; record whether iCloud login is required or whether entitlement/account state is sufficient.
- **Acceptance:** explicit allowed/blocked result; no inference from on-device availability.
- **Depends on:** PCC-2.

### PCC-4 — Extend `omp-ai::local::applefm` backend

- **Objective:** Add PCC as an explicit backend beside `SystemLanguageModel` without changing on-device behavior.
- **Scope:** dynamic symbols, backend selection, availability evidence, context size, locale, quota and typed errors; preserve Linux stub.
- **Acceptance:** macOS build and unit ABI tests; Linux build remains framework-free.
- **Depends on:** PCC-3.

### PCC-5 — Catalog and runtime semantics

- **Objective:** Register an explicit PCC route and map capabilities conservatively.
- **Scope:** provider/route identity, no silent fallback, quota exhaustion, cancellation, retry policy, receipts and observability.
- **Acceptance:** provider selection and failure states are explicit in catalog/runtime tests.
- **Depends on:** PCC-4.

### PCC-6 — Streaming/tool/structured-output validation

- **Objective:** verify capabilities instead of inheriting on-device assumptions.
- **Scope:** streaming, multi-turn continuity, tool conformance, guided generation, cancellation and concurrent request behavior.
- **Acceptance:** each capability is classified supported/restricted/unsupported/unknown with a macOS test or documented Apple limitation.
- **Depends on:** PCC-4 and PCC-5.

### PCC-7 — Documentation and release gate

- **Objective:** document setup, entitlements, supported OS/device/region, quota, privacy/provider switching and troubleshooting.
- **Acceptance:** docs list the external blocker and PCC implementation status; release checklist refuses PCC claims without entitlement proof.
- **Depends on:** PCC-1 through PCC-6.

## Explicit non-goals

- No OpenAI-compatible HTTP gateway unless PCC-3 proves direct invocation impossible and a signed companion host is justified.
- No iCloud credential flow unless Apple's documented PCC path requires it.
- No change to existing on-device Apple behavior.
- No PCC implementation in OMP before PCC-3 closes.
