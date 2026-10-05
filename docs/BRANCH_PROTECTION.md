# Branch protection and pull request automation

Pull request automation in this repository is GitHub-native: two workflows
label pull requests, and repository settings do the gating. No third-party
review service is installed. This page lists the settings that cannot live in
code; apply them once in the GitHub UI.

## What runs on every pull request

| Workflow | What it does | Gating |
|---|---|---|
| `CI` (`ci.yml`) | Format, licences, runtime-symbol contracts, Linux lint, workspace tests and P1-P8 on macOS, P7 on a Linux PTY, P9, P10, P11 and `tool_sources` on Linux | Required (see below) |
| `Package macOS` (`package-macos.yml`) | Release build, package, install smoke; only when the workflow or its scripts change | Not required |
| `PR labels` (`pr-labels.yml`) | `area/*`, `kind/*`, `risk/*` labels from `.github/labeler.yml`, then `size/xs` .. `size/xl` labels and one comment on `size/xl` | Informational |

The label workflow runs on `pull_request_target`, so it can label pull
requests from forks. It checks out nothing and runs no code from the pull
request. Its size job runs after the path job, because `actions/labeler`
replaces the whole label set and would drop a size label added beside it. A
`workflow_dispatch` input relabels an existing pull request, for example after
`.github/labeler.yml` or the size buckets change:

```sh
gh workflow run pr-labels.yml -f pr=<number>
```

## Ruleset on `omp2`

The ruleset below is also kept as importable JSON in
[`.github/rulesets/omp2-protected.json`](../.github/rulesets/omp2-protected.json).
To apply it: **Settings > Rules > Rulesets > New ruleset > Import a ruleset**, choose that file, review
it and save. The JSON targets `refs/heads/omp2` only, lets the repository **Admin** role bypass it
(`bypass_mode: always`, so the owner can still push to `omp2` directly and merge with failing or
pending checks when a runner is down), allows only merge commits, requires the eight status checks
listed below and resolved review threads, and leaves "up to date" and linear history off. Keep the file
and the table below in sync.

To apply it by hand instead: **Settings > Rules > Rulesets > New branch ruleset**

- **Name**: `omp2-protected`
- **Enforcement status**: Active
- **Target branches**: the default branch (`omp2`); add `main` too while it
  still receives pushes.

| Rule | Setting | Why |
|---|---|---|
| Restrict deletions | on | No accidental `git push origin :omp2`. |
| Block force pushes | on | `omp2` history is shared; feature branches stay force-pushable. |
| Require linear history | **off** | Pull requests land as merge commits. |
| Require a pull request before merging | on | Every change goes through CI on a pull request. |
| - Required approvals | 0 while there is one maintainer; 1 after that | GitHub does not count an author's own approval. |
| - Dismiss stale approvals on new commits | on | A push after approval is reviewed again. |
| Require status checks to pass | on | See the list below. |
| - Require branches to be up to date | off | The base moves often; CI already runs on the merge ref. |
| Require conversation resolution | on | Open review threads block the merge. |

### Required status checks

Use the job names exactly as the checks list of a pull request shows them:

- `Rust format`
- `License policy and release notices`
- `Runtime symbol and dependency contracts`
- `Lint workspace (Linux)`
- `Rust workspace and acceptance proofs`
- `Terminal proof P7 (Linux PTY)`
- `Acceptance proofs P9, P10 and tool sources (Linux)`
- `Error-formatting ratchet (lintx)`

`Acceptance proofs P9, P10 and tool sources (Linux)` runs on a GitHub-hosted
runner, not the self-hosted Mac, so requiring it does not lengthen the macOS
queue. Add it to the ruleset once it has reported green on a pull request;
existing required checks keep their names.

`Error-formatting ratchet (lintx)` (ADR 0035) runs on a GitHub-hosted runner as
well and is required (it reported green on a pull request before it was added).
It is a job of `ci.yml` like the others, so it is skipped, and passes, on pull
requests that change no CI path.

Do **not** require `Package macOS`, `PR labels`, or the P8
baseline recorder: they are conditional, informational, or run only after a
merge.

On the push that a merge makes to `omp2`, `Pushed tree already verified` checks
whether the merge commit's tree is identical to the merged pull request's head
and whether that head's `Rust workspace and acceptance proofs` run passed. If
both are true, the macOS job is skipped rather than run a second time on the
one runner. With "Require branches to be up to date" off, the trees match only
when the pull request had the current base merged in before it was merged;
otherwise the push runs the full job as before. It runs only on pushes, so do
not require it either.

CI is one workflow, `ci.yml`, and it runs on every pull request. It has no
`paths` filter on `pull_request`: a workflow that a path filter keeps out of a
pull request reports nothing, so its required checks would wait forever. A job
named `changes` lists the pull request's files through the API and matches them
against `.github/ci-paths.txt` (one glob per line, `**` crosses directories).
When nothing matches, every other job is skipped, and GitHub counts a skipped
required check as passing. A docs-only pull request therefore gets all eight
checks without running them; one that touches a CI path runs them for real.

Why one workflow and not a second one that reports the same names for skipped
pull requests: a pull request that touched both CI paths and other files (code
plus an ADR, say) triggered both workflows, so each check name was reported
twice, an instant pass and the real result, and `Pushed tree already verified`
reads a check run by name. One workflow reports each name once.

Things that keep this honest:

- The decision fails closed. If `changes` errors, its output is empty and the
  jobs run. Only an explicit `ci=false` skips them.
- `push` in `ci.yml` still uses a native `paths` list (a push has no required
  checks). It must equal `.github/ci-paths.txt`, `pull_request` must carry no
  path filter, and every required check in the ruleset must be a `name:` of a
  `ci.yml` job. `just check-ci-paths` (`scripts/check-ci-paths.py`) verifies all
  three; run it whenever `ci.yml`, `ci-paths.txt` or the ruleset changes, and
  rename a required check in `ci.yml` and the ruleset together.
- `.github/ci-paths.txt` and `.github/workflows/ci.yml` are themselves in the
  list, so editing the gate runs CI.

`Package macOS`, `PR labels` and the P8 recorder are not required and are not
gated by `changes`.

If the ruleset requires `Rust workspace and acceptance proofs` while the
`MACOS_RUNNER` variable points that job at a self-hosted Mac, an outage of that
runner leaves the check pending and blocks every merge that touches CI paths.
The escape hatch is the ruleset's admin bypass: merge as an admin, or unset
`MACOS_RUNNER` so the job falls back to the hosted `macos-15` runner.

Measured on the hosted runner (run 37329841934, `macos-15-arm64`): the job builds and
links the whole workspace in about 52 minutes (against about 15 on a warm self-hosted
Mac) and ends with about 21 GiB of the 43 GiB free, so disk is not the limit. Three tests
that pass on the self-hosted Mac failed there, so the hosted fallback is installable but
not yet green: `docserver::actor::tests::active_permissions_require_current_revision_and_preserve_head`,
`sandbox_proxy::tests::over_limit_rejections_never_stall_accept` and
`omp-shell-builtins` `executes_unix_behavior_corpus` (`ps` returned nothing). The doctests and
the acceptance proofs did not run after that failure.

## Auto-merge

**Settings > General > Pull Requests**

- Allow auto-merge: on
- Allow merge commits: on (the merge method this repository uses)
- Automatically delete head branches: on

Then, on a pull request:

```sh
gh pr merge <number> --merge --auto
```

GitHub merges it the moment every required check is green. `size/xs` and
`kind/docs` pull requests are the natural candidates.

## Labels

| Family | Examples | Use |
|---|---|---|
| `area/*` | `area/agent`, `area/session`, `area/tools`, `area/ci` | Which subsystem the change is in; follows the crate layout in `AGENTS.md`. |
| `kind/*` | `kind/tests`, `kind/docs`, `kind/dependencies` | What sort of change it is. |
| `risk/*` | `risk/journal-format`, `risk/wire-protocol`, `risk/toolchain`, `risk/python-linkage` | A surface where a mistake outlives the pull request. |
| `size/*` | `size/xs` .. `size/xl`, `size/intrinsic` | Expected review effort. Add `size/intrinsic` to acknowledge a size that cannot be split. |

Useful searches:

```
is:pr is:open -label:kind/docs          behavioural pull requests only
is:pr is:open label:risk/journal-format pull requests that touch the durable journal
is:pr is:open label:size/xl             large pull requests worth splitting
```

## Deliberately not installed

**gitStream (and similar review-routing services).** The GitHub App reports a
`gitStream.cm` check on every pull request as *Skipped* because the repository
has no `.cm/` configuration. The labels above cover what it would add here.
Remove the app from this repository under **Settings > GitHub Apps > gitStream >
Configure > Repository access** (organization or personal installation,
wherever it was installed).

**CODEOWNERS.** With one maintainer, every review routes to the same person.
Add `.github/CODEOWNERS` when a second maintainer owns part of the tree, and
then turn on *Require review from Code Owners* in the ruleset.
