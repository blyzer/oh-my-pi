# OMP v1 coordinator for OMP2

Use OMP v1 as the execution harness to develop OMP2 in `~/repositories/oh-my-pi`. Operate as
the coordinator until no actionable tasks remain in `docs/adr/TASKS.md`. Retain blocked
owner-gated rows and their evidence. Use the current `omp2` base and preserve unrelated work.

`AGENTS.md` already governs architecture, conventions, editing discipline, verification and
delivery, and the pstack rules govern playbooks and reply shape. Do not restate them here. This
file carries only what they do not: provider routing, arbitration, and the resource budget.

## Provider ladder

Use the configured ompv1 role mappings. Never invent a provider model id.

1. the configured primary role, currently OpenAI GPT
2. the configured OpenAI GPT role using its `openai-codex` provider identifier
3. the configured Devin model or agent role
4. the configured Gemini model
5. the configured GLM model

Use configured role mappings rather than hardcoding a model identifier. Skip duplicate entries.
For `reviewer`, `task.agentModelOverrides.reviewer` selects the reviewer model;
`task.agentAdvisor.reviewer: on` enables advisor assistance only. The advisor route is
`modelRoles.advisor`. Check these effective configuration paths and verify runtime model
resolution before dispatch; do not infer a successful route from configuration alone.

Take the first entry with quota and working credentials. On exhaustion, repeated timeout, or
unavailability, fall to the next immediately; do not retry an exhausted provider. A provider
failure never stops the loop unless every configured provider is unavailable. Record each
fallback in the status report.

One primary model per task. Combine models only for independent work, review, adversarial
checking, or verification. Mechanical edits and inspection go to the cheapest configured model
(`sonic`); architecture, debugging, security and final review go to the strongest available.

## Arbitration

Delegate only independent, bounded slices, each resumable after a provider failure: preserve its
branch, worktree and verification evidence.

The owner authorizes `/council` to decide software development questions, including decisions
previously reserved for the owner. Use it when a material engineering doubt or disagreement
needs deliberation; decide routine matters directly. Record the question, verdict, and evidence.

A council verdict cannot override an explicit prohibition, higher-priority instructions, or
required approval and safety controls. Never treat a failed or unavailable council as a verdict.
When `/council` is unavailable, report the blocker and do not claim it decided. Escalate missing
credentials and actions that require separate human approval.

Notify the owner through `omp-ntfy` when the run finishes, blocks on an owner decision, or
exhausts every provider.

## Resource budget

This MacBook is the primary constrained worker **and the self-hosted macOS CI runner**. Local
load corrupts CI: a concurrent stress run once failed four unrelated process and signal tests in
the `Rust workspace and acceptance proofs` job. Before any heavy local run, check for an
in-flight macOS job; prefer `just remote-*` on BuildBuddy for Linux-valid verification.

Prefer targeted checks over workspace checks. Keep `CARGO_TARGET_DIR` shared. Avoid concurrent
heavy builds, test runs or agents. Stop nonessential background jobs when pressure appears.

## Monitoring and integration

Monitor each active PR through OMP native tools or extensions. Start `pr_monitor` when available,
inspect its reports and current-head review and CI evidence, and restart it after a session change.
Record monitor start, stop, failures, significant CI or review changes, and merge outcomes in the
ledger or a linked coordinator record. Do not claim monitoring is active without a successful start.

Track required GitHub Actions checks and BuildBuddy verification, including invocation links and
results. Use native tools or extensions when available; otherwise use the supported GitHub or
BuildBuddy interface. Record unavailable monitoring and verification separately from passing checks.
Before heavy local work, check whether self-hosted macOS CI is running.

Merge only after independent review, green required CI, and all applicable ADR and owner gates.
Preserve worktrees containing unrelated or uncommitted work.

## Status

At every stopping point report completed, pending, blocked and next, each with its evidence, and
keep the ledger current as each item moves.