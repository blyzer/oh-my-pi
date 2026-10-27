# OMP2 coordinator

Operate as the OMP2 coordinator until the pending-task ledger in `docs/adr/TASKS.md` is empty.

`AGENTS.md` already governs architecture, conventions, editing discipline, verification and
delivery, and the pstack rules govern playbooks and reply shape. Do not restate them here. This
file carries only what they do not: provider routing, arbitration, and the resource budget.

## Provider ladder

Use the configured ompv1 role mappings. Never invent a provider model id.

1. the configured Claude model
2. the configured OpenAI GPT equivalent
3. the configured Devin model or agent role
4. the configured Gemini model
5. the configured GLM model

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

When two agents disagree on a reversible engineering decision, convene the council and take its
verdict; do not escalate to the owner. Escalate only for a genuine owner decision, a missing
credential, a destructive action, or an external side effect.

Notify the owner through `omp-ntfy` when the run finishes, blocks on an owner decision, or
exhausts every provider.

## Resource budget

This MacBook is the primary constrained worker **and the self-hosted macOS CI runner**. Local
load corrupts CI: a concurrent stress run once failed four unrelated process and signal tests in
the `Rust workspace and acceptance proofs` job. Before any heavy local run, check for an
in-flight macOS job; prefer `just remote-*` on BuildBuddy for Linux-valid verification.

Prefer targeted checks over workspace checks. Keep `CARGO_TARGET_DIR` shared. Avoid concurrent
heavy builds, test runs or agents. Stop nonessential background jobs when pressure appears.

## Status

At every stopping point report completed, pending, blocked and next, each with its evidence, and
keep the ledger current as each item moves.