# BuildBuddy CI on pull requests: a safe mode for trusted PRs, and the limit that remains

Status: research and decision proposal. Nothing in this note is applied: no workflow, script, ruleset or repository setting was changed, and no remote run was made.
Date: 2026-10-13
Base: `origin/omp2` at `e8f2d027eb`.

Closes the open todo "BuildBuddy CI PR execution: design a safe mode for internal/trusted PRs without exposing secrets to forks, and document the limit."

Evidence keys:
- **OBSERVED**: read in the tree at the base commit above, or read from the GitHub API with `gh` on 2026-10-13 (names and settings only; no secret value was read).
- **[INFERENCE]**: the author's reasoning from observed facts.
- **UNVERIFIED**: no source or run confirmed it. Each one is collected in section 9.
- **GH docs**: a claim about GitHub Actions semantics, with the GitHub page it comes from. Quotes are verbatim from the page as fetched on 2026-10-13.

Inputs read: `.github/workflows/ci.yml` (job `remote_linux`), `scripts/remote-verify.sh`, `scripts/remote-verify-runner.sh`, `justfile` (`remote-heavy`), `.github/rulesets/omp2-protected.json`, [`docs/BRANCH_PROTECTION.md`](../BRANCH_PROTECTION.md), [`docs/audits/remote-verification-trial.md`](../audits/remote-verification-trial.md), [ADR 0040](../adr/0040-remote-build-and-test-execution.md) Parts H and the Findings it cites, and [`omp2-build-acceleration.md`](omp2-build-acceleration.md) item 30.

## 1. Decisions (summary)

| # | Decision | Verdict |
|---|---|---|
| D1 | Keep `pull_request` as the trigger and keep the same-repo guard as defence in depth | **Adopt** |
| D2 | Move the key out of a repository secret into two admin-configured GitHub Environments: `buildbuddy-main` (no reviewers, only `omp2`/`main`) and `buildbuddy-pr` (required reviewer, only `refs/pull/*/merge`) | **Adopt** |
| D3 | A pull request gets a separate, lower-privilege BuildBuddy key, never the key that omp2 pushes and seeds use | **Adopt**, subject to one trial that this note does not run (section 9, U1) |
| D4 | Fork pull requests never get the key. After review, a maintainer re-pushes the head to a same-repo branch, which then takes the D2 path | **Adopt** |
| D5 | `pull_request_target` to run PR code | **Reject** |
| D6 | A label as the approval | **Reject** |
| D7 | `workflow_dispatch` as the PR check | **Reject as the gate**; keep it as the push-to-`omp2` and on-demand path |
| D8 | Treat the remaining exposure (section 7) as accepted and owned by ADR 0040 Slice 1 | **Record** |

The one sentence to remember: while verification runs through `bb remote`, every process of the remote run can read the BuildBuddy key, so any design here decides **who may cause that and how much the key can do**, not whether the key reaches PR code.

## 2. What the gate does today (OBSERVED)

### 2.1 Triggers (`ci.yml:3-35`)

`push` to `main` and `omp2` with a `paths` list, `pull_request` to `main` and `omp2` with no path filter, and `workflow_dispatch`. Workflow permissions are `contents: read` (`ci.yml:37-38`).

### 2.2 The job `Remote Linux verification (BuildBuddy)` (`remote_linux`, `ci.yml:789-850`)

Gates, quoted:

```yaml
needs: changes
if: ${{ !cancelled() && needs.changes.outputs.ci != 'false' }}
```

The trust step (`ci.yml:798-817`) computes two values in its `env`:

```yaml
FORK: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name != github.repository }}
HAS_KEY: ${{ secrets.BUILDBUDDY_ORG_API_KEY != '' }}
```

If `FORK = true` it prints a notice, writes a step summary, sets `remote=false` and exits 0. If `HAS_KEY != true` it prints `::error title=BuildBuddy key missing::...` and exits 1. Otherwise it sets `remote=true`. Every later step (`checkout`, `Install pinned BuildBuddy client`, `Probe BuildBuddy connectivity and auth`, `Run Linux-valid Cargo verification remotely`) has `if: steps.trust.outputs.remote == 'true'`.

Secrets and variables the job reads:
- Secret `BUILDBUDDY_ORG_API_KEY`, three times: in `HAS_KEY` (only the boolean reaches the step) and as `BUILDBUDDY_API_KEY` in the `env` of exactly two steps, the probe (`ci.yml:843`) and the heavy run (`ci.yml:849`).
- No `vars.*` in this job. (`vars.MACOS_RUNNER` and `vars.CI_MIN_FREE_GB` are read by other jobs; the repository has no variables: `gh variable list` returned nothing.)
- `github.token` is not used here (only the `changes` job uses it).

The key then travels as `BUILDBUDDY_API_KEY` into `scripts/remote-verify.sh`, which exports it to `bb remote`; `bb` hands it to every process of the remote run (script header, `remote-verify.sh:9-11`; ADR 0040 Part H.2). `remote-verify-runner.sh:11` runs `unset BUILDBUDDY_API_KEY REPO_TOKEN REPO_USER`, which ADR 0040 H.2 says cannot hide the copy in a bazelrc beside the checkout. The script passes no `include-secrets`, so BuildBuddy-stored secrets are not injected (`remote-verify.sh:74-85`).

The check is required: `.github/rulesets/omp2-protected.json:47` lists `Remote Linux verification (BuildBuddy)`; the Admin role bypasses the ruleset (`bypass_mode: always`).

### 2.3 Behaviour by event

| Event | `FORK` | `HAS_KEY` | Result |
|---|---|---|---|
| push to `omp2`/`main` | false | true | Real remote run, key on two steps |
| `workflow_dispatch` (any ref the dispatcher picks) | false | true | Real remote run, key on two steps |
| `pull_request`, head in this repo | false | true | Real remote run, key on two steps |
| `pull_request`, head in a fork | true | false (GitHub withholds secrets) | Notice, `remote=false`, job **succeeds**; the required check is green and nothing ran remotely |
| `pull_request` by Dependabot (not configured today: no `dependabot.yml`) | false | false | Job **fails** with "key missing" |
| key secret absent | any | false | Fails closed |
| `changes` says no CI path changed | n/a | n/a | Job skipped, which reports Success |

Supporting GitHub semantics:
- Fork runs get no secrets: "With the exception of `GITHUB_TOKEN`, secrets are not passed to the runner when a workflow is triggered from a forked repository." (GH docs, [Using secrets in GitHub Actions](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets)). So the `FORK` test does not add secrecy; it turns an otherwise failing run into a pass.
- An unset secret evaluates to an empty string (same page), which is what `HAS_KEY` relies on.
- Dependabot runs are "treated as if they were opened from a repository fork" and "do not have access to any secrets that are normally available" (GH docs, [Dependabot on GitHub Actions](https://docs.github.com/en/code-security/reference/supply-chain-security/troubleshoot-dependabot/dependabot-on-actions)).
- A job skipped by a conditional "reports Success" (GH docs, [Troubleshooting required status checks](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks), "Handling skipped but required checks").
- The "send secrets to fork PRs" setting exists only for private repositories: "Available to private repositories only" (GH docs, [Managing GitHub Actions settings for a repository](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository)). This repository is public today (below), so a fork cannot get secrets by that route.

### 2.4 Facts about this repository (OBSERVED via `gh`, 2026-10-13)

| Fact | Value |
|---|---|
| Visibility | **PUBLIC** (a fork of `can1357/oh-my-pi`). ADR 0040 and the trial audit call the repo private; that is no longer true |
| Collaborators | 1 (`blyzer`, admin) |
| Last 200 PRs (all states) | all authored by `blyzer`, all from branches of this repo (`isCrossRepository: false`); no fork PR yet |
| Repository secrets | `BB_API_KEY` (set 2026-09-12) and `BUILDBUDDY_ORG_API_KEY` (set 2026-10-10). Both are repository-level |
| Environments | none |
| Fork-PR approval policy | `first_time_contributors` |
| Default `GITHUB_TOKEN` | `write`; Actions may approve PRs (`can_approve_pull_request_reviews: true`) |
| Self-hosted runner | `Envers-MacBook-Pro` (macOS, ARM64) is registered and online; no job uses it today because `MACOS_RUNNER` is unset |

`BB_API_KEY` is the key ci.yml's own comment calls Nostro's (`ci.yml:838-839`). No workflow, script or justfile line in this repository references `secrets.BB_API_KEY` (grep of `.github`, `scripts`, `justfile`), so today it is a secret held by this repository that nothing here uses.

### 2.5 What this means, stated precisely

1. **Fork PRs are safe from the key today.** Not because of the `if`, but because GitHub withholds secrets (section 2.3) and the guard is a second layer.
2. **Same-repo PRs get the key, and the guard is written by the PR's author.** A `pull_request` run uses the workflow file from the PR's merge commit: "The `pull_request` event ... runs the workflow file from the **merge commit of the pull request**" (GH docs, [Securely using pull_request_target](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target)). So a same-repo PR can edit `ci.yml` (remove the `FORK`/`HAS_KEY` logic, print the secret, reference any other secret, change `runs-on`). It can also edit `justfile`, `scripts/remote-verify.sh` and `scripts/remote-verify-runner.sh`; the `unset` line is part of the PR, so it protects nothing against a hostile PR.
3. **The effective control is who can push a branch to this repository.** GitHub's own statement: "Any user with write access to your repository has read access to all secrets configured in your repository." (GH docs, [Secure use reference](https://docs.github.com/en/actions/reference/security/secure-use), "Use secrets for sensitive information"). Today that is one human. It is also every agent or automation that pushes with that human's credentials, and every crate whose `build.rs` or proc-macro runs in the remote run (ADR 0040 H.2: "the repo and its 1315 registry packages").
4. **The "green" for a fork PR is hollow for Linux unit tests.** `nextest run --workspace --exclude omp-e2e` and the doctests run in the macOS job `rust` (`ci.yml:404-411`) and in the remote job; the Linux jobs run only clippy (`lint_linux`, `ci.yml:581`) and `omp-e2e` subsets (`p9_p10_linux`, `p7_linux`). So `cfg(target_os = "linux")` unit tests execute for a fork PR nowhere. The required check passes anyway. (OBSERVED from the `cargo`/`nextest` lines of `ci.yml`.)

## 3. Threat model for the key

Assume code written by a stranger or a compromised dependency runs on the job that holds the key. What can it do with `BUILDBUDDY_ORG_API_KEY`?

| Capability | Basis |
|---|---|
| Read the key itself, from any step it controls, from the process environment inside the remote VM, and from the bazelrc beside the checkout | ADR 0040 H.2 and Findings 4 (read from BuildBuddy sources, not run); GitHub's own caveat that "automatic redaction is not guaranteed" (GH docs, Secure use reference) |
| Send the key anywhere (the remote VM has external network; the hosted runner has no egress limit configured in `ci.yml`) | OBSERVED (`ci.yml`); ADR 0040 H.2 ("Firecracker networking is `external`") |
| Start its own remote runs on the org: compute spend, up to the plan's limits | BuildBuddy: "This key can be used by anyone in your organization, as it ties builds to your org" ([Authentication guide](https://www.buildbuddy.io/docs/guide-auth)) |
| Write CAS and AC entries, unless the key is read-only | BuildBuddy: a read-only key "will allow users of these keys to download from the remote cache, but not upload" (same page); ADR 0040 line 846: "A key with AC write could then forge cached results" |
| Overwrite the snapshot every later run resumes from: run `bb remote --run_from_branch=omp2` with `x-buildbuddy-platform.remote-snapshot-save-policy=always`, exactly what `remote-verify.sh --seed` does (`remote-verify.sh:86-91`) | OBSERVED mechanism; snapshots fall back to the default branch's (BuildBuddy [Remote Bazel](https://www.buildbuddy.io/docs/remote-bazel)); that a stolen key can poison it is [INFERENCE]; ADR 0040 says "Snapshot reuse needs an API key with AC write", so a read-only key blocking it is UNVERIFIED (U1) |
| Read invocation data of the org (logs, other projects' invocations) | UNVERIFIED (U3); depends on whether the org is dedicated (item 30 of `omp2-build-acceleration.md`, still open) |
| Use the GitHub App token `REPO_TOKEN` that `bb remote` puts in the runner | Scope UNVERIFIED (ADR 0040 H.2, open question list) |

The worst realistic outcome is **verdict forgery**: a poisoned snapshot or cache entry makes a later required-check run on `omp2` green over code it did not verify. ADR 0040's own mitigation, "never let that organization's action cache gate an omp verdict" (H.2), no longer holds, because the BuildBuddy job is a required check.

The second is **credential reach beyond BuildBuddy**: on a same-repo PR, the PR's workflow can reference every repository secret, not just this one, so `BB_API_KEY` (another project's key) is exposed to any writer's PR today. A repository-level secret has no gate other than "can push a branch".

## 4. The designs compared

Common to all: the orchestration job is a GitHub-hosted `ubuntu-24.04` runner, free on a public repo ("GitHub Actions usage is free ... for public repositories that use standard GitHub-hosted runners", GH docs, [Billing for GitHub Actions](https://docs.github.com/en/billing/concepts/product-billing/github-actions)). Cost below is therefore BuildBuddy cost: compute and cache transfer. Measured: `just remote-heavy` exited 0 after 864 s on run `38127067863` (`omp2-build-acceleration.md` section 4); `BRANCH_PROTECTION.md` says a cold run is about 15 minutes. A seed snapshot upload is about 56 GB (`remote-verify.sh:15`). The current BuildBuddy plan and usage were not read (U4).

### 4.1 Design A: `pull_request` with a same-repo-only guard (today)

- **Who can trigger:** anyone with write access (push a branch, open or update a PR). A fork author can trigger a workflow run, subject to the approval policy, but without secrets (section 2.3).
- **Secret exposure:** the repository secret, to the PR's own workflow file. Also every other repository secret (section 3).
- **Malicious same-repo PR:** everything in section 3. The guard is in the file the PR controls.
- **Blast radius:** the org behind the key, plus `BB_API_KEY`'s.
- **Revocation:** delete or replace the repository secret (GitHub); delete the key in BuildBuddy, which "can take up to 5 minutes to take effect" (Authentication guide). Whether an in-flight run keeps the old value is UNVERIFIED.
- **Cost:** one remote run per push to each same-repo PR; `cancel-in-progress` (`ci.yml:40-42`) drops superseded ones. Bounded by how many people can push, today one.
- **Verdict:** adequate only while "can push a branch" equals "fully trusted, including every dependency", which is not true of agent-authored or dependency-bumping PRs.

### 4.2 Design B: `pull_request_target`

- **Who can trigger:** anyone who can open a PR, including strangers. Approval policies do not apply: "workflows triggered by these events will always run, regardless of approval settings" (GH docs, Managing GitHub Actions settings for a repository).
- **Secret exposure:** full. "The job receives the base repository's `GITHUB_TOKEN` and access to repository and organization secrets." (GH docs, Securely using pull_request_target).
- **Malicious PR:** to test the PR the workflow must check out its head and run it; GitHub names this the "pwn request" and says: "An attacker only needs to open a pull request from a fork whose `Makefile` (or build script, test command, dependency, or configuration file) contains malicious commands. Those commands then run with the base repository's secrets and token." (same page). Here the equivalent is `justfile`, `scripts/remote-verify*.sh`, any `build.rs`. `actions/checkout` now refuses fork PR heads under this trigger unless `allow-unsafe-pr-checkout: true` is set ([actions/checkout README](https://github.com/actions/checkout)), which is the opt-out GitHub tells you to use only if "the checked-out code is never executed". `bb remote` executes it by design.
- **Blast radius:** the key plus a possibly write-capable `GITHUB_TOKEN` (the repository default is `write`; only the explicit `permissions:` block narrows it) plus the shared default-branch cache ("share the same cache of the main branch", GH docs, Secure use reference).
- **Revocation:** same as A.
- **Cost:** unbounded by strangers (no approval gate).
- **Extra:** GitHub's default policy blocks `pull_request_target` in public repositories, in evaluate mode today and enforced from 2026-11-02 for repositories that used the default policy (GH docs, Securely using pull_request_target). See section 8.
- **Verdict: reject.** Splitting into an unprivileged `pull_request` build plus a privileged `workflow_run` does not help here, because the thing that needs the key is the code execution itself. GitHub: a `workflow_run` workflow "should treat artifacts uploaded by other workflows as untrusted data" (same page).

### 4.3 Design C: a label as the approval

Two variants:
- *`pull_request` with `types: [labeled]` and `if: contains(...labels..., 'x')`.* The condition is in the PR's own workflow file, so a writer deletes it; for a fork PR the label is irrelevant because secrets are withheld anyway. No security value beyond ordering.
- *`pull_request_target` with a label.* The workflow file is trusted and secrets are present, so the label does gate a fork author. But GitHub Security Lab says it "is still prone to a race condition in which the attacker may push new changes after the workflow was approved (labeled), but has not started yet. As such this approach should only be used as a temporary solution" ([Preventing pwn requests](https://securitylab.github.com/resources/github-actions-preventing-pwn-requests/)). It also inherits all of Design B.

- **Who can trigger:** the label is applied by "anyone with triage access" (GH docs, [Managing labels](https://docs.github.com/en/issues/using-labels-and-milestones-to-track-work/managing-labels)); the run needs the PR author or event actor as above.
- **Blast radius, revocation, cost:** as A (variant 1) or B (variant 2).
- **Verdict: reject.** A label is metadata, not an enforced approval of a specific commit.

### 4.4 Design D: environment with required reviewers (secret lives in the environment)

Mechanics, all GH docs ([Deployments and environments](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments), [Managing environments](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)):
- "Secrets stored in an environment are only available to workflow jobs that reference the environment. If the environment requires approval, a job cannot access environment secrets until one of the required reviewers approves it."
- "Anyone that can edit workflows in the repository can create environments via a workflow file, but only repository admins can configure the environment." A new environment made by a workflow "will not have any protection rules or secrets".
- Up to six required reviewers; an optional "prevent self-review"; deployment branch and tag rules matched against `GITHUB_REF`; administrators can bypass by default.
- "Adding another branch rule for `refs/pull/*/merge` would also allow workflows triggered by `pull_request` events to deploy to the environment."
- Environment secrets and required reviewers are available on private repositories only on paid plans; on Free they are "only available for public repositories", and converting to private makes "protection rules or environment secrets ... ignored" (same pages). Plan tier here is UNVERIFIED (U5), but the repository is public, so it works today.

Why it fixes the structural problem in A: the gate and the secret move from a file the PR edits to settings only an admin edits. A PR can name the environment but cannot remove its protection or read a secret before approval.

- **Who can trigger:** anyone with write access can start the run; nothing runs with the key until a required reviewer approves that run.
- **Secret exposure:** none before approval; after approval, the approved run's code can read the key (the limit, section 7). Other repository secrets are not reachable if the key is no longer a repository secret and `BB_API_KEY` is removed or moved (section 6).
- **Malicious PR:** has to survive a human look at the diff. Cannot read the key by editing `ci.yml` alone, and cannot choose `buildbuddy-main` because that environment's branch rule excludes PR refs.
- **Blast radius:** bounded by what the PR key can do (D3), not by the org-wide write key.
- **Revocation:** as A, but the PR key is separate, so revoking it does not stop `omp2` runs; delete or rotate the environment secret; delete the BuildBuddy key (up to 5 minutes).
- **Cost:** one approval click per PR run that should reach BuildBuddy. Unreviewed runs cost nothing. A waiting required check blocks merge until approved; the ruleset's Admin bypass (`BRANCH_PROTECTION.md`) is the escape hatch. Expiry of an unapproved environment wait is UNVERIFIED (U6; the 30-day expiry documented on [Approving workflow runs from forks](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks) is for fork approvals).
- **Friction for a solo maintainer:** "prevent self-review" must stay off while there is one reviewer, or no run can ever be approved; with one human this is one approval click, not a four-eyes review. It still converts "push a branch" into "push a branch and consciously release the key to exactly this run", which also catches an agent-authored PR that changed `Cargo.lock`, `build.rs`, `justfile` or `scripts/`.

### 4.5 Design E: maintainer-triggered `workflow_dispatch`

- **Who can trigger:** "Write access to the repository is required" (GH docs, [Manually running a workflow](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)); the workflow file must exist on the default branch ([Events that trigger workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_dispatch)); you can dispatch "against any branch or tag". Whether the workflow definition that runs is the one on the chosen ref is not stated on those pages: UNVERIFIED (U7). If it is, a writer dispatching on their own branch controls the workflow file and reads the repository secret, as in A.
- **Secret exposure:** the repository secret, unless the key is in an environment whose deployment branches exclude feature branches (then only a dispatch on `omp2` gets it).
- **Malicious PR:** cannot trigger it without write access, and a dispatch on `omp2` runs only merged code. But then it does not test an unmerged PR.
- **Blast radius, revocation:** as A or D depending on where the key lives.
- **Cost:** only explicit runs.
- **Why not the gate:** a required check must report on the PR head. A dispatch run is not a PR-triggered run; making it report the required check name on the right SHA needs extra plumbing (check-run API, permissions), and that plumbing is not documented in the sources read for this note. A `pull_request`-triggered job that merely "passes" while a dispatch does the real work would be a hollow required check.
- **Verdict:** keep `workflow_dispatch` for on-demand runs on `omp2`; do not make it the PR gate.

### 4.6 Design F (baseline): remote only on push to `omp2`, never on PRs

This was the shape before the job moved into `ci.yml` (`BRANCH_PROTECTION.md` says it "replaces the push-only `buildbuddy-remote.yml`").
- **Secret exposure:** only merged code ever meets the key.
- **Cost:** one run per merge.
- **Why not:** it cannot be a required PR check, and Linux unit tests then run pre-merge nowhere (section 2.5, point 4). It is the fallback if the owner rejects D2's approval click.

### 4.7 Side-by-side

| | A: `pull_request` + guard | B: `pull_request_target` | C: label | D: environment + reviewer | E: dispatch | F: push only |
|---|---|---|---|---|---|---|
| Who can trigger | any writer | any PR author | triage+ labels | any writer starts, reviewer releases | any writer | merge to `omp2` |
| Secret before human action | yes, whole repo | yes, whole repo + token | yes | **no** | depends (U7) | n/a |
| Gate lives in | PR-editable YAML | base YAML (safe) but runs PR code | label metadata (race) | admin-only settings | n/a | n/a |
| Fork author | no key | key if workflow runs PR code | key if variant 2 | no key (job skipped) | cannot | no key |
| PR code meets key | always | always | after label | after approval | on chosen ref | only after merge |
| Stranger-driven cost | none | **unbounded** | variant 2: unbounded | none | none | none |
| Required check possible | yes | yes | yes | yes | no (plumbing) | no |

## 5. Recommendation

**Adopt D (environments), keep A's guard, and give pull requests their own key.**

1. `pull_request` stays the trigger, so the required check reports natively on the PR head and merge ref (D1).
2. The key moves from a repository secret into two environments (D2):
   - `buildbuddy-main`: deployment branches `omp2` and `main`, no required reviewers. Used by `push` and by `workflow_dispatch` on those branches. A dispatch on any other ref is denied by the branch rule.
   - `buildbuddy-pr`: deployment branch rule `refs/pull/*/merge`, one required reviewer (the owner), prevent-self-review off until a second reviewer exists.
   The job picks one with an expression on `github.event_name`; the `environment` name accepts an expression over the `github` context (GH docs, [Workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax), `jobs.<job_id>.environment`). A PR cannot select `buildbuddy-main` because its branch rule excludes PR refs, and cannot weaken either environment because only an admin configures it.
3. `buildbuddy-pr` holds a **new, separate BuildBuddy key**, ideally read-only (D3), labelled in BuildBuddy so it can be deleted alone. This is what bounds the blast radius of an approved-but-malicious run: no AC/CAS write and, if BuildBuddy honours it, no snapshot save (U1). `buildbuddy-main` keeps the current write-capable key because `omp2` pushes and `just remote-seed` need it.
4. Fork PRs never reach an environment (D4): the job is skipped before any environment is evaluated (the `if:` in section 6, W1), so no approval prompt appears and the required check reports Success per the skipped-check semantics in section 2.3. To get Linux unit tests for a fork PR, the maintainer reviews the diff, pushes the head to a branch of this repository (for example `gh pr checkout N && git push origin HEAD:refs/heads/fork-pr/N`) and opens a same-repo PR, which then takes the approval path.
5. Pin the checkout to the event's commit so the approved run and the executed tree cannot diverge (W4).

Decisions about what is **not** done: no `pull_request_target`; no label as approval; no dispatch-as-gate; no reliance on `unset` lines or other in-repo scripts as a control, because a PR can edit them.

## 6. Concrete changes this needs (a list; not applied)

### Workflow (`.github/workflows/ci.yml`, `remote_linux`)

- **W1. Skip fork PRs at job level, before any environment.** Add `github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository` to the job's `if`. Keep the trust step's `FORK` test as a second layer. The skipped job reports Success (section 2.3). That a skipped job never reaches environment protection is [INFERENCE] from the `if` and environment pages; confirm on a test fork PR (A3).
- **W2. Add `environment: ${{ github.event_name == 'pull_request' && 'buildbuddy-pr' || 'buildbuddy-main' }}`.** Leave `secrets.BUILDBUDDY_ORG_API_KEY` as the reference on both steps; each environment supplies its own value under the same name, so the step `env` lines do not change.
- **W3. Move the key-presence check into the job.** A job without `environment:` cannot see environment secrets, so any separate "gate" job loses `HAS_KEY`. Keep `HAS_KEY` in the first step of the environment job; it still fails closed on a missing key. If a visible message for fork PRs is wanted, add a separate **non-required** job with a different name that prints the existing notice when `FORK` is true; do not reuse the required job's name.
- **W4. Pin the checkout.** `actions/checkout` with `ref: ${{ github.sha }}` in `remote_linux` (the events page defines `GITHUB_SHA` for `pull_request` as the last merge commit). What `actions/checkout` does by default is not read here, so pin explicitly rather than rely on it.
- **W5. Keep `permissions: contents: read` on the job** (already present) and do not add `id-token`, `pull-requests`, or `actions: write`.
- **W6. Update comments and docs together:** the block comment above `remote_linux`, the paragraph in `BRANCH_PROTECTION.md` (it currently says "only code from this repository ever gets it"), the `AGENTS.md` CI paragraph, and an amendment line in ADR 0040 Part H. Required check names are unchanged, so `just check-ci-paths` and the ruleset need no edit.
- **W7. Optional cost bound:** lower `timeout-minutes: 90` (`ci.yml:794`) and the script's `--timeout=60m` (`remote-verify.sh:83`) toward the measured 864 s cold run. Not required by this design.

### Repository settings (admin; GitHub UI or API)

- **S1.** Create environment `buildbuddy-main`; deployment branches: selected `omp2` and `main`; no reviewers.
- **S2.** Create environment `buildbuddy-pr`; deployment branches: selected branch rule `refs/pull/*/merge`; required reviewers: the owner; prevent self-review: off while there is one reviewer; keep "allow administrators to bypass" as the documented escape hatch.
- **S3.** Create `BUILDBUDDY_ORG_API_KEY` as an environment secret in each environment (different values: the PR key and the write key). **Delete the repository-level `BUILDBUDDY_ORG_API_KEY`**, otherwise any same-repo workflow can still read it.
- **S4.** Decide `BB_API_KEY`: nothing in this repository uses it. Delete it from this repository (or move it into a protected environment), because any writer's PR can currently reference it.
- **S5. In BuildBuddy:** create a new key for PR runs, read-only if the trial in U1 shows remote runs still work with it; label it; keep the current key for `buildbuddy-main`; set a usage alert if the plan offers one (U4).
- **S6.** Actions, General, "Approval for running fork pull request workflows from contributors": change `first_time_contributors` to "Require approval for all external contributors". GitHub's own warning applies to the weaker settings: "A malicious user could meet this requirement by getting a simple typo or other innocuous change accepted by a maintainer" (GH docs, Managing GitHub Actions settings for a repository). This protects compute, not the key.
- **S7.** Adjacent hardening, independent of this design: set the default `GITHUB_TOKEN` to read-only and turn off "Allow GitHub Actions to create and approve pull requests" (GH docs, Secure use reference, "Use secrets" and "Preventing GitHub Actions from creating or approving pull requests"). `ci.yml` and `pr-labels.yml` already declare `permissions:`; check the other three workflows declare theirs before flipping.
- **S8.** When a second writer exists: turn on prevent-self-review with two reviewers, and add `CODEOWNERS` for `.github/`, `scripts/`, `justfile`, `Cargo.lock` and `.cargo/` (GH docs: "You can use the `CODEOWNERS` feature to control how changes are made to your workflow files", Secure use reference). `BRANCH_PROTECTION.md` currently lists CODEOWNERS as deliberately not installed for a solo maintainer.

### Acceptance checks to run after applying (A1-A6; none run here)

- **A1.** Push to `omp2`: runs with `buildbuddy-main`, no prompt.
- **A2.** Same-repo PR: the job waits for approval; before approval no step has the key; after approval the run passes.
- **A3.** Fork PR: the job is skipped, the check is green, no approval is requested, no environment appears in the run.
- **A4.** A throwaway same-repo PR that edits `ci.yml` to print `secrets.BB_API_KEY` and `secrets.BUILDBUDDY_ORG_API_KEY` before approval, expecting empty values; delete the PR afterwards.
- **A5.** `workflow_dispatch` on a feature branch: denied by the branch rule; on `omp2`: runs.
- **A6.** A PR run with the PR key: does the probe (`remote-probe`) and `remote-heavy` still pass (this answers U1).

## 7. Residual risk and the exact limit

What remains after D1-D4 is applied:

1. **Approved code can read and use the PR key.** Approval is a code review, not a sandbox. The reviewer must read build-relevant files, not only Rust: `build.rs`, `Cargo.toml`, `Cargo.lock`, `justfile`, `scripts/**`, `.cargo/**`, `rust-toolchain.toml`, `.github/**`. A malicious *transitive crate version* arriving through a lockfile bump cannot be fully reviewed by reading a diff (ADR 0040 H.2 counts 1315 registry packages). This is the main residual.
2. **The bound is the key's privilege, not the approval.** With a read-only PR key (if U1 holds) the worst case is compute burn, exfiltration of that key, and reading cache. Not forging cached results or the shared snapshot, and not reaching the write key. If U1 fails (a read-only key cannot run), the PR key must be write-capable and a poisoned snapshot becomes possible again; the fallback is then to scope it to a dedicated BuildBuddy organization whose cache never gates a verdict, per ADR 0040 H.2, and to treat any suspicious run as a reason to orphan snapshots (below).
3. **One reviewer is one person.** For a solo maintainer, approving your own agent's PR is a conscious release of the key, not independent review. It is still a different security property from today (nothing releases the key at all).
4. **Admin can bypass everything.** Environment protection can be bypassed by administrators by default, and the ruleset's Admin bypass exists. A compromised admin account defeats the design, as it defeats any GitHub-side control; GitHub-side secrets are readable by an admin-written workflow regardless.
5. **The `omp2` write key still meets all merged code.** Every dependency `build.rs` that lands on `omp2` runs on a runner holding the write key (`buildbuddy-main`). This design reduces who reaches it before merge; it does not remove it.
6. **Revocation is not instant.** BuildBuddy: "it can take up to 5 minutes" after deleting a key. Whether a running GitHub job keeps the old secret value is UNVERIFIED (U2). A suspected snapshot poisoning can be answered by changing any runner property in `remote-verify.sh` (every property is part of the snapshot key: `remote-verification-trial.md` "What it took", item 6; BuildBuddy snapshot key docs), which orphans the old snapshots until a fresh seed, plus a reseed with the write key from a clean machine.
7. **Fork PRs get a green check without a Linux unit run.** D4 documents the re-push path; it does not automate Linux coverage for forks.

**The exact limit.** As long as verification goes through `bb remote`, `bb` puts the API key (and a GitHub App token whose scope is UNVERIFIED) into the environment of every process the remote script starts, and writes the key into a bazelrc beside the checkout (ADR 0040 H.2, Findings 4; `remote-verify.sh:9-11`). Any code that runs in that VM, including every third-party build script, can read it and has external network. Nothing in `ci.yml` or in this repository's scripts can prevent that, because the PR controls those files. This design controls **who can cause that run** (settings an admin owns) and **how much the key can do** (a separate, lower-privilege key). It does not make the run key-free. The structural fix is already in ADR 0040: the omp REAPI backend (Slice 1) sends the key only as gRPC metadata ("omp keeps the key in the Keychain or the credential store and injects it as gRPC metadata only", ADR 0040, "Answers to the owner's ten questions", item 6; also Part H.2 first bullets), or self-hosted BuildBuddy executors with an executor key (ADR 0040, "Bring your own runners"). Until one of those replaces `bb remote` for CI, record D8 as an accepted limit.

## 8. Adjacent findings (not part of the design; for the owner)

- **Public vs. private.** ADR 0040, the trial audit and several parked notes describe `blyzer/oh-my-pi` as private. It is public (OBSERVED). Any "private repo is linked to BuildBuddy" reasoning in those documents should be re-read.
- **`pr-labels.yml` uses `pull_request_target`.** It checks out nothing and runs no PR code (its header says so), which is the use GitHub endorses ("label or comment on pull requests from forks", GH docs, [Events that trigger workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request_target)). GitHub's page states a default policy that blocks `pull_request_target` in public repositories and will be enforced on 2026-11-02 for repositories that used the default policy (Securely using pull_request_target). Whether this repository is in that population, and whether the label workflow will stop, is UNVERIFIED (U8); check the Actions policy insights before that date.
- **A self-hosted runner is registered on a public repository.** GitHub says self-hosted runners "should almost never be used for public repositories" because any pull request can compromise the environment (GH docs, Secure use reference, "Hardening for self-hosted runners"). Nothing routes to it today (`MACOS_RUNNER` unset), but a same-repo PR can set `runs-on` to it. `omp2-build-acceleration.md` row 8 records the owner decision pending on this runner.
- **`BB_API_KEY`** (section 6, S4).

## 9. UNVERIFIED items and how to close each

| # | Claim | How to close |
|---|---|---|
| U1 | A read-only BuildBuddy key can still start `bb remote` runs and resume snapshots; and it blocks snapshot save and AC/CAS writes | One run with a read-only key: `remote-probe` and `remote-heavy`; try `--seed` with it and expect a refusal. ADR 0040 line 297 says snapshot reuse needs AC write, which points the other way |
| U2 | An in-flight GitHub job keeps the secret value it started with after the secret is changed | Rotate during a throwaway run |
| U3 | What the org key can read through BuildBuddy's API (other projects' invocations), and whether the org is dedicated (item 30 of `omp2-build-acceleration.md`) | Read the key's permissions and the org's member/project list in the BuildBuddy UI |
| U4 | Current BuildBuddy plan, remaining cache-transfer allowance, and whether usage alerts or hard limits exist | Read the organization usage page (the trial audit says it was not read) |
| U5 | GitHub plan tier of the account (affects environment features if the repo becomes private) | Account settings |
| U6 | Expiry of an unapproved environment wait | Observe on a test PR, or search the Deployments docs |
| U7 | Which workflow definition a `workflow_dispatch` runs when dispatched on a non-default ref | Dispatch a test workflow that differs between `omp2` and a branch |
| U8 | Whether `pr-labels.yml` falls under the 2026-11-02 default-policy enforcement | Read the repository's Actions policy insights |
| U9 | That a job skipped by `if` never evaluates environment protection (W1) | Fork PR in A3 |
| U10 | Scope of the GitHub App token (`REPO_TOKEN`) that `bb remote` injects | Read BuildBuddy's GitHub App permissions (carried over from ADR 0040) |

## 10. Sources

GitHub:
- [Secure use reference](https://docs.github.com/en/actions/reference/security/secure-use)
- [Using secrets in GitHub Actions](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets)
- [Events that trigger workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows) (`pull_request`, `pull_request_target`, `workflow_dispatch`)
- [Securely using pull_request_target](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target)
- [Deployments and environments](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)
- [Managing environments for deployment](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)
- [Managing GitHub Actions settings for a repository](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository)
- [Approving workflow runs from forks](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks)
- [Manually running a workflow](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
- [Workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [Troubleshooting required status checks](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks)
- [Dependabot on GitHub Actions](https://docs.github.com/en/code-security/reference/supply-chain-security/troubleshoot-dependabot/dependabot-on-actions)
- [Managing labels](https://docs.github.com/en/issues/using-labels-and-milestones-to-track-work/managing-labels)
- [Billing for GitHub Actions](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
- GitHub Security Lab, [Preventing pwn requests](https://securitylab.github.com/resources/github-actions-preventing-pwn-requests/)
- [`actions/checkout` README](https://github.com/actions/checkout) (`allow-unsafe-pr-checkout`)

BuildBuddy (vendor documentation, not GitHub semantics):
- [Authentication guide](https://www.buildbuddy.io/docs/guide-auth) (multiple keys, read-only keys, deletion latency)
- [Remote Bazel](https://www.buildbuddy.io/docs/remote-bazel) (snapshot key, secrets handling)

In this repository: `.github/workflows/ci.yml`, `.github/rulesets/omp2-protected.json`, `scripts/remote-verify.sh`, `scripts/remote-verify-runner.sh`, `justfile`, `docs/BRANCH_PROTECTION.md`, `docs/audits/remote-verification-trial.md`, `docs/adr/0040-remote-build-and-test-execution.md`, `docs/research/omp2-build-acceleration.md`.
