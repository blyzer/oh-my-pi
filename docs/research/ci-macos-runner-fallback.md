# Hybrid GitHub-hosted + self-hosted macOS runners

## Question

Can CI combine GitHub-hosted and self-hosted runners per step so a PR prefers
a fast idle self-hosted Mac when one is available, and otherwise uses hosted
`macos-15` instead of waiting on a busy or offline machine?

## Verdict

**Yes, and it is useful for this repository.** GitHub Actions has no native
`runs-on` OR (a label array is AND). A small resolve job that probes the
Actions runners API and emits JSON for `fromJSON(...)` is the supported
pattern. omp² already mixes topologies (hosted Linux, optional self-hosted
Mac via `vars.MACOS_RUNNER`, BuildBuddy remote Linux); the missing piece was
availability-aware selection for the required macOS jobs.

## What GitHub supports

| Mechanism | Behavior | Fit here |
|---|---|---|
| `runs-on: [self-hosted, macOS, ARM64]` | AND of labels; queues until a match is free | Fast when idle; blocks merges up to ~24h when offline/busy |
| `runs-on: ${{ fromJSON(vars.MACOS_RUNNER \|\| '"macos-15"') }}` | Static choice at workflow parse time | Current baseline; no live availability |
| Resolve job → `needs.*.outputs` → `fromJSON` | Pick labels after listing runners (`status=online`, `busy=false`) | Prefer warm Mac; fall back to hosted without pending forever |
| Larger hosted runners / matrix fan-out | More CPU or shards | Orthogonal; does not replace Seatbelt/P12 on macOS |
| BuildBuddy `just remote-heavy` | Remote Linux proof from a hosted driver | Already in `ci.yml`; not a macOS substitute |

Fork pull requests must not land on self-hosted runners (untrusted code on a
persistent machine). That gate lives in **workflow YAML** (`macos_runner`'s
fork step and the `rust` job's `runs-on` expression), not in
`scripts/ci-resolve-macos-runner.sh`: a fork PR can rewrite the script, so a
script-only `IS_FORK` check is not a security boundary (GitHub Advanced
Security finding on PR #458).

## Measured cost of the status quo

From `docs/BRANCH_PROTECTION.md` and `docs/research/ci-test-acceleration.md`:

- Hosted `macos-15` `rust` job: about **52 minutes** wall (median in recent
  samples ~55 min).
- Warm self-hosted Apple silicon: about **15 minutes** for the same job.
- When `MACOS_RUNNER` points at a single Mac that is offline or occupied,
  required checks (`Rust workspace and acceptance proofs`, `Lint workspace
  (macOS)`) stay **pending** and block merge until an admin bypasses the
  ruleset or unsets the variable.

So the hybrid resolve keeps the ~15 min path when the Mac is free, and the
~52 min hosted path when it is not — instead of an unbounded queue.

## What we changed

1. `ci.yml` job `macos_runner` — on fork PRs, a workflow step emits
   `"macos-15"` and skips checkout/script. Otherwise a cheap `ubuntu-24.04`
   job with `actions:read` runs the resolve script and writes `outputs.runs_on`.
2. `scripts/ci-resolve-macos-runner.sh` — for trusted/same-repo runs only:
   given `PREFERRED` / `FALLBACK` / `GH_TOKEN` / `REPO`, probe idle runners and
   print JSON for `runs-on`. Empty/invalid preference, API errors, and “no
   online idle match” emit hosted fallback. Not used for fork gating.
3. `rust` — `needs: macos_runner`; `runs-on` is hosted when the PR is a fork
   (workflow expression), else `fromJSON(needs.macos_runner.outputs.runs_on)`.
4. `lint_macos` — always `runs-on: macos-15`. It has no self-hosted target
   cache, and a warm clippy was no faster than a cold one. It still has to be
   a Mac so `#[cfg(target_os = "macos")]` is linted.

Non-gating macOS workflows (`p8-baseline.yml`, `package-macos.yml`) still use
the static `vars.MACOS_RUNNER || macos-15` expression; they are not merge
blockers. Reuse the same script there if those start to queue behind CI.

## How to enable the fast path

```sh
gh variable set MACOS_RUNNER --body '["self-hosted","macOS","ARM64"]'
```

With the variable unset, resolve emits hosted `macos-15` immediately (no API
match required). With it set, CI uses the self-hosted labels only while a
matching runner is online and idle.

## Risks and non-goals

- **Hosted-only test flakes.** Three tests have failed on hosted `macos-15`
  while passing on the self-hosted Mac (`BRANCH_PROTECTION.md`). Fallback is
  still better than a pending required check; fixing those tests remains
  separate work.
- **One Mac, one heavy job.** Only `rust` takes the self-hosted labels. `lint_macos`
  stays on hosted `macos-15`, so the two no longer serialize on that machine.
- **Probe race.** A runner can become busy between resolve and job start; the
  job then queues briefly on self-hosted. Acceptable; the important case is
  offline / long-busy → hosted.
- **Permissions.** Listing runners needs `actions: read` on the resolve job.
  Failures fall back to hosted rather than failing the workflow.
- **Fork workflow edits.** A fork can also edit `ci.yml` on `pull_request`.
  Keep GitHub’s “require approval for first-time contributors” (or runner
  groups that only accept workflows from the default branch) enabled so a
  rewritten workflow cannot schedule onto the Mac without review.
- **Not** replacing BuildBuddy remote Linux or splitting the macOS suite onto
  Linux — those are other tracks in the acceleration research.

## Related

- `docs/BRANCH_PROTECTION.md` — required checks and previous manual escape hatch
- `docs/research/ci-test-acceleration.md` — Phase 7 runner/remote
- `docs/research/omp2-build-acceleration.md` — hybrid topology inventory
- ADR 0040 — remote Linux execution (orthogonal to this Mac resolve)
