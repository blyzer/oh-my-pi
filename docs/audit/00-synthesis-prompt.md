# Synthesis prompt: the ADW / OMP local-first architectural audit

You are being handed this file cold. Everything you need is in it. There is no
prior conversation to recover.

---

## 1. Your job

Five auditors produced evidence files in this directory:

| File | Scope |
| --- | --- |
| `01-outbound-path.md` | Everything that crosses the wire to a model provider |
| `02-tool-results.md` | How tool output is produced, bounded and delivered into context |
| `03-secrets-privacy.md` | Secret/PII detection, redaction, obfuscation, egress control |
| `04-context-reduction.md` | Compaction, context selection, summarisation, token economics |
| `05-extension-points.md` | Extensions, hooks, custom tools, MCP, subagents, ADW seats |

Read every one that exists. **Some may be absent — that is expected.** An
absent file is a stated coverage gap in your report, never a reason to guess or
to postpone. Write the report from what exists and name what did not.

Produce **one** file: `docs/audit/00-architecture-audit.md`. Nothing else.

You are writing a single architectural audit that answers one question with
evidence: *how local-first is the ADW/OMP system in reality, and what is
actually missing?*

---

## 2. What ADW is (orientation, not evidence)

ADW — AI Developer Workflow — is an ordered list of phases declared in
`.omp/adw/<name>.yml` and executed by `/adw <workflow> <request>`. Deterministic
code decides what runs next and whether it counts; models only do the work
inside one bounded phase. A Rust state machine (`crates/pi-tasks/`) answers
*what runs next* and *did that count*; only the TypeScript layer
(`packages/coding-agent/src/adw/`) can reach a model.

Read for context, in this order:

- `docs/adw-workflows.md` — what ADW is and its full key surface.
- `docs/adw/examples/*.yml` — `fix.yml`, `ship.yml`, `review.yml` — what a
  workflow actually looks like.
- `docs/adw/safety-guard-evaluation.md` — **match this document's tone and
  evidentiary discipline.** Note how it states a conclusion up front, marks
  every claim `OBSERVED`, tabulates authority and failure modes, keeps an
  evidence index at the end, and is unembarrassed to conclude that a widely
  assumed capability does not exist.

These documents are orientation. **They are not evidence for your findings.**
See rule R1.

---

## 3. Non-negotiable rules

**R1 — Documentation is not evidence.** Only runtime implementation counts. A
doc, comment, README, settings description or design note that claims a
behaviour the code does not implement is **itself a finding** and must be
reported as one, naming the doc and the code that contradicts it.

**R2 — Keep LOCAL EXECUTION and LOCAL CONTEXT REDUCTION separate.** These are
different axes and conflating them is the single failure mode this audit exists
to prevent.

- LOCAL EXECUTION: `grep` runs on this machine.
- LOCAL CONTEXT REDUCTION: `grep`'s output is *reduced locally* before any of it
  is sent.

A tool that runs locally and ships its full output to the cloud is **local
execution and zero reduction**. Score it that way. Say it that way.

**R3 — The 100MB test.** 100MB sent to Claude, which returns a summary, is
**not** local reduction — the 100MB already left. 100MB into a local parser
producing 20KB that is then sent **is** local reduction. Apply this test to
every claimed reduction mechanism and state which side it falls on.

**R4 — MISSING is a valid and valuable finding.** Never manufacture a mechanism
that does not exist, never soften an absence into "partially", never describe an
extension point as if it were an implementation. "There is no X" backed by an
exhaustive search is a first-class result.

**R5 — Every score and every conclusion cites `file:line`.** A score without
citations is invalid. Where a claim rests on an exhaustive search returning
nothing, say so explicitly: *"exhaustive grep for `<pattern>` across `<paths>`
returns no runtime call site."*

**R6 — Reuse > extend > compose > create.** Do not propose a new subsystem where
an extension point already exists. Every recommendation must first answer: what
existing seam does this hang on, and why is that seam insufficient?

**R7 — Read-only.** No source changes, no commits, no dependency installs, no
formatters, no builds, no test runs. You may read, grep, glob and inspect. The
one file you write is the report.

**R8 — Mark every claim.** `OBSERVED` (read it in the code), `INFERRED` (follows
from observed code, reasoning shown), `UNVERIFIED` (an auditor asserted it and
you could not confirm it). An `UNVERIFIED` claim may not carry a score.

**R9 — Where auditors disagree, say so.** Do not average two contradictory
findings into a middle number. Name the disagreement, go to the code, resolve
it, and cite what resolved it. If it cannot be resolved, report both readings
and score the pessimistic one.

---

## 4. Method

1. Read the evidence files. Build a claim ledger: claim → file:line → auditor.
2. **Spot-verify.** Re-open the code behind every claim that carries a score or
   feeds a conclusion. Auditors make mistakes; you are the last reader.
3. Trace the outbound path end to end yourself, at least once: from a tool
   result being produced, through context assembly, obfuscation, compaction, to
   the bytes a provider receives. Name every place a transform could sit and
   whether one does.
4. Find the bypasses. For every enforcement point, ask who does **not** go
   through it: subagents, ADW phase seats, fusion panel seats, `code` phases,
   MCP servers, custom tools, extension-registered tools, hooks, the advisor,
   background jobs, title/classifier/memory calls, embedding calls.
5. Score. Then write.

### Search starting points (pointers, not findings — verify everything)

| Area | Where to look |
| --- | --- |
| Outbound wire | `packages/ai/src/providers/`, `packages/ai/src/stream.ts` |
| Context assembly | `packages/agent/src/agent-loop.ts`, `packages/agent/src/compaction/` |
| Secrets | `packages/coding-agent/src/secrets/` (`obfuscator.ts`, `message-transform.ts`), `docs/secrets.md` |
| Tool results | `packages/coding-agent/src/tools/` (`output-meta.ts`, `tool-result.ts`, `grep.ts`, `read.ts`, `bash.ts`) |
| Compaction | `packages/agent/src/compaction/`, `docs/compaction.md` |
| Extension points | `packages/coding-agent/src/extensibility/{extensions,hooks,custom-tools,plugins}/` |
| MCP | `packages/coding-agent/src/mcp/` |
| Subagents | `packages/coding-agent/src/task/` |
| ADW | `packages/coding-agent/src/adw/`, `crates/pi-tasks/` |
| Local models | `packages/coding-agent/src/tiny/`, `packages/coding-agent/src/config/model-discovery.ts`, `docs/local-models.md` |

---

## 5. Report structure

Exactly these sections, in this order. Do not add, drop or reorder.

1. **Executive Summary** — the conclusion first, in prose, no hedging.
2. **Final Classification** — exactly one of:
   - `A CLOUD-CENTRIC`
   - `B LOCAL EXECUTION-CLOUD CONTEXT`
   - `C PARTIALLY LOCAL-FIRST`
   - `D STRONGLY LOCAL-FIRST`
   - `E PRIVACY-ENFORCED LOCAL-FIRST`

   State the letter, then the evidence that forces it and the evidence that
   would have been required for the next letter up.
3. **Architecture Discovered** — what the system actually is. A `mermaid`
   diagram is appropriate here if it carries real structure.
4. **Actual Outbound Data Flow** — every category of bytes that reaches a
   provider, and by which code path.
5. **Local Execution Findings** — what runs on this machine. Per R2, this
   section says nothing about reduction.
6. **Context Selection** — how it is decided what enters context at all.
7. **Context Reduction** — what shrinks locally before egress. Apply R3
   explicitly to each mechanism.
8. **Tool-Result Handling** — production, bounding, truncation, spill,
   artifacts, what the model actually receives.
9. **Compaction Analysis** — where compaction runs, who summarises, what the
   summariser saw first. Apply R3.
10. **Privacy/Secret Handling** — detection, redaction, obfuscation, coverage,
    and precisely what is *not* covered.
11. **Provider Boundary Analysis** — is the boundary a single chokepoint or many
    doors? Enumerate the doors.
12. **OMP Existing Capabilities** — what the harness already provides.
13. **ADW Existing Capabilities** — what the workflow engine already provides.
14. **Subagent/Extension/MCP Bypass Analysis** — every path that reaches a
    provider without passing the enforcement points named in §11.
15. **Local Model Usage** — where local/tiny models run today, for what, and
    what they are *not* used for.
16. **Maturity Scorecard** — 0–5 on each dimension below, each row carrying
    `file:line` citations and a one-line justification. Scale: 0 absent · 1
    incidental · 2 partial/ad hoc · 3 works for the common path · 4 systematic
    with known gaps · 5 centrally enforced and unbypassable.

    - local tool execution
    - local repo exploration
    - local context selection
    - local relevance filtering
    - local context reduction
    - tool-result reduction
    - token efficiency
    - local sanitization
    - secret detection
    - PII detection
    - outbound inspection
    - outbound modification
    - outbound blocking
    - provider-independent enforcement
    - subagent coverage
    - extension/MCP coverage
    - local-model preprocessing
    - auditability
    - fail-closed behaviour

17. **Failure/Leakage Paths** — concrete, ordered by severity, each with the
    code path that permits it.
18. **Already Implemented** — do not rebuild these.
19. **Partially Implemented** — what exists, what is missing from it, the exact
    delta.
20. **Missing** — per R4, stated plainly with the search that establishes
    absence.
21. **Reuse-vs-Build** — per R6, a table: capability → existing seam → reuse /
    extend / compose / create → why.
22. **Minimal Architecture Changes** — the smallest set of changes that moves
    the classification up one letter.
23. **Recommended Tests** — tests that would *fail today* and prove a gap; not
    tests that pin current behaviour.
24. **Evidence Appendix** — claim → `file:line` → source evidence file, in the
    style of `safety-guard-evaluation.md` §15. Also list what was **not**
    examined and why.

---

## 6. Required final section: the twelve questions

End the report with a section titled **"The twelve questions"**, answering each
concisely — a sentence or two plus citations, not an essay. Lead with the
answer word.

1. **Is ADW local-first?**
2. **Is OMP already providing most of it?**
3. **What percentage of the target architecture exists?** Evidence-based, not
   guessed: derive it from the scorecard and show the arithmetic. A percentage
   without a stated derivation is prohibited.
4. **What raw information can currently reach cloud LLMs?**
5. **What is reduced locally first?**
6. **Are secrets reliably prevented from leaving?**
7. **Is enforcement centralized or bypassable?**
8. **Do we need a Local Context Engine?**
9. **Do we need a Privacy/DLP Engine?**
10. **Would a local model materially improve things?**
11. **The three highest-value changes.**
12. **What should NOT be built because it already exists?**

Questions 8 and 9 must be answered against R6: if the answer is yes, name the
seam it hangs on; if no, name what already does the job.

---

## 7. Style

Terse, declarative, evidence-first. No filler, no throat-clearing, no
"it is worth noting". Tables where the data is tabular. State conclusions
before their justification. Do not soften a negative finding, and do not
inflate a positive one — a system that scores 2 across the board and says so is
a more useful document than one that scores 4 and cannot show why.

Prefer a short report that is entirely defensible over a long one that is
partly assumed.
