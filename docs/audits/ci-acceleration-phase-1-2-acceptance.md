# CI acceleration Phase 1-2: acceptance check on post-merge `omp2` runs

This audit checks three acceptance criteria of Phases 1 and 2 of
`docs/research/ci-test-acceleration.md` against the five most recent completed, non-cancelled `CI`
runs on `omp2`. It reads GitHub Actions logs and job metadata. It changes no workflow, cache or
setting.

| | |
|---|---|
| **Date** | 2026-10-13 (data collected 16:39-17:12 UTC) |
| **Repository / branch** | `blyzer/oh-my-pi`, `omp2` |
| **Workflow** | `ci.yml` (`CI`), `push` events only |
| **Phase 1-2 change** | PR #259, merge `b9f56065b7` (2026-10-11 19:14:50 UTC), content commit `c57680b7c5` |
| **Tip when sampled** | `2ec8ab920f` (run 38387917998 is the newest completed non-cancelled run) |
| **Tools** | `gh` 2.102.0, `jq`, `unzip`, `sed`, `grep` |

## Verdicts

| # | Criterion | Verdict | Basis |
|---|---|---|---|
| C1 | Every Linux Cargo job gets an exact or prefix `omp2` cache hit in at least 4 of 5 pushes (Rust target cache and CPython cache) | **met** | 5 of 5 pushes. 15 of 15 Rust-target restores were hits (9 by the first restore-key, 6 by the second, none a miss). 15 of 15 CPython restores were exact hits. |
| C2 | No "Unable to reserve cache" | **met** | 0 matches in 61 job logs of the 5 runs. The same scan finds the error in 4 older runs, so it can see the message. |
| C3 | `rust` created at most 20 s after run creation | **not met** | Of the 3 runs where `rust` ran: +14 s (met), +33 s, +99 s. In the other 2 runs `push_verified` skipped `rust` (+11 s, nothing started). Also not met under the alternative reading "job start to first `cargo` step": 38-84 s for every sampled job. |

C1 and C2 hold on every run. C3 fails on 2 of the 3 runs that executed `rust`, and the criterion is
a per-run bound, so it does not hold.

Caveats that bound these verdicts are in "Not verified".

## Criteria as written

The acceptance text is in `docs/research/ci-test-acceleration.md`, section 9, Phase 1 row. Quoted:

> Over n >= 5 pushes: every Linux job gets an exact or prefix `omp2` hit in >= 4/5. No "Unable to
> reserve cache". Repo cache < 9 GB for a week. `rust` created <= 20 s after run creation. PR wall
> median <= baseline - 100 s

The first, second and fourth sentences are the three criteria assigned to this audit (C1, C2, C3).
The other Phase 1 and Phase 2 criteria are out of scope here. Section 9 requires "at least 5 PR runs
where `rust` runs and at least 5 `omp2` pushes". This audit covers the pushes only.

Definitions used:

- **Linux job (C1)**: the three jobs that restore a Cargo `target/` cache: `lint_linux` ("Lint
  workspace (Linux)"), `p9_p10_linux` ("Acceptance proofs P9, P10 and tool sources (Linux)") and
  `p7_linux` ("Terminal proof P7 (Linux PTY)"). `lintx_ratchet` and `licenses` restore other caches
  and are listed under "Adjacent observations".
- **Hit classes**: `Cache hit for: <key>` is an exact hit on the primary key. `Cache hit for
  restore-key: <key>` is a prefix hit. `Cache not found for input keys: ...` is a miss. For a prefix
  hit, "restore-key 1/2" means the first of the step's two `restore-keys` matched (same Cargo
  manifest hash, older commit) and "2/2" means only the manifest-independent prefix matched.
- **C3, primary reading**: the `rust` job's `created_at` minus the run's `created_at`, both from
  `gh api`. This is how section 2 of the research note measures it ("`rust` job created at +65...+134
  s").
- **C3, alternative reading**: the task title says "rust starts <= 20 s". The job's `started_at`
  minus run `created_at`, and the time from the job's `started_at` to the first `cargo` step in its
  log, are also reported.

## Sample

The five runs are the first five rows of this query, taken at 2026-10-13 16:39 UTC, after dropping
cancelled runs:

```
gh run list -R blyzer/oh-my-pi --branch omp2 --workflow ci.yml --event push --limit 60 \
  --json databaseId,headSha,status,conclusion,createdAt \
  -q '[.[]|select(.status=="completed" and .conclusion!="cancelled")]|.[0:5]|.[]|[.databaseId,.headSha[0:10],.conclusion,.createdAt]|@tsv'
```

15 `omp2` push runs created after the oldest sampled run were cancelled by the workflow's
`concurrency` group (`cancel-in-progress: true`) and are skipped. All five sampled runs are
attempt 1 and descend from `c57680b7c5`.

| Run | Commit | Created (UTC) | Conclusion | `rust` | Workflow differences |
|---|---|---|---|---|---|
| 38387917998 | `2ec8ab920f` | 2026-10-13 11:06:19 | success | ran | latest `ci.yml` change `93a54c9f00` |
| 38359085353 | `2f3e06dc6b` | 2026-10-13 06:51:12 | success | skipped by `push_verified` | same |
| 38219050370 | `97a04e25c4` | 2026-10-12 07:40:21 | success | skipped by `push_verified` | same |
| 38188051843 | `7639a32eb3` | 2026-10-12 00:27:12 | success | ran | same |
| 38174361651 | `39df8163db` | 2026-10-11 20:57:32 | **failure** | ran | predates `93a54c9f00` (clippy still inside `rust`); logs show `actions/cache@0057852b...` (v4.3.0) where the others show `@caa29612...` |

Run 38174361651 failed because one P7 test failed in `p7_linux`: `FAIL [21.634s] (13/17) omp-e2e::p7_tsp
tsp_surface_opens_optimistically_and_builds_the_chat_document` (`Summary [113.265s] 17 tests run: 16
passed, 1 failed`). The job's cache restore succeeded before the test ran, so the run stays in the
sample. `push_verified` skipped `rust` in the two runs where it states "Tree ... already passed
'Rust workspace and acceptance proofs' (check run ... on ...); skipping the macOS job".

## Per-run evidence

Line numbers (`L<n>`) are lines of the per-job file `N_<job name>.txt` inside the run's log archive
(`gh api repos/blyzer/oh-my-pi/actions/runs/<id>/logs`), after removing ANSI sequences and CRs. The
commands below regenerate them. In cache keys, 40-hex commit SHAs are shortened to 12 characters and
the 64-hex hashes to 12, each followed by `...`. Offsets ("+N s") are relative to the run's
`created_at`.

#### Run 38387917998

Commit `2ec8ab920f` "Merge pull request #284 from blyzer/feat/trust-slice13-volatile-band"; `push` on `omp2`; created 2026-10-13T11:06:19Z; attempt 1; conclusion **success**; <https://github.com/blyzer/oh-my-pi/actions/runs/38387917998>.

| Criterion | Job (id) | Result | Evidence |
|---|---|---|---|
| C1 Rust target restore | lint_linux (115265403312) | partial (restore-key 1/2) | L289: `Cache hit for restore-key: cargo-lint-linux-Linux-X64-cef9591b4da3…-7122178f0e81…` |
| C1 CPython restore | lint_linux (115265403312) | hit (exact) | L257: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p9_p10_linux (115265403238) | partial (restore-key 1/2) | L354: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-cef9591b4da3…-7122178f0e81…` |
| C1 CPython restore | p9_p10_linux (115265403238) | hit (exact) | L321: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p7_linux (115265403335) | partial (restore-key 1/2) | L354: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-cef9591b4da3…-7122178f0e81…` |
| C1 CPython restore | p7_linux (115265403335) | hit (exact) | L321: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| CPython restore (macOS, not a criterion) | rust (115264783317) | hit (exact) | L254: `Cache hit for: python-macOS-ARM64-5ab4dc93cdfd…` |
| Cargo registry restore (macOS, not a criterion) | rust (115264783317) | partial (restore-key 1/1) | L295: `Cache hit for restore-key: cargo-registry-macOS-ARM64-bdc571e9f23f…` |
| CPython restore (macOS, not a criterion) | lint_macos (115264783437) | hit (exact) | L186: `Cache hit for: python-macOS-ARM64-5ab4dc93cdfd…` |
| Cargo registry restore (macOS, not a criterion) | lint_macos (115264783437) | partial (restore-key 1/1) | L223: `Cache hit for restore-key: cargo-registry-macOS-ARM64-bdc571e9f23f…` |
| C2 No reserve error | all 13 job logs | 0 matches | no line matches `Unable to reserve cache\|ReserveCacheError\|Failed to save: `. Saves that did succeed: lint_linux (115265403312) L36510: `Cache saved with key: cargo-lint-linux-Linux-X64-cef9591b4da3…-2ec8ab920f0f…`; p9_p10_linux (115265403238) L1717: `Cache saved with key: cargo-e2e-linux-Linux-X64-cef9591b4da3…-2ec8ab920f0f…`; rust (115264783317) L15922: `Cache saved with key: cargo-registry-macOS-ARM64-5ac3facadb38…` |
| C3 `rust` created ≤ 20 s | rust (115264783317) | **not met**: created +99 s, started +106 s | job `created_at` 2026-10-13T11:07:58Z, `started_at` 2026-10-13T11:08:05Z; run `created_at` 2026-10-13T11:06:19Z; first job created +88 s; `changes` completed +99 s |
| C3b job start → first `cargo` step | rust (115264783317) | 64 s (first `Compiling`/`Checking` line: 81 s) | L303: `##[group]Run cargo nextest run --profile ci --workspace --no-run --locked`; first unit L310: `   Compiling proc-macro2 v1.0.107` |
| C3b job start → first `cargo` step | lint_macos (115264783437) | 43 s (first `Compiling`/`Checking` line: 61 s) | L229: `##[group]Run cargo clippy --workspace --all-targets --locked`; first unit L236: `   Compiling proc-macro2 v1.0.107` |
| C3b job start → first `cargo` step | lint_linux (115265403312) | 38 s (first `Compiling`/`Checking` line: 44 s) | L369: `##[group]Run cargo clippy --workspace --all-targets --locked --keep-going`; first unit L2416: `    Checking omp-agent v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/agent)` |
| C3b job start → first `cargo` step | p9_p10_linux (115265403238) | 63 s (first `Compiling`/`Checking` line: 70 s) | L442: `##[group]Run cargo nextest run --profile ci -p omp-e2e --locked \`; first unit L506: `   Compiling omp-tools v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/tools)` |
| C3b job start → first `cargo` step | p7_linux (115265403335) | 61 s (first `Compiling`/`Checking` line: 66 s) | L441: `##[group]Run cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked`; first unit L502: `   Compiling omp-tools v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/tools)` |

#### Run 38359085353

Commit `2f3e06dc6b` "Merge pull request #273 from blyzer/feat/trust-slice4-envd"; `push` on `omp2`; created 2026-10-13T06:51:12Z; attempt 1; conclusion **success**; <https://github.com/blyzer/oh-my-pi/actions/runs/38359085353>.

| Criterion | Job (id) | Result | Evidence |
|---|---|---|---|
| C1 Rust target restore | lint_linux (115169304817) | partial (restore-key 1/2) | L288: `Cache hit for restore-key: cargo-lint-linux-Linux-X64-cef9591b4da3…-97a04e25c4e0…` |
| C1 CPython restore | lint_linux (115169304817) | hit (exact) | L257: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p9_p10_linux (115169304799) | partial (restore-key 1/2) | L354: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-cef9591b4da3…-97a04e25c4e0…` |
| C1 CPython restore | p9_p10_linux (115169304799) | hit (exact) | L321: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p7_linux (115169304836) | partial (restore-key 1/2) | L354: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-cef9591b4da3…-97a04e25c4e0…` |
| C1 CPython restore | p7_linux (115169304836) | hit (exact) | L321: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C2 No reserve error | all 11 job logs | 0 matches | no line matches `Unable to reserve cache\|ReserveCacheError\|Failed to save: `. Saves that did succeed: lint_linux (115169304817) L36369: `Cache saved with key: cargo-lint-linux-Linux-X64-cef9591b4da3…-2f3e06dc6bde…`; p9_p10_linux (115169304799) L1710: `Cache saved with key: cargo-e2e-linux-Linux-X64-cef9591b4da3…-2f3e06dc6bde…` |
| C3 `rust` created ≤ 20 s | rust (115168802922) | n/a: skipped by `push_verified`, never started | job `created_at` 2026-10-13T06:51:23Z (+11 s); run `created_at` 2026-10-13T06:51:12Z; first job created +0 s; `changes` completed +11 s |
| C3b job start → first `cargo` step | lint_linux (115169304817) | 42 s (first `Compiling`/`Checking` line: 48 s) | L367: `##[group]Run cargo clippy --workspace --all-targets --locked --keep-going`; first unit L443: `   Compiling omp-proto v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/proto)` |
| C3b job start → first `cargo` step | p9_p10_linux (115169304799) | 84 s (first `Compiling`/`Checking` line: 91 s) | L441: `##[group]Run cargo nextest run --profile ci -p omp-e2e --locked \`; first unit L452: `   Compiling omp-proto v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/proto)` |
| C3b job start → first `cargo` step | p7_linux (115169304836) | 58 s (first `Compiling`/`Checking` line: 64 s) | L440: `##[group]Run cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked`; first unit L448: `   Compiling omp-proto v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/proto)` |

#### Run 38219050370

Commit `97a04e25c4` "Merge pull request #272 from blyzer/feat/trust-slice3-driver"; `push` on `omp2`; created 2026-10-12T07:40:21Z; attempt 1; conclusion **success**; <https://github.com/blyzer/oh-my-pi/actions/runs/38219050370>.

| Criterion | Job (id) | Result | Evidence |
|---|---|---|---|
| C1 Rust target restore | lint_linux (114710994697) | partial (restore-key 2/2) | L288: `Cache hit for restore-key: cargo-lint-linux-Linux-X64-fc114107218d…-7639a32eb361…` |
| C1 CPython restore | lint_linux (114710994697) | hit (exact) | L256: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p9_p10_linux (114710994639) | partial (restore-key 2/2) | L352: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-fc114107218d…-7639a32eb361…` |
| C1 CPython restore | p9_p10_linux (114710994639) | hit (exact) | L320: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p7_linux (114710994643) | partial (restore-key 2/2) | L352: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-fc114107218d…-7639a32eb361…` |
| C1 CPython restore | p7_linux (114710994643) | hit (exact) | L320: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C2 No reserve error | all 11 job logs | 0 matches | no line matches `Unable to reserve cache\|ReserveCacheError\|Failed to save: `. Saves that did succeed: lint_linux (114710994697) L36525: `Cache saved with key: cargo-lint-linux-Linux-X64-cef9591b4da3…-97a04e25c4e0…`; p9_p10_linux (114710994639) L1716: `Cache saved with key: cargo-e2e-linux-Linux-X64-cef9591b4da3…-97a04e25c4e0…` |
| C3 `rust` created ≤ 20 s | rust (114710643179) | n/a: skipped by `push_verified`, never started | job `created_at` 2026-10-12T07:40:32Z (+11 s); run `created_at` 2026-10-12T07:40:21Z; first job created +1 s; `changes` completed +11 s |
| C3b job start → first `cargo` step | lint_linux (114710994697) | 40 s (first `Compiling`/`Checking` line: 45 s) | L368: `##[group]Run cargo clippy --workspace --all-targets --locked --keep-going`; first unit L6075: `    Checking omp-ext v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/ext)` |
| C3b job start → first `cargo` step | p9_p10_linux (114710994639) | 52 s (first `Compiling`/`Checking` line: 59 s) | L434: `##[group]Run cargo nextest run --profile ci -p omp-e2e --locked \`; first unit L669: `   Compiling omp-ext v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/ext)` |
| C3b job start → first `cargo` step | p7_linux (114710994643) | 79 s (first `Compiling`/`Checking` line: 84 s) | L439: `##[group]Run cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked`; first unit L671: `   Compiling omp-ext v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/ext)` |

#### Run 38188051843

Commit `7639a32eb3` "Merge pull request #270 from blyzer/test/p7-tsp-hermetic-config"; `push` on `omp2`; created 2026-10-12T00:27:12Z; attempt 1; conclusion **success**; <https://github.com/blyzer/oh-my-pi/actions/runs/38188051843>.

| Criterion | Job (id) | Result | Evidence |
|---|---|---|---|
| C1 Rust target restore | lint_linux (114614991753) | partial (restore-key 1/2) | L288: `Cache hit for restore-key: cargo-lint-linux-Linux-X64-fc114107218d…-9ddcb349a37e…` |
| C1 CPython restore | lint_linux (114614991753) | hit (exact) | L256: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p9_p10_linux (114614991866) | partial (restore-key 1/2) | L353: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-fc114107218d…-9ddcb349a37e…` |
| C1 CPython restore | p9_p10_linux (114614991866) | hit (exact) | L320: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p7_linux (114614991851) | partial (restore-key 1/2) | L352: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-fc114107218d…-9ddcb349a37e…` |
| C1 CPython restore | p7_linux (114614991851) | hit (exact) | L320: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| CPython restore (macOS, not a criterion) | rust (114614696582) | hit (exact) | L254: `Cache hit for: python-macOS-ARM64-5ab4dc93cdfd…` |
| Cargo registry restore (macOS, not a criterion) | rust (114614696582) | hit (exact) | L294: `Cache hit for: cargo-registry-macOS-ARM64-bdc571e9f23f…` |
| CPython restore (macOS, not a criterion) | lint_macos (114614696545) | hit (exact) | L186: `Cache hit for: python-macOS-ARM64-5ab4dc93cdfd…` |
| Cargo registry restore (macOS, not a criterion) | lint_macos (114614696545) | hit (exact) | L224: `Cache hit for: cargo-registry-macOS-ARM64-bdc571e9f23f…` |
| C2 No reserve error | all 13 job logs | 0 matches | no line matches `Unable to reserve cache\|ReserveCacheError\|Failed to save: `. Saves that did succeed: lint_linux (114614991753) L36505: `Cache saved with key: cargo-lint-linux-Linux-X64-fc114107218d…-7639a32eb361…`; p9_p10_linux (114614991866) L1722: `Cache saved with key: cargo-e2e-linux-Linux-X64-fc114107218d…-7639a32eb361…` |
| C3 `rust` created ≤ 20 s | rust (114614696582) | **not met**: created +33 s, started +40 s | job `created_at` 2026-10-12T00:27:45Z, `started_at` 2026-10-12T00:27:52Z; run `created_at` 2026-10-12T00:27:12Z; first job created +24 s; `changes` completed +33 s |
| C3b job start → first `cargo` step | rust (114614696582) | 52 s (first `Compiling`/`Checking` line: 65 s) | L303: `##[group]Run cargo nextest run --profile ci --workspace --no-run --locked`; first unit L310: `   Compiling proc-macro2 v1.0.107` |
| C3b job start → first `cargo` step | lint_macos (114614696545) | 48 s (first `Compiling`/`Checking` line: 63 s) | L231: `##[group]Run cargo clippy --workspace --all-targets --locked`; first unit L238: `   Compiling proc-macro2 v1.0.107` |
| C3b job start → first `cargo` step | lint_linux (114614991753) | 42 s (first `Compiling`/`Checking` line: 48 s) | L369: `##[group]Run cargo clippy --workspace --all-targets --locked --keep-going`; first unit L4047: `   Compiling omp-tools v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/tools)` |
| C3b job start → first `cargo` step | p9_p10_linux (114614991866) | 56 s (first `Compiling`/`Checking` line: 63 s) | L438: `##[group]Run cargo nextest run --profile ci -p omp-e2e --locked \`; first unit L502: `   Compiling omp-tools v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/tools)` |
| C3b job start → first `cargo` step | p7_linux (114614991851) | 53 s (first `Compiling`/`Checking` line: 60 s) | L434: `##[group]Run cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked`; first unit L495: `   Compiling omp-tools v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/tools)` |

#### Run 38174361651

Commit `39df8163db` "Merge pull request #261 from blyzer/feat/omp-exec"; `push` on `omp2`; created 2026-10-11T20:57:32Z; attempt 1; conclusion **failure**; <https://github.com/blyzer/oh-my-pi/actions/runs/38174361651>.

| Criterion | Job (id) | Result | Evidence |
|---|---|---|---|
| C1 Rust target restore | lint_linux (114573746243) | partial (restore-key 2/2) | L293: `Cache hit for restore-key: cargo-lint-linux-Linux-X64-52aae571b0d0…-bd77fac2a7c5…` |
| C1 CPython restore | lint_linux (114573746243) | hit (exact) | L258: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p9_p10_linux (114573746200) | partial (restore-key 2/2) | L358: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-52aae571b0d0…-bd77fac2a7c5…` |
| C1 CPython restore | p9_p10_linux (114573746200) | hit (exact) | L322: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| C1 Rust target restore | p7_linux (114573746207) | partial (restore-key 2/2) | L358: `Cache hit for restore-key: cargo-e2e-linux-Linux-X64-52aae571b0d0…-bd77fac2a7c5…` |
| C1 CPython restore | p7_linux (114573746207) | hit (exact) | L322: `Cache hit for: python-Linux-X64-5ab4dc93cdfd…` |
| CPython restore (macOS, not a criterion) | rust (114573499524) | hit (exact) | L256: `Cache hit for: python-macOS-ARM64-5ab4dc93cdfd…` |
| Cargo registry restore (macOS, not a criterion) | rust (114573499524) | partial (restore-key 1/1) | L299: `Cache hit for restore-key: cargo-registry-macOS-ARM64-0faeb68c390c…` |
| CPython restore (macOS, not a criterion) | lint_macos (114573499602) | hit (exact) | L189: `Cache hit for: python-macOS-ARM64-5ab4dc93cdfd…` |
| Cargo registry restore (macOS, not a criterion) | lint_macos (114573499602) | partial (restore-key 1/1) | L230: `Cache hit for restore-key: cargo-registry-macOS-ARM64-0faeb68c390c…` |
| C2 No reserve error | all 13 job logs | 0 matches | no line matches `Unable to reserve cache\|ReserveCacheError\|Failed to save: `. Saves that did succeed: lint_linux (114573746243) L36446: `Cache saved with key: cargo-lint-linux-Linux-X64-fc114107218d…-39df8163dbf5…`; p9_p10_linux (114573746200) L1731: `Cache saved with key: cargo-e2e-linux-Linux-X64-fc114107218d…-39df8163dbf5…`; rust (114573499524) L52022: `Cache saved with key: cargo-registry-macOS-ARM64-bdc571e9f23f…` |
| C3 `rust` created ≤ 20 s | rust (114573499524) | **met**: created +14 s, started +24 s | job `created_at` 2026-10-11T20:57:46Z, `started_at` 2026-10-11T20:57:56Z; run `created_at` 2026-10-11T20:57:32Z; first job created +2 s; `changes` completed +13 s |
| C3b job start → first `cargo` step | rust (114573499524) | 72 s (first `Compiling`/`Checking` line: 90 s) | L307: `##[group]Run cargo clippy --workspace --all-targets --locked`; first unit L314: `   Compiling proc-macro2 v1.0.107` |
| C3b job start → first `cargo` step | lint_macos (114573499602) | 50 s (first `Compiling`/`Checking` line: 66 s) | L237: `##[group]Run cargo clippy --workspace --all-targets --locked`; first unit L244: `   Compiling proc-macro2 v1.0.107` |
| C3b job start → first `cargo` step | lint_linux (114573746243) | 40 s (first `Compiling`/`Checking` line: 46 s) | L375: `##[group]Run cargo clippy --workspace --all-targets --locked --keep-going`; first unit L382: `    Checking omp-core v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/core)` |
| C3b job start → first `cargo` step | p9_p10_linux (114573746200) | 84 s (first `Compiling`/`Checking` line: 92 s) | L444: `##[group]Run cargo nextest run --profile ci -p omp-e2e --locked \`; first unit L508: `   Compiling omp-tools v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/tools)` |
| C3b job start → first `cargo` step | p7_linux (114573746207) | 66 s (first `Compiling`/`Checking` line: 73 s) | L450: `##[group]Run cargo nextest run --profile ci -p omp-e2e --test p7_tui --test p7_tsp --locked`; first unit L511: `   Compiling omp-tools v2.0.0 (/home/runner/work/oh-my-pi/oh-my-pi/crates/tools)` |

## Findings

### C1: cache hits

Rust target cache, per Linux job and run (restore-key class, exact hit never expected because a push
key ends in the new commit SHA):

| Run | `lint_linux` | `p9_p10_linux` | `p7_linux` | CPython (all three) |
|---|---|---|---|---|
| 38387917998 | restore-key 1/2 | restore-key 1/2 | restore-key 1/2 | exact |
| 38359085353 | restore-key 1/2 | restore-key 1/2 | restore-key 1/2 | exact |
| 38219050370 | restore-key 2/2 | restore-key 2/2 | restore-key 2/2 | exact |
| 38188051843 | restore-key 1/2 | restore-key 1/2 | restore-key 1/2 | exact |
| 38174361651 | restore-key 2/2 | restore-key 2/2 | restore-key 2/2 | exact |

- 5 of 5 pushes had a hit in every Linux job, against the required 4 of 5.
- "2/2" occurred in the two runs whose Cargo manifest hash differed from the cache entry's: run
  38219050370 (`Cargo.lock`, `crates/driver/Cargo.toml` changed against the restored commit
  `7639a32eb3`) and run 38174361651 (`Cargo.lock`, `Cargo.toml`, `crates/exec/Cargo.toml` changed
  against `bd77fac2a7`). That is an exact `git diff --name-only` result and matches the key change
  `52aae571b0d0...` to `fc114107218d...` to `cef9591b4da3...`.
- Restored sizes: `cargo-lint-linux` about 617 MB, `cargo-e2e-linux` about 1434 MB
  (`Cache Size:` lines in each log).
- Writers: only `p9_p10_linux` saved `cargo-e2e-linux-*` and only `lint_linux` saved
  `cargo-lint-linux-*`, in all 5 runs (a "Cache saved with key" line). `p7_linux` has only a
  restore step in `ci.yml` and logged no save, so the single-writer design of Diff 3 held.

[INFERENCE] "`omp2` hit": the log does not print the ref of the entry it restored. On a push to
`omp2` (also the default branch), GitHub restores only from that ref or its base, and the current
cache listing shows every `cargo-e2e-linux-*`, `cargo-lint-linux-*` and `python-*` entry on
`refs/heads/omp2`. The only entries on other refs are `cargo-registry-macOS-*` on four
`refs/pull/*/merge` refs.

### C2: no reserve error

The scan `grep -E 'Unable to reserve cache|ReserveCacheError|Failed to save: '` over every job log
of the run found 0 lines in each of the five runs (13, 11, 11, 13, 13 logs = 61 logs).

A broader, case-insensitive scan for `reserve` (excluding `preserve`) finds only unrelated text: a
Rust doc comment in a compiler diagnostic ("band below twelve reserved rows") and, in run
38174361651 only, `ld64.lld: cannot export hidden symbol _Py_qsbr_reserve`, one of the `ld64.lld: cannot export hidden symbol` linker
warnings ("1374 more similar warnings" per link) in the macOS `rust` job (L38167 and L51652 of
`8_Rust workspace and acceptance proofs.txt`). Neither is a cache reserve error. Test names that
contain "reserve" are excluded from the count above.

Positive control: the same scan on four pre-Phase-1 runs finds the error in two job logs of each:
38127067908, 38120425785, 38110897963 and 38064207482. First line of each, for the key
`python-Linux-X64-5ab4dc93cdfd...`: `Failed to save: Unable to reserve cache with key
python-Linux-X64-5ab4dc93cdfd54c4dc1e322e65a269b14ce6d3223b2968bacc805fe70854f8b2, another job may
be creating this cache.` (run 38127067908, 2026-10-11T09:12:11Z). Their commits `506b7c143e`,
`bc1e12d21b`, `aedb705882` and `9ef5cde7dd` do not contain `c57680b7c5`. Run 38106690990 was
scanned too and had no match.

Limit of this evidence: that error needs two jobs to write one key in the same window, which needs
a cache miss. In the five sampled runs the CPython cache never missed: 15 of 15 Linux restores and
the 6 macOS restores in the three runs where `rust` ran were exact hits, and the logs say "Cache hit
occurred on the primary key python-..., not saving cache." So the single-writer path of Diff 4 was
not exercised by a miss. The criterion is met; its cause (the writer change) is not demonstrated by
these five runs.

### C3: `rust` created at most 20 s after run creation

| Run | Run created -> first job created | First job -> `rust` created | `rust` created | `rust` started | Outcome |
|---|---|---|---|---|---|
| 38387917998 | 88 s | 11 s | +99 s | +106 s | ran; **not met** |
| 38359085353 | 0 s | 11 s | +11 s | skipped | skipped by `push_verified` |
| 38219050370 | 1 s | 10 s | +11 s | skipped | skipped by `push_verified` |
| 38188051843 | 24 s | 9 s | +33 s | +40 s | ran; **not met** |
| 38174361651 | 2 s | 12 s | +14 s | +24 s | ran; met by created time, started +24 s |

- The mechanism of Diff 1 holds. `rust` was created 0-1 s after `changes` and `push_verified` both
  finished, and in the same second as `Rust format` (`format` created +99, +11, +11, +33, +14 s).
  It no longer waits on `format`.
- The 20 s bound fails because of the time between run creation and the first job being created
  (88 s and 24 s in two runs), which is before any workflow job runs, plus about 9-12 s for
  `changes`. [UNVERIFIED] why GitHub took 88 s in run 38387917998: the logs and the jobs API carry
  no queue reason. The lowest value that the current `needs: [changes, push_verified]` shape can
  reach is the 9-12 s column.
- The two skipped runs meet 20 s only literally (+11 s). No `rust` job started, so they are
  excluded from the verdict.

Alternative reading, job start to first `cargo` step (first `Run cargo ...` group in the job log):

| Job | Run 38387917998 | 38359085353 | 38219050370 | 38188051843 | 38174361651 |
|---|---|---|---|---|---|
| `rust` (macOS) | 64 s | skipped | skipped | 52 s | 72 s (clippy) |
| `lint_linux` | 38 s | 42 s | 40 s | 42 s | 40 s |
| `p9_p10_linux` | 63 s | 84 s | 52 s | 56 s | 84 s |
| `p7_linux` | 61 s | 58 s | 79 s | 53 s | 66 s |

None is at or below 20 s (minimum 38 s). Reading the first `Compiling`/`Checking` line instead adds
5-18 s. For `rust` in run 38387917998 the 64 s are setup steps from the job's step list: set up job
1 s, checkout 5 s, toolchain 8 s, "Install native build tools" 30 s, nextest 1 s, `macos-build-state`
12 s, "Cache Cargo" 6 s. `lint_macos` (not a Phase 1 criterion) shows 43-50 s.

## Not verified

- **The ref of restored entries** (C1). The log has no ref. See the [INFERENCE] above.
- **Why run creation to first job creation took 88 s** (C3, run 38387917998).
- **The single-writer fix for the CPython key under a miss** (C2). No miss occurred in the sample.
- **Sample size.** The five runs are one `omp2` stretch of three days. One of them failed on an
  unrelated test, and one predates the `ci.yml` change `93a54c9f00`. They are all the completed,
  non-cancelled runs available; there is no larger post-merge set to draw from yet.
- **PR runs.** Section 9 also asks for 5 PR runs. Not sampled.
- **A pre-merge baseline for C3.** The research note gives `rust` created at +65...+134 s (Sample R,
  n = 10). It is not re-derived here.

## Adjacent observations (not verdicts)

- **Repository cache size.** The Phase 1 criterion "< 9 GB for a week" was not assigned. At
  2026-10-13 17:12 UTC `gh api repos/blyzer/oh-my-pi/actions/cache/usage` returned
  `active_caches_size_in_bytes` = 9,747,888,705 (9.75 GB; 9.08 GiB) in 16 entries. The cache listing
  (`.../actions/caches`) returned 18 entries with the same total. A week of history cannot be read
  from the API, so the criterion is **UNVERIFIED**, but the current value is above 9 GB in both
  units. Of it, 9,202,936,392 B is on `refs/heads/omp2`.
- **CPython and tool caches.** `lintx` and `compliance-tools` restored as exact hits in all five
  runs (`lintx_ratchet` L141/L143 and `licenses` L141/L143 of their job files).
- **Phase 2 effect on `omp-proto`.** `Compiling omp-proto` lines per Linux job against the files
  changed between the restored cache commit and the run's commit (`git diff --name-only`):

  | Run | Restored commit | `crates/proto` files changed | `omp-proto` units rebuilt |
  |---|---|---|---|
  | 38387917998 | `7122178f0e` | 0 | 0 in all three jobs |
  | 38359085353 | `97a04e25c4` | 1 (`document.proto`) | 1 in all three jobs |
  | 38219050370 | `7639a32eb3` | 0 | 0 in all three jobs |
  | 38188051843 | `9ddcb349a3` | 0 | 0 in all three jobs |
  | 38174361651 | `bd77fac2a7` | 0 | `lint_linux` 1, others 0 (`Cargo.toml` and `Cargo.lock` changed) |

  This is consistent with the Phase 2 `source-mtimes` action but is not the Phase 2 criterion,
  which concerns a PR that changes no crate.
- **Run 38168459418** (`bd77fac2a7`, the first completed non-cancelled `omp2` run that contains
  `c57680b7c5`; sixth by recency, so not in the sample) shows the first run after the macOS key
  change: `cargo-registry-macOS` missed in `rust` and `lint_macos` (L297 and L228, `Cache not
  found for input keys: cargo-registry-macOS-ARM64-0faeb68c390c...`), its `rust` job started
  +539 s after run creation and its `lint_macos` job +986 s. Both Linux caches hit (restore-key
  1/2).

## Commands

Everything above comes from these commands. `gh` must be authenticated for `blyzer/oh-my-pi`.

1. Pick runs (see "Sample"). Run metadata and jobs, with step timestamps:

   ```
   gh api repos/blyzer/oh-my-pi/actions/runs/<id>
   gh api repos/blyzer/oh-my-pi/actions/runs/<id>/jobs --paginate
   ```

2. Logs. `gh api .../logs` returns a ZIP with one `N_<job name>.txt` per job. The per-job endpoint
   `gh api --allow-escape-sequences repos/blyzer/oh-my-pi/actions/jobs/<job id>/logs` returns the
   same text, but `gh` refuses it without that flag because the logs contain escape sequences.
   `gh run view <id> -R blyzer/oh-my-pi --log` also works and gave the same `Cache hit for` count for
   run 38359085353 (8 lines), but it prefixes lines with job and step names and does not give
   per-job line numbers, so line numbers in this audit are from the ZIP.

3. Strip ANSI and CR:

   ```
   sed -E 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\r//g' <file>
   ```

4. Reserve-error scan over all job logs of a run (ANSI does not hide the text, but strip first):

   ```
   grep -lE 'Unable to reserve cache|ReserveCacheError|Failed to save: ' <extracted dir>/*.txt
   ```

5. Cache result lines per job (what `repro.sh` prints):

   ```
   grep -nE 'Cache hit for(: | restore-key: )|Cache not found|Unable to reserve' <clean file>
   ```

   To classify a prefix hit as 1/2 or 2/2, compare the printed key with the `restore-keys:` block of
   the same step's `##[group]Run actions/cache...` header.

6. Cache scope and size:

   ```
   gh api repos/blyzer/oh-my-pi/actions/caches --paginate -q '.actions_caches[]|[.ref,.key[0:60],.size_in_bytes,.created_at]|@tsv'
   gh api repos/blyzer/oh-my-pi/actions/cache/usage
   ```

7. Commit ancestry and diffs (in a clone with `omp2`):

   ```
   git merge-base --is-ancestor c57680b7c5 <sha>
   git diff --name-only <restored cache commit> <run commit> -- crates/proto
   ```

`repro.sh` (save it anywhere), run as `./repro.sh 38387917998 38359085353 38219050370 38188051843 38174361651`:

```bash
#!/usr/bin/env bash
# Reproduce the CI-acceleration Phase 1-2 acceptance evidence for one or more
# CI run ids on blyzer/oh-my-pi. Needs: gh (authenticated), jq, unzip, sed, grep.
#   usage: repro.sh <run-id>...
set -euo pipefail
REPO=blyzer/oh-my-pi
ANSI='s/\x1b\[[0-9;?]*[A-Za-z]//g; s/\r//g'
secs() { jq -rn --arg t "$1" '$t | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601'; }

for run in "$@"; do
  work=$(mktemp -d)
  gh api "repos/$REPO/actions/runs/$run" >"$work/run.json"
  gh api "repos/$REPO/actions/runs/$run/jobs" --paginate >"$work/jobs.json"
  gh api "repos/$REPO/actions/runs/$run/logs" >"$work/logs.zip"
  unzip -q -o "$work/logs.zip" -d "$work/logs"
  created=$(jq -r .created_at "$work/run.json")
  echo "=== run $run sha=$(jq -r .head_sha "$work/run.json" | cut -c1-10) event=$(jq -r .event "$work/run.json") created=$created conclusion=$(jq -r .conclusion "$work/run.json")"

  # (3) reserve errors: every job log of the run.
  logs=$(find "$work/logs" -maxdepth 1 -name '*.txt' | wc -l | tr -d ' ')
  hits=$( (grep -lE 'Unable to reserve cache|ReserveCacheError|Failed to save: ' "$work"/logs/*.txt 2>/dev/null || true) | wc -l | tr -d ' ')
  echo "reserve-scan: job logs=$logs matching=$hits"

  # (1) cache restores and (2) rust timing, per job of interest.
  jq -r '.jobs[] | select(.conclusion != "skipped")
         | select(.name | test("^(Rust workspace|Lint workspace|Acceptance proofs P9|Terminal proof P7)"))
         | [.id, .name, .started_at] | @tsv' "$work/jobs.json" |
  while IFS=$'\t' read -r id name started; do
    log=$(find "$work/logs" -maxdepth 1 -name "*_${name}.txt")
    clean="$work/clean-$id.txt"
    sed -E "$ANSI" "$log" >"$clean"
    echo "--- job $id $name (runner start $started)"
    # Restore results: "Cache hit for: <key>" = exact (primary key),
    # "Cache hit for restore-key: <key>" = partial (prefix), "Cache not found" = miss.
    grep -nE 'Cache hit for(: | restore-key: )|Cache not found|Unable to reserve' "$clean" |
      sed -E 's/^([0-9]+):[^ ]+ /  L\1 /' || true
    # Start of the first cargo invocation and of the first compile line.
    first=$(grep -m1 -nE '^[^ ]+ ##\[group\]Run cargo (clippy|nextest run|test|build|check)' "$clean" || true)
    if [ -n "$first" ]; then
      ts=$(echo "$first" | sed -E 's/^[0-9]+:([^ ]+) .*/\1/')
      echo "  first cargo: $(echo "$first" | sed -E 's/^([0-9]+):[^ ]+ /L\1 /') (+$(( $(secs "$ts") - $(secs "$started") )) s after job start)"
    fi
    unit=$(grep -m1 -nE '^[^ ]+ +(Compiling|Checking) ' "$clean" || true)
    if [ -n "$unit" ]; then
      ts=$(echo "$unit" | sed -E 's/^[0-9]+:([^ ]+) .*/\1/')
      echo "  first unit:  $(echo "$unit" | sed -E 's/^([0-9]+):[^ ]+ +/L\1 /') (+$(( $(secs "$ts") - $(secs "$started") )) s)"
    fi
  done

  # rust job creation offset (the criterion as the research note words it).
  jq -r --arg c "$created" '.jobs[] | select(.name | test("^(Detect|Rust workspace)"))
        | [.name, .created_at, .conclusion] | @tsv' "$work/jobs.json" |
  while IFS=$'\t' read -r n c r; do
    echo "job-created: $n +$(( $(secs "$c") - $(secs "$created") )) s ($r)"
  done
done
```

Its output was compared with this audit for runs 38387917998, 38359085353, 38219050370, 38188051843
and 38174361651: reserve-scan counts, hit lines with line numbers, first-`cargo` offsets and the
`rust` creation offsets equal the tables above (read side by side for all five runs). It does not classify prefix hits as 1/2 or 2/2 (step 5 does), and the
reason `rust` was skipped is in the `Pushed tree already verified` job log, not in the script.
