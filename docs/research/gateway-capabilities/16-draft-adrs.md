# Draft ADRs (only where justified)

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 16. Audited commit `85d219ae7f`. Research proposal: nothing here is implemented, no ADR is approved, and no existing ADR was edited.

## 1. Rule applied

A draft ADR is written only when the evidence shows a decision that (a) crosses more than one crate or layer, or changes behaviour an existing ADR records as Implemented, and (b) cannot be settled by a code fix that follows an existing seam. Everything else is a card in a deliverable, not an ADR. The drafts follow the format of the existing records (`Status`, `Date`, `Area`, `Read against`, `Context`, `Decision`, `Consequences`, `Alternatives`, `Acceptance`, `References`), carry the status "proposed (draft ... not owner-reviewed, not approved)", and use the next free numbers on the audited commit (0077 to 0080, provisional: another record may claim them). They live in `docs/adr/` as the brief asked; they are not in `docs/adr/STATUS.md`, which this research did not edit.

## 2. Drafts written

The four draft files are submitted in a separate pull request after this research is merged, because each draft links back to these documents. The table below names the files they will occupy.

| Draft | Title | Cards | Would amend | Gate |
|---|---|---|---|---|
| 0077 (draft file docs/adr/0077-route-runtime-evidence-producers.md, proposed separately) | Route runtime evidence gets production producers; a circuit breaker is a health state | PRV-01, PRV-02, PRV-03 | ADR 0018 (records the provider runtime as Implemented while its evidence inputs are inert) | OQ-02, OQ-11; breaker only after its benchmark |
| 0078 (draft file docs/adr/0078-credential-refresh-owner-and-shared-auth-stack.md, proposed separately) | Credential refresh has a per-process owner, and in-process subagents share the parent auth stack | AUTH-01, AUTH-03, AUTH-07, AUTH-11 | ADR 0018 (credentials amendment), touches ADR 0075 | OQ-03 |
| 0079 (draft file docs/adr/0079-shared-code-index-and-query-result.md, proposed separately) | A shared code index is keyed by workspace identity and revision, and queries return one typed result | CIX-02, CIX-03, CIX-05, CIX-10, CIX-11, CIX-12 | ADR 0007 and ADR 0049 are read, not changed; ADR 0060 context | OQ-17 |
| 0080 (draft file docs/adr/0080-local-inference-placement-and-shared-residency.md, proposed separately) | Local inference placement and shared residency: out-of-process C and C++ runtimes, one memory budget per process | INF-01, INF-02, INF-03, INF-04, INF-05, INF-07, INF-10 | ADR 0023 and ADR 0042 read, not changed; interacts with the locked "candle only" rule | OQ-07, OQ-08, OQ-09 |

Each draft ends with the owner questions it needs answered. None proposes an implementation date.

## 3. Where a draft was considered and not written

| Area | Decision | Reason |
|---|---|---|
| Python runtime (deliverable 8) | none | no evidence supports changing ADR 0036; two small points (the opt-in external Eval interpreter and the `omp setup python` CLI boot are absent from ADR 0036 text, PY-10) need the owner's choice between a status edit and an amendment. A superseding ADR becomes appropriate only if PY-02, PY-03 and the PY-05 evidence show a gap |
| Extensions (deliverable 7) | none | EXT-01 to EXT-09 are code or documentation fixes under existing ADRs; ADR 0076 already proposes the surrounding permission design. EXT-10 (Wasm components) and EXT-11 (native executable extensions) need the owner to open ADR 0036 decision 2 first (OQ-06); a draft before that decision would be speculative |
| Swarm (deliverable 9) | none | no new orchestration layer is justified; two edits to existing records are suggested for owner approval: correct the ADR 0051 row in `docs/adr/STATUS.md` once production journaling is verified, and add resource inputs to the `WorkpoolPolicy` section of ADR 0039 only if the measurements show pressure failures. A new ADR becomes justified only if SWM-07 passes its benchmark |
| IPC (deliverable 6b) | none now | IPC-05 would amend ADR 0048 (the zero-copy rejection lacks evidence) only after a benchmark; IPC-04 needs a consumer for the IR schemas first |
| Authentication, other points (deliverable 5b) | not drafted | AUTH-02 (`omp.creds.*`), AUTH-06 (CLIProxyAPI import) and AUTH-10 (reading another application's stored token) each need an owner decision on provider terms or ADR 0049 D4 before any text is useful (OQ-04, OQ-05) |
| Provider, other points (deliverables 5 and 6) | none | PRV-13 (OpenAI-compatible facade) waits on OQ-10; ADR 0074 versus the locked "no vendor server-side tools" rule is an owner decision (OQ-01), not a design to draft |

Back to the [index](README.md).
