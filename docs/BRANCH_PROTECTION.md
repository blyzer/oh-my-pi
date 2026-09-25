# Branch protection and pull request automation

Pull request automation in this repository is GitHub-native: two workflows
label pull requests, and repository settings do the gating. No third-party
review service is installed. This page lists the settings that cannot live in
code; apply them once in the GitHub UI.

## What runs on every pull request

| Workflow | What it does | Gating |
|---|---|---|
| `CI` (`ci.yml`) | Format, licences, runtime-symbol contracts, Linux lint, workspace tests and P1-P8 on macOS, P7 on a Linux PTY; the `CI gate` job summarises them | `CI gate` required (see below) |
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

**Settings > Rules > Rulesets > New branch ruleset**

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

Require exactly one check, named as the checks list of a pull request shows it:

- `CI gate`

`CI` runs on every pull request and every push to `omp2` and `main`. Its
first job, `Detect Rust-relevant changes`, diffs the pull request against its
merge base (a push against the previous head) and decides whether anything
the Rust jobs check has changed: the paths listed in that job's `relevant()`
function. When nothing has, for example in a pull request that touches only
`docs/` outside `docs/py/`, every other job is skipped before it is queued, so
nothing waits for the macOS runner. `CI gate` needs every job and always runs:

| Situation | Jobs | `CI gate` |
|---|---|---|
| Nothing Rust-relevant changed | skipped | passes |
| Rust-relevant change, every job green | succeeded | passes |
| Any job failed, timed out or was cancelled | failure / cancelled | fails |
| A job skipped although Rust-relevant files changed (its `needs` failed) | skipped | fails |
| The change detection itself failed | skipped | fails |

A manual run, a new branch and a push whose previous head is unknown always
run every job. A job added to `ci.yml` must also be added to the gate's
`needs:` list, or the gate will not wait for it.

With the ruleset open in the UI, **Require status checks to pass > Add
checks** takes `CI gate`, with GitHub Actions as its source. Through the API,
the rule is:

```json
{
  "type": "required_status_checks",
  "parameters": {
    "strict_required_status_checks_policy": false,
    "required_status_checks": [
      { "context": "CI gate", "integration_id": 15368 }
    ]
  }
}
```

`integration_id` 15368 is GitHub Actions, so no other app can report a check
of that name. The update replaces the ruleset's whole `rules` array, so keep
every other rule and swap only the status-check rule:

```sh
id=$(gh api repos/blyzer/oh-my-pi/rulesets \
  --jq '.[] | select(.name == "omp2-protected") | .id')
gh api "repos/blyzer/oh-my-pi/rulesets/$id" --jq '{rules: ([.rules[]
  | select(.type != "required_status_checks")] + [{type: "required_status_checks",
  parameters: {strict_required_status_checks_policy: false, required_status_checks:
  [{context: "CI gate", integration_id: 15368}]}}])}' > rules.json
gh api --method PUT "repos/blyzer/oh-my-pi/rulesets/$id" --input rules.json
```

The six job checks (`Rust format`, `License policy and release notices`,
`Runtime symbol and dependency contracts`, `Lint workspace (Linux)`,
`Rust workspace and acceptance proofs`, `Terminal proof P7 (Linux PTY)`) keep
their names, so a ruleset that still requires them keeps working until it is
switched: GitHub counts a skipped job as passing a required check. Remove
them from the ruleset when adding `CI gate`; the gate already covers them,
including the case where one is skipped when it should not be.

Do **not** require `Package macOS`, `PR labels`, or the P8
baseline recorder: they are conditional, informational, or run only after a
merge.

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
