# OMP2 gateway-capabilities research: index

> Research, comparison and architectural proposal only. Nothing in this set is implemented, no existing ADR is edited, and nothing was pushed. Audited OMP2 commit: `85d219ae7f` (`origin/omp2`, 2026-11-27). Branch `docs/research-gateway-capabilities`.

Question (owner brief, section 1): which gateway capabilities would give measurable architectural, operational, security, performance or resource-efficiency benefit if implemented inside OMP2's existing native Rust harness, without turning OMP2 into another monolithic AI gateway. Governing principle (section 18): OMP2 stays a lightweight, modular native Rust agent harness; gateway capabilities strengthen the existing provider runtime and do not create a second application architecture.

## Deliverables (owner brief, section 16)

| # | Deliverable | File | Status |
|---|---|---|---|
| 1 | Executive architectural assessment | [01-executive-architectural-assessment.md](01-executive-architectural-assessment.md) | TODO |
| 2 | Verified current-state architecture | [02-verified-current-state.md](02-verified-current-state.md) | DONE |
| 3 | Cross-gateway capability matrix | [03-gateway-capability-matrix.md](03-gateway-capability-matrix.md) | PARTIAL (first pass, see the file's status note) |
| 4 | OMP2 existing-versus-missing capability matrix | [04-existing-vs-missing-matrix.md](04-existing-vs-missing-matrix.md) | PARTIAL (first pass, see the file's status note) |
| 5 | Provider integration recommendations | [05-provider-integration-recommendations.md](05-provider-integration-recommendations.md) | DONE |
| 6 | Transport and protocol recommendations | [06-transport-and-protocol-recommendations.md](06-transport-and-protocol-recommendations.md) | PARTIAL (first pass, see the file's status note) |
| 7 | ABI-independent extension assessment | [07-abi-independent-extension-assessment.md](07-abi-independent-extension-assessment.md) | DONE |
| 8 | Python runtime comparative assessment | [08-python-runtime-comparative-assessment.md](08-python-runtime-comparative-assessment.md) | DONE |
| 9 | Single-session swarm assessment | [09-single-session-swarm-assessment.md](09-single-session-swarm-assessment.md) | DONE |
| 10 | Local inference assessment | [10-local-inference-assessment.md](10-local-inference-assessment.md) | DONE |
| 11 | Shared code intelligence and SCIP assessment | [11-code-intelligence-and-scip-assessment.md](11-code-intelligence-and-scip-assessment.md) | TODO |
| 12 | Proposed architecture diagrams | [12-proposed-architecture-diagrams.md](12-proposed-architecture-diagrams.md) | TODO |
| 13 | Performance and benchmark plan | [13-performance-and-benchmark-plan.md](13-performance-and-benchmark-plan.md) | DONE |
| 14 | Security and licensing assessment | [14-security-and-licensing-assessment.md](14-security-and-licensing-assessment.md) | DONE |
| 15 | Prioritized architectural backlog | [15-prioritized-architectural-backlog.md](15-prioritized-architectural-backlog.md) | DONE |
| 16 | Draft ADRs (only where justified) | [16-draft-adrs.md](16-draft-adrs.md) | TODO |
| 17 | Incremental implementation roadmap | [17-incremental-implementation-roadmap.md](17-incremental-implementation-roadmap.md) | DONE |
| 18 | Explicitly rejected alternatives and reasons | [18-rejected-alternatives.md](18-rejected-alternatives.md) | DONE |
| 19 | Source references with pinned commits and paths | [19-source-references.md](19-source-references.md) | DONE |
| 20 | Open questions requiring architectural approval | [20-open-questions.md](20-open-questions.md) | DONE |

Companion files linked from the deliverables: `05b-authentication-and-credential-lifecycle.md` (part of 5), `06b-ipc-and-serialization.md` (Part B of 6), `adr-drafts/` (draft ADRs, deliverable 16), `diagrams/` (the 19 diagrams of owner section 14, indexed in deliverable 12).

## Supporting material

- `evidence/00-owner-brief-verbatim.md`: the owner's request, unchanged.
- `evidence/01-method-and-dimension-ids.md`: evidence rules, maturity labels, the nine decision classes (owner section 17), capability-card template, dimension IDs used by the matrices.
- `evidence/ref-inventory.md`: the 39 reference repositories with pinned SHAs, licences and availability (37 cloned; 2 unavailable).
- `evidence/omp2-*.md` and `evidence/omp2-*.tsv`: per-area audits of OMP2 with path, line and symbol citations.
- `evidence/ref-*.md` and `evidence/matrix-*.tsv`: per-repository reading notes and matrix rows with `repo@sha7:path` citations.
- `evidence/measurements/`: every measurement with its command, environment and raw output. Anything not listed there was not measured.
- `evidence/check_docs.py`: mechanical checks (links, back-links, decision classes, citation existence). It checks form, not truth.

## Decision classes (owner section 17)

Every proposed capability carries exactly one: Already implemented: reuse. Partially implemented: extend. Missing and justified: propose. Suitable as an optional extension. Suitable as an external integration. Requires benchmarking. Requires ADR reconsideration. Unnecessary or redundant. Rejected for security, licensing or operational reasons.

## Reading order

Start at deliverable 1 for the conclusions, then 2 for what OMP2 verifiably is today. Deliverables 3 and 4 are the matrices; 5 to 11 are the per-area assessments; 12 to 20 are diagrams, plan, risks and decisions needed.
