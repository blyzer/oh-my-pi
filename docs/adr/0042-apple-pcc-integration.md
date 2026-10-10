# 0042. Apple PCC extends the existing Apple Foundation Models boundary

Status: proposed — blocked on Apple PCC entitlement and signed-host proof
Date: 2026-10-10
Area: inference/provider

## Decision proposed

If Apple confirms that an independently distributed OMP host can obtain and use the managed PCC entitlement, extend the existing `omp-ai::local::applefm` integration with an explicit PCC backend. Do not introduce an OpenAI-compatible gateway or a second agent loop.

```text
Apple catalog
  on-device route ─┐
                   ├─ omp-ai::local::applefm backend boundary
PCC route ─────────┘
```

The existing `apple-intelligence` on-device identifier and behavior remain unchanged. PCC selection is explicit; no silent PCC-to-local fallback is allowed.

## Evidence

- `crates/ai/src/local/applefm.rs` dynamically loads FoundationModels.framework and already owns availability, streaming, cancellation, timeout, concurrency and typed Apple errors.
- Apple documents `PrivateCloudComputeLanguageModel` as another `LanguageModel` consumed by `LanguageModelSession`.
- Apple documents PCC-specific availability, quota, context size, locale and error APIs.
- `apple-code` has an `apple-pcc` configuration lane but currently returns an unavailable-client explanation; it is not execution proof.
- `PrivateFoundationModels` and `FoundationModelsAgent` demonstrate reusable LanguageModel-shaped abstractions but do not establish OMP PCC access.
- The supplied `keejkre/maclab` repository URL was unavailable (404).

## Alternatives rejected for now

- Dedicated PCC provider: duplicates Apple lifecycle/availability and is less compatible with the existing boundary.
- Swift HTTP gateway: adds a process, serialization, security and deployment boundary without evidence that it is required.
- Sharing v1/v2 implementation code: v1 source was not located in the maintained OMP trees; share behavior/contracts first, not unverified ABI code.

## Hard blocker

Apple's current access page requires App Store Small Business Program eligibility, fewer than 2 million first-time App Store downloads, and a managed PCC entitlement assigned to the developer account. It describes App Store, TestFlight and ad hoc distribution. Direct CLI eligibility is not established.

## Required gates

1. Obtain/confirm PCC entitlement access.
2. Build a signed macOS proof host.
3. Verify availability, one response, streaming, cancellation, multi-turn context, quota exhaustion and entitlement failure.
4. Verify whether a directly distributed CLI is eligible.
5. Only then implement the dynamic PCC ABI and catalog route.

## Backlog dependency

```text
PCC access request
  -> signed host proof
  -> CLI/distribution eligibility decision
  -> applefm PCC ABI extension
  -> catalog/capability/error/quota integration
  -> macOS integration tests
  -> documentation
```

PCC implementation has not started.
