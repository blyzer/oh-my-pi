# Apple PCC Integration Research — OMP v1.x and OMP²

Status: research complete enough for a proposed architecture; PCC implementation **not started**.
Date: 2026-10-10

## Executive conclusion

Apple PCC is technically aligned with OMP²'s existing Apple Foundation Models boundary, not a new HTTP provider ecosystem. Apple documents `PrivateCloudComputeLanguageModel` as another `LanguageModel` consumed by the same `LanguageModelSession` API. The smallest technical extension is therefore a second Apple backend behind `omp-ai::local::applefm`, sharing request/session/streaming/error machinery while selecting on-device or PCC explicitly.

A hard feasibility blocker remains: Apple requires a managed PCC entitlement assigned to an eligible Apple Developer account and describes use through App Store, TestFlight, or ad hoc distributed apps. A directly distributed OMP CLI may not be eligible. This must be proven with a signed macOS sample before product work.

Recommendation: extend the existing Apple provider boundary (Alternative B), with explicit `on-device` and `pcc` backend modes. Do not silently fall back between them. Do not add a Swift HTTP gateway unless the signed-process proof shows a direct CLI cannot consume PCC.

## OMP² baseline

Source: `crates/ai/src/local/applefm.rs` at OMP² `dec59c1287567e788f4fc331e381b62ccaa0c9d0` / current `omp2` lineage.

Execution path:

```text
catalog/provider route
  -> AppleFmTransport
  -> AppleFm::availability / stream
  -> dynamic FoundationModels.framework ABI
  -> SystemLanguageModel.default
  -> LanguageModelSession
  -> RawEvent / ChatEvent stream
```

Observed implementation facts:

- `crates/ai/src/local/applefm.rs` is the provider transport and codec seam.
- macOS loads `/System/Library/Frameworks/FoundationModels.framework/FoundationModels` dynamically.
- Non-macOS builds compile a typed unavailable stub; Linux does not link Apple frameworks.
- The native ABI resolves `SystemLanguageModel`, `LanguageModelSession`, generation options, availability and guardrail symbols.
- Availability distinguishes OS, architecture, framework, Apple Intelligence settings, model readiness and runtime failure.
- Streaming is incremental and cancellable through `AppleFmStream` and `CancellationToken`.
- A 30-second framework timeout is enforced.
- `AdmissionControl::new(1)` limits process-local concurrency to one request.
- Catalog discovery emits provider/route data with wire model `apple-intelligence`, chat-only operations, context limit 4096, and runtime availability evidence.
- Current request lowering rejects tools/tool results, reasoning settings, structured output and non-text content where the dynamic ABI cannot prove support.
- Tool support is explicitly classified as `RequiresCompiledSwiftToolConformance`; structured generation is `DynamicSchemaAbiUnverified`.
- Errors are typed into capability, cancellation, timeout, unavailable, context, guardrail, locale, rate-limit and runtime categories.

The checked OMP² tree contains no separate Swift gateway or HTTP Apple provider. No distinct OMP v1.x Apple implementation was found in the available repository trees; the v1.x side therefore remains an evidence gap rather than an assumed architecture.

## PCC API findings

Official sources:

- [PrivateCloudComputeLanguageModel](https://developer.apple.com/documentation/foundationmodels/privatecloudcomputelanguagemodel)
- [Accessing Private Cloud Compute](https://developer.apple.com/private-cloud-compute/)
- [LanguageModelSession](https://developer.apple.com/documentation/foundationmodels/languagemodelsession)

Apple documents:

- `PrivateCloudComputeLanguageModel()` as a `LanguageModel` implementation.
- The same `LanguageModelSession(model:)` construction used by Foundation Models.
- `availability`, `isAvailable`, `quotaUsage`, `contextSize`, `supportedLanguages` and `supportsLocale`.
- A PCC-specific error enum.
- Availability on iOS/iPadOS/macOS/Mac Catalyst/visionOS/watchOS 27.0+ in the current documentation.
- PCC access for eligible App Store Small Business Program developers with fewer than 2 million first-time App Store downloads and a managed entitlement assigned to the developer account.
- Eligible apps can use PCC where Apple Intelligence is available and can test through TestFlight or ad hoc distribution.

Capability matrix:

| Capability | Evidence | Classification |
|---|---|---|
| Text generation | `LanguageModelSession.respond` example | Officially supported |
| Streaming | Inherited Foundation Models session API; requires signed SDK proof | Supported by shared API, runtime proof required |
| Multi-turn session | Shared `LanguageModelSession` | Official API shape; runtime proof required |
| Tools | Shared `LanguageModel` protocol, but OMP's dynamic Rust ABI cannot synthesize arbitrary Swift `Tool` conformances | Unknown for OMP; do not assume |
| Structured output | PCC conforms to `LanguageModel`, but OMP dynamic ABI has no verified schema witness support | Unknown for OMP |
| Context | `contextSize` | Officially supported |
| Quota | `quotaUsage` | Officially supported |
| Locale | `supportedLanguages` / `supportsLocale` | Officially supported |
| Cancellation | Session async APIs; OMP already has cancellation boundary | Needs signed runtime proof |
| Concurrent requests | No safe OMP assumption; current Apple provider admits one | Unknown; preserve one until measured |
| Session persistence | `LanguageModelSession` retains native transcript; durable cross-process persistence not established | Unknown |
| Usage accounting | PCC exposes quota usage, but mapping to OMP receipts is unverified | Unknown |

The documentation does **not** establish that direct iCloud login is the PCC authentication mechanism. The visible requirements are developer-account entitlement, distribution eligibility, device/region availability and Apple Intelligence state. A signed CLI eligibility proof is mandatory.

## External repository assessment

| Project | Commit | Finding | Recommendation |
|---|---|---|---|
| `keejkre/maclab` | unavailable; GitHub URL returned 404 | Could not verify repository or implementation | Treat as unavailable evidence; do not rely on it |
| `dkyazzentwatwa/apple-code` | `29ef774dc44bbac977d1f3c0bd3b0322ed977191` | Has provider/config entries for `apple-pcc`, reasoning levels and status/fallback UX, but `ModelClient` currently returns `UnavailableModelClient` saying PCC requires an SDK with `PrivateCloudComputeLanguageModel` and account/device access | Useful for explicit provider UX; not proof of PCC execution |
| `john-rocky/PrivateFoundationModels` | `29e0939d21ebc11535f7a0b985c6211af5524299` | Provides a Swift-shaped `LanguageModel` abstraction and an OpenAI-compatible optional server, but its documented Apple backend is on-device Foundation Models; PCC support is not established | Useful abstraction/testing patterns only; gateway is not required for OMP |
| `rudrankriyam/FoundationModelsAgent` | `4bc5676bb25b78d691cbcca4b8c2f98d430eaba7` | Accepts any native `LanguageModel`, adds policy, budgets, checkpoints and observability; no need to replace OMP's agent loop | Reuse conceptual evidence/policy patterns, not the agent architecture |

The requested `keejkre/maclab` repository could not be fetched at the supplied URL, so its PCC status is unresolved.

## Alternatives

| Alternative | Score /10 | Decision |
|---|---:|---|
| A. Extend existing Apple provider directly | 8.8 | Preferred boundary if PCC symbols can be loaded safely |
| B. Extend native Apple integration layer | 9.2 | Recommended implementation shape; shares ABI/session/error machinery |
| C. Dedicated PCC provider | 6.4 | Avoids some branching but duplicates Apple-specific lifecycle and availability |
| D. Native Swift adapter/IPC | 5.8 | Only justified if direct dynamic ABI cannot satisfy managed entitlement/signing |
| E. OpenAI-compatible gateway | 3.9 | Adds process, serialization, security and lifecycle cost; no technical need shown |
| F. Shared v1/v2 native component | 4.7 | v1 implementation is not present/evidenced; independent release/runtime constraints make code sharing premature |
| G. PCC through a future provider-neutral native backend | 7.5 | Viable if Apple changes ABI, but currently less direct than extending `applefm` |

Hard blocker overrides scores: if an independently distributed CLI cannot obtain the managed entitlement, direct OMP CLI PCC support must not be implemented as if it were available.

## Recommended design

```text
omp-catalog
  apple provider
    apple-intelligence / on-device route
    apple-pcc / PCC route (availability-gated)
          |
omp-ai::local::applefm
  AppleBackend::{OnDevice, PrivateCloud}
          |
  shared LanguageModelSession ABI, stream, cancellation, timeout,
  typed errors, quota/context evidence
```

- Preserve the existing `apple-intelligence` identifier and on-device behavior.
- Add PCC as an explicit route/provider identity only after entitlement proof.
- Never silently fall back PCC → on-device; privacy/capability/quota semantics differ.
- Keep Linux builds unchanged with a non-macOS unavailable stub.
- Extend availability evidence with PCC quota/context/eligibility state.
- Keep current one-request admission limit until concurrency is measured.
- Keep tool/structured-output capabilities disabled until compiled Swift conformance or ABI evidence exists.
- Map `quotaUsage` to typed OMP receipts only after observing its fields and reset semantics.

## Required proof of concept before implementation

1. Build a signed minimal macOS app/CLI host with the managed PCC entitlement.
2. Verify PCC availability and print its typed reason/quota/context evidence.
3. Run one text response.
4. Verify streaming and cancellation.
5. Verify multi-turn context.
6. Verify quota exhaustion and entitlement failure messages.
7. Test tools and structured output separately; absence is an explicit capability result.
8. Confirm whether an independently distributed CLI can run, or whether a signed companion app/host is required.

## Backlog tasks created

GitHub Issues are disabled for this repository, so the existing parked-work
convention is the task system. Actionable tasks and dependencies were written
to [`docs/parked/2026-10-10-apple-pcc-next-steps.md`](../parked/2026-10-10-apple-pcc-next-steps.md):

- `PCC-1`: request/verify the managed entitlement;
- `PCC-2`: signed macOS PCC proof host;
- `PCC-3`: CLI/distribution eligibility decision;
- `PCC-4`: extend `applefm` with a PCC backend;
- `PCC-5`: catalog/quota/error/runtime semantics;
- `PCC-6`: streaming/tool/structured-output validation;
- `PCC-7`: documentation and release gate.

`PCC-1` through `PCC-3` are external blockers. PCC implementation remains
unscheduled until `PCC-3` closes.

OMP v1.x remains an evidence gap: locate the production Apple provider source
or explicitly record that v1.x has no maintained Apple integration in this
repository.

## Current status

- Research: complete enough for a proposed architecture; external eligibility
  proof remains open.
- Architecture recommendation: proposed, pending signed-host/entitlement proof.
- Backlog update: completed in `docs/parked/2026-10-10-apple-pcc-next-steps.md`.
- PCC implementation started: **NO**.

