# Build and Execution Ecosystem Survey (OMP² agents building other projects)

Status: discovery/research only. No files outside this document were changed, no builds ran, and no implementation is authorized. Every recommendation below needs owner review, including any follow-up to ADR 0040.
Date: 2026-10-11

## Scope

This survey covers build systems, CI orchestrators, runners, remote-execution servers, caches and pipeline engines that an OMP² agent might drive when it builds **other** projects. It does not cover OMP²'s own build. The goal is to decide which adapters and backends to build first, and to check that the ADR 0040 execution contract can absorb them without changing its core.

### Verification key

Each row and claim carries one of these tags. All of them are kept from the source survey.

| Tag | Meaning |
|---|---|
| **V** | Checked on 2026-10-11 by web search against the vendor's official docs, repo, changelog or license file. The source is named in the row or in [Sources](#sources). |
| **T** | Not verified. Comes from training knowledge. |
| **OBS** | Seen directly in the local OMP² repo. A file path is given. |
| **[INFERENCE]** | The author's own reasoning. |

**Pricing rule.** The only figures given are ones the vendor publishes, and each is tagged V. Every other price reads "see vendor; parameterize". No prices are invented.

### Anchor in OMP²

The survey reuses ADR 0040's execution contract. ADR 0040 is written for OMP²'s own builds, but its contract is the right core for other projects too.

- **Contract.** `BuildSystemAdapter → ExecutionPlan → ExecutionRouter → ExecutionBackend`, with the verbs `start / next_event / cancel / detach / reattach` and a `BackendCapabilities` struct. Source: OBS `docs/adr/0040-remote-build-and-test-execution.md` Parts A–B, lines 427–567. The adapter list is at `:436`, and `CargoJust` is the single adapter in slice 1 (`:449`).
- **Amendment.** OBS `0040…md:1467-1500` states: "BuildBuddy-specific headers, properties, cancellation RPCs and snapshot behavior MUST stay behind the BuildBuddy backend."
- **Measured evidence.** OBS `docs/audits/remote-verification-trial.md:105-125`, plus Slice 0c at `:148-176`, found two things. Raw-REAPI runners were never recycled (Slices 0b and 0c). Input timestamps are not preserved (`:114`).

The OBS line numbers come from the `omp2` checkout used for the survey. Spot checks against this tree matched the cited content: the cancel table at ADR `:870`, `CancelExecutions` at `:258-261` and `:459`, and the audit at `:114-115` and `:148-176`.

---

## 1. Verification status and vendor status changes

Web search was available. Every inventory row in §2 is tagged V or T.

These vendor status changes affect the recommendations. All are **V**.

| Technology | Change | Source |
|---|---|---|
| Earthly | The OSS project is "no longer actively maintained". Earthly Cloud and Satellites stopped on **2025-07-16**. | earthly.dev blog "shutting-down-earthfiles-cloud"; GitHub README |
| BuildJet | Shutdown announced 2026-02-06. Jobs stopped running on **2026-03-31**. | buildjet.com blog "we-are-shutting-down" |
| GitHub Actions dispatch | Since **2026-02-19**, `POST …/dispatches` with `return_run_details: true` returns the run ID (HTTP 200). `gh` v2.87.0 supports this. | github.blog changelog |
| GitHub Actions self-hosted fee | Since **2026-03-01**, self-hosted runner jobs in private repos cost **$0.002/min**. Public repos and GHES are exempt. | github.com/resources/insights/2026-pricing-changes-for-github-actions |
| Depot CI | API and CLI went GA on 2026-06-04. `depot ci run` uploads a patch of local changes. | depot.dev changelog and CLI reference |
| NativeLink | Licensed FSL-1.1-Apache-2.0, which converts to Apache-2.0 two years after each release. The metrics and persistent-worker modules are BSL-1.1. | GitHub LICENSE; nativelink.com/license |
| Semaphore | Community Edition has been Apache-2.0 since 2025-02-26. `ee/` has a separate license. | semaphore.io |
| GoCD | The May 2025 status page describes a "very small" maintainer group and EOL dependencies. | gocd.org project-status |
| Bamboo | Server support ended 2024-02-15. Data Center continues past the 2029-03-28 Atlassian Data Center EOL. | atlassian.com |
| Drone | 2.x is maintained on the `drone` branch. Its successor, Harness Open Source (formerly Gitness, Apache-2.0), was still in beta in 2025. | readme.drone.io FAQ |
| Nx | The self-hosted remote cache is free, through an OpenAPI spec or official S3/GCS/Azure/FS plugins (2025-03-31). Powerpack stopped being sold standalone on 2025-12-10. | nx.dev blog |

---

## 2. Inventory (62 technologies)

**Role codes**

| Code | Role |
|---|---|
| BS | build system |
| CI | CI orchestrator |
| RUN | runner |
| RE | remote-exec protocol implementation |
| RC | remote cache |
| CC | compiler cache |
| AS | artifact store |
| PE | pipeline engine |

**Priority levels**

| Level | Meaning |
|---|---|
| P0 | Initial implementation, or its conformance target |
| P1 | Roadmap |
| P2 | Later, or on demand |
| none | No adapter (see §9) |

The count is 62, well above the requested 40. "Local process" is a mechanism, not a product, so it is not counted. It is still the baseline backend everywhere below.

### 2.1 Remote execution and cache

| # | Name | Role(s) | REAPI? | Submit / status / cancel / logs / artifacts surface | Auth | Deploy | License | Maintenance (2026-10) | Priority and reason | Verif |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | BuildBuddy | RE, RC, BES UI, CI (Workflows), remote runner (Run API) | **Server: yes.** CAS, AC, Execute, WaitExecution. No LRO `CancelOperation` found; uses the proprietary `CancelExecutions` (OBS ADR 0040 :1082-1097) | REAPI gRPC. CLI `bb remote` / `bb execute`. Run API: `POST /api/v1/Run`, `GetInvocation`, `GetLog`. `bb remote` streams logs | `x-buildbuddy-api-key` header; `bb login` | SaaS + self-host | MIT outside `enterprise/`; proprietary inside | Active (bb 5.0.492 used 2026-10, OBS audit :13) | **P0**: first concrete host for generic REAPI conformance. The Run API persistent workspace is P1 | V (buildbuddy.io/docs/remote-bazel, LICENSE) + OBS |
| 2 | EngFlow | RE, RC, BES | Server: yes | REAPI. The `ef` CLI runs commands remotely, uploads and downloads CAS, and manages secrets | mTLS, or JWT service-account tokens; `ef login` (browser) | Managed SaaS / customer cloud | Proprietary | Active | **P1**: second REAPI conformance target. Needs no adapter of its own | V (docs.engflow.com auth, ef, secrets) |
| 3 | NativeLink | RE, RC (Rust) | Server: yes | REAPI only | Depends on configuration | OSS self-host + Cloud | FSL-1.1-Apache-2.0; BSL-1.1 modules | Active | **P1**: self-hosted REAPI test target. Review the license before bundling | V (LICENSE); auth T |
| 4 | Buildbarn | RE, CAS/AC storage (Go) | Server: yes | REAPI only; no vendor API | mTLS/JWT, configurable | OSS self-host | Apache-2.0 | Active (167 commits in a year; release 2026-08-14) | **P0**: spec-pure REAPI reference for hermetic conformance tests (docker-compose deployment) | V (GitHub); auth T |
| 5 | BuildGrid | RE, Remote Workers API (Python) | Server: yes | REAPI + RWAPI | Configuration | OSS self-host | Apache-2.0 | Active (0.8.14, 2026-10-06) | **P2**: niche; covered by the generic REAPI backend | V maintenance; license T |
| 6 | Develocity | RC (Gradle, Maven, Bazel AC+CAS over gRPC), Build Scans/BES; Test Distribution (T) | **Cache only.** Bazel `--remote_cache=grpcs://`. Not RE | Build Scan UI/API | Access keys | SaaS + self-host | Proprietary | Active (2026.3 docs) | **P2**: configured in the build tool's config. OMP² records only the scan URL | V (docs.develocity.ai/bazel/2026.3); Test Distribution T |
| 7 | Nx Cloud | RC, distributed task execution (Nx Agents), CI insights | No | Nx Cloud API. The self-hosted cache is an OpenAPI contract (`NX_SELF_HOSTED_REMOTE_CACHE_SERVER`) | Bearer `NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN` | SaaS + free self-hosted cache | Proprietary SaaS; Nx MIT | Active | **P2**: pass cache settings through env only | V (nx.dev/docs/kb/self-hosted-caching); Agents T |
| 8 | Turborepo Remote Cache | RC protocol (HTTP v8 OpenAPI) | No | `GET/PUT/HEAD /artifacts/{hash}`, `POST /artifacts`, `POST /artifacts/events`, `GET /artifacts/status` | Bearer | Spec + community servers | Turborepo MIT (T) | Active | **P2**: env passthrough | V (turborepo.dev openapi) |
| 9 | Vercel Remote Cache | Hosted RC (Turborepo protocol) | No | Same as #8 | Vercel token | SaaS | Proprietary | Active | **P2**: same as #8 | T (V that #8 is the protocol) |
| 10 | sccache | CC + distributed compilation | No | Env vars; `--show-stats`. Backends: disk, S3, GCS, Redis, GHA; multi-level chain | Backend credentials | OSS | Apache-2.0 (T) | Active | **P1**: one way to honor Cargo/C++ cache hints (`RUSTC_WRAPPER`). No provider adapter | V (sccache docs/Configuration.md, MultiLevel.md) |
| 11 | ccache | CC | No | `remote_storage` (http, redis, file; helpers); `read-only`, `shards` | Backend credentials | OSS | GPL-3.0+ (T) | Active (4.12 manual) | **P2**: CMake `*_COMPILER_LAUNCHER` hint | V (ccache manual) |

### 2.2 Build systems

| # | Name | Role(s) | REAPI? | Machine surface | Auth | Deploy | License | Maintenance | Priority and reason | Verif |
|---|---|---|---|---|---|---|---|---|---|---|
| 12 | Bazel | BS, REAPI client, BEP producer | **Client** | `bazel build/test`. BEP via `--build_event_json_file` (`testResult`/`testSummary`) or BES upload. Cancel = SIGINT to the client | Delegated (`--remote_header`, credential helper) | OSS | Apache-2.0 (T) | Active | **P1**: thin adapter. Place it locally, parse BEP, never wrap Bazel's actions (consistent with OBS ADR 0040 Part A.4) | V (bazel.build BEP) |
| 13 | Buck2 | BS, REAPI client | Client | `.buckconfig [buck2_re_client]`; build report JSON (T) | Configuration headers | OSS | MIT/Apache-2.0 (T) | Active | **P2**: same pattern as Bazel | V (buck2 remote_execution.md) |
| 14 | Pants | BS, REAPI client (RE + cache) | Client | `remote_execution_address`, `remote_store_address`; JUnit XML (T) | Headers/tokens | OSS | Apache-2.0 (T) | Active (2.29 docs) | **P2** | V (pantsbuild docs) |
| 15 | Please | BS, REAPI v2.1 client + Remote Asset API | Client | `.plzconfig [Remote] URL/Instance/Secure/Platform` | Configuration | OSS | Apache-2.0 (T) | Active (T) | **P2** | V (please.build/remote_builds.html) |
| 16 | Cargo | BS and package manager (Rust) | No | `--message-format=json` (stable compiler messages). libtest `--format json` is **unstable** (nightly `-Z unstable-options`). nextest JUnit (T) | Registry tokens | OSS | MIT/Apache-2.0 | Active | **P0**: initial adapter #1 | V (rustc book; RFC 3558 still open) |
| 17 | Maven | BS (JVM) | No | Surefire JUnit XML (T). Build Cache Extension (Maven 3.9+): remote HTTP GET/PUT/HEAD | `settings.xml` credentials | OSS | Apache-2.0 | Active | **P1** | V (maven build-cache extension) |
| 18 | Gradle | BS (JVM, Kotlin, Android) | No | `HttpBuildCache` `GET/PUT /cache/<key>`; JUnit XML (T); `--scan` | Cache credentials | OSS | Apache-2.0 | Active | **P1**: first JVM adapter | V (docs.gradle.org build_cache) |
| 19 | Go toolchain | BS (Go) | No | `go test -json` (stable test2json event stream). `go build -json` (Go 1.24, T). `GOCACHEPROG` external cache hook (T) | GOPROXY/GOPRIVATE credentials | OSS | BSD-3 | Active | **P0**: initial adapter #2 | V (go.dev cmd/test2json) |
| 20 | MSBuild | Build engine (.NET, C++) | No | Binlog `-bl` (T); SARIF via `ErrorLog` (T) | NuGet | OSS | MIT | Active | **P1** (through dotnet) | T |
| 21 | dotnet CLI | Build/test driver (.NET) | No | MTP reporters: `--report-trx`, `--report-junit` (extension packages) | NuGet | OSS | MIT | Active | **P1** | V (learn.microsoft.com MTP reports) |
| 22 | CMake | Meta-build (C/C++) | No | File API JSON (T); `ctest --output-junit` (T) | — | OSS | BSD-3 | Active | **P2** | T |
| 23 | Ninja | Low-level executor | No | `.ninja_log`; `-t compdb` (T) | — | OSS | Apache-2.0 | Active | **P2**: reached through CMake/Meson | T |
| 24 | Meson | Meta-build | No | Introspection JSON; JUnit test log (T) | — | OSS | Apache-2.0 | Active | **P2** | T |
| 25 | npm | Package manager, script runner (JS) | No | `--json` on some commands (T); test output belongs to the test runner | npm token | OSS | Artistic-2.0 | Active | **P1** (JS family) | T |
| 26 | pnpm | Package manager, workspace runner | No | `--reporter ndjson` | npm token | OSS | MIT | Active | **P1**: entry point for the JS adapter | V (reporter); license T |
| 27 | Bun | Runtime, package manager, test runner | No | `bun test --reporter=junit --reporter-outfile=…` | npm token | OSS | MIT | Active | **P1** (JS family) | V (bun.sh reporters) |
| 28 | uv | Python package/project manager + `uv_build` backend (pure Python only) | No | `uv build`; tests via pytest `--junitxml` (T) | Index credentials | OSS | MIT/Apache-2.0 (T) | Active | **P1**: Python adapter | V (docs.astral.sh build-backend) |

### 2.3 CI/CD

| # | Name | Role(s) | REAPI? | Submit / status / cancel / logs / artifacts | Auth | Deploy | License | Maintenance | Priority and reason | Verif |
|---|---|---|---|---|---|---|---|---|---|---|
| 29 | GitHub Actions | CI, hosted RUN, cache, AS | No | Dispatch returns the run ID with `return_run_details:true`. Run/job status. `POST …/runs/{id}/cancel` and `/force-cancel` (202/409). Job log download (T). Artifacts API. `gh` CLI | PAT, GitHub App, `GITHUB_TOKEN` (needs Actions: write to cancel); OIDC to clouds (T) | SaaS + GHES | Proprietary (runner MIT, T) | Active; self-hosted fee V | **P0**: initial provider #2 | V (docs.github.com workflow-runs; changelog 2026-02-19) |
| 30 | GitLab CI/CD | CI | No | Create pipeline (T); cancel pipeline/job (`?force=true`); `GET …/jobs/:id/trace`; artifacts as archive or single file | PAT, project token, CI job token | SaaS + self-host | CE MIT / EE proprietary (T) | Active | **P1**: first non-GitHub CI after Buildkite | V (docs.gitlab.com api/jobs, pipelines, job_artifacts) |
| 31 | Jenkins | CI | No | `buildWithParameters`, `/api/json`, `progressiveText` log, `stop`, artifacts (all T) | API token + crumb (T) | Self-host | MIT | Active | **P2** | T |
| 32 | Azure Pipelines | CI | No | `POST pipelines/{id}/runs`; run states include `canceling`; cancel via `PATCH build {status:cancelling}`; logs (text, line ranges); artifacts via a `signedContent` URL | PAT / Entra; `vso.build_execute` | SaaS + Azure DevOps Server | Proprietary | Active | **P1** | V (learn.microsoft.com REST 7.1) |
| 33 | CircleCI | CI | No | v2 trigger (the legacy endpoint does not work for GitHub App/GitLab projects); workflow/job cancel (202); artifacts. **Logs only via v1.1 `output_url`.** v3 artifacts | `Circle-Token` | SaaS + Server | Proprietary | Active | **P2** | V (circleci.com/docs/api/v2) |
| 34 | Buildkite | CI (bring your own agents) | No | REST create build (T); `PUT …/builds/{n}/cancel`; job log (`text/plain`, HEAD for size, suffix byte-range tail); artifacts (redirect to a short-lived URL); GraphQL; `bk` CLI; MCP server | Scoped bearer tokens (`read_build_logs`, `read_artifacts`) | SaaS control plane + self-hosted agents | Proprietary SaaS; agent MIT (T) | Active | **P1**: the independent CI with the most complete API | V (buildkite.com/docs/apis) |
| 35 | TeamCity | CI | No | `POST /app/rest/buildQueue`; cancel queued or running builds (`readdIntoQueue`); `downloadBuildLog.html?buildId=` | Bearer | Self-host + Cloud | Proprietary | Active | **P2** | V (jetbrains.com TeamCity REST) |
| 36 | Bitbucket Pipelines | CI | No | Trigger, stop, step logs (T) | API tokens (T) | SaaS | Proprietary | Active | **P2** | T |
| 37 | AWS CodeBuild | CI build service | No | `StartBuild` → ID; `BatchGetBuilds` (1–100); `StopBuild` (only attempts a stop); logs through CloudWatch `GetLogEvents` (stream created in PROVISIONING) | IAM SigV4 | AWS SaaS | Proprietary | Active | **P2**: candidate "cloud batch" backend | V (docs.aws.amazon.com CodeBuild API) |
| 38 | AWS CodePipeline | Release orchestrator | No | `StartPipelineExecution` / `StopPipelineExecution` (T) | IAM | AWS SaaS | Proprietary | Active | **none**: release orchestration, not build execution | T |
| 39 | Google Cloud Build | CI build service | No | `builds.create` (T); `builds/{id}:cancel` returns a Build; logs at `gs://<logsBucket>/log-<id>.txt` | OAuth/IAM (`cloudbuild.builds.update`) | GCP SaaS | Proprietary | Active | **P2** | V (docs.cloud.google.com cancel) |
| 40 | Harness CI | CI/CD platform | No | Platform API (T) | API keys (T) | SaaS + self-managed; Harness Open Source | Proprietary; Harness OSS Apache-2.0 | Active; OSS in beta in 2025 | **P2** | V (OSS status); API T |
| 41 | Semaphore | CI | No | API (T) | Token | SaaS + CE self-host | CE Apache-2.0 + `ee/` | Active | **P2** | V (license) |
| 42 | Travis CI | CI | No | API v3 (T) | Token | SaaS + Server | Proprietary | Operating; repositioned 2025-06 | **none** | V (status) |
| 43 | Drone | CI (containers) | No | REST (T) | Token | Self-host | CE Apache-2.0 / EE Polyform (T) | Maintenance mode; successor is Harness OSS | **none** | V (status) |
| 44 | Woodpecker CI | CI (Drone fork) | No | `POST /repos/{id}/pipelines/{n}/cancel`; step logs; **live log stream** `GET /stream/logs/…` | Bearer PAT | Self-host | Apache-2.0 (T) | Active (v3 API). A 2025 cancel bug was tracked as #5003 | **P2** | V (woodpecker-ci.org/api) |
| 45 | Tekton | Kubernetes CI primitives | No | Create a PipelineRun; `spec.status`: `Cancelled` / `CancelledRunFinally` / `StoppedRunFinally`; pod logs (T) | Kubernetes RBAC | OSS | Apache-2.0 | Active | **P2** | V (tektoncd pipelineruns.md) |
| 46 | Argo Workflows | Kubernetes workflow engine | No | `argo stop` (runs exit handlers) vs `argo terminate` (skips them); server API (T) | Kubernetes RBAC / SSO | OSS | Apache-2.0 | Active (release-4.1 docs) | **P2** | V (argo docs) |
| 47 | GoCD | CD server | No | REST (T) | Token | Self-host | Apache-2.0 | Small-maintainer risk | **none** | V (status) |
| 48 | Bamboo | CI (Atlassian Data Center) | No | REST (T) | PAT | Self-host DC | Proprietary | DC continues | **none** | V (status) |
| 49 | Bitrise | Mobile CI | No | v0.1 trigger (`hook_info.type:bitrise`), abort (with reason, can skip notifications), `/log`, artifacts with `expiring_download_url` | Token | SaaS | Proprietary | Active | **P2** (on demand) | V (devcenter.bitrise.io) |
| 50 | Codemagic | Mobile CI | No | `POST /builds` → buildId; `POST /builds/:id/cancel` (208 if already done); artifacts plus a time-limited public URL | `x-auth-token` | SaaS | Proprietary | Active | **P2** (on demand) | V (docs.codemagic.io) |

### 2.4 Runners

All of these except Depot CI are transparent behind GitHub Actions `runs-on` labels.

| # | Name | Role(s) | REAPI? | Surface | Auth | Deploy | License | Maintenance | Priority and reason | Verif |
|---|---|---|---|---|---|---|---|---|---|---|
| 51 | Depot | RUN (GitHub Actions), remote BuildKit container builds, **Depot CI** (its own CI) | No | Container Builds API: `CreateBuild`, `GetBuild`, `GetBuildSteps`, `GetBuildStepLogs`, BuildKit endpoint. CI API: `Run` → runId, status, cancel, logs. `depot ci run` uploads a local patch | Bearer (`api.depot.dev`) | SaaS | Proprietary | Active | **P1**: Depot CI is the only CI in this survey that natively submits a working tree. Its GHA runners need no adapter | V (depot.dev SDK, CI reference, changelogs) |
| 52 | BuildJet | RUN | — | — | — | — | — | **Shut down 2026-03-31** | **none** | V |
| 53 | Namespace | RUN, NVMe cache volumes, remote Docker builders | No | Runner profiles (`runs-on: namespace-profile-…`); `nscloud-cache-action` | Token/OIDC (T) | SaaS | Proprietary | Active | **none** (GHA label) | V (namespace.so blog) |
| 54 | Blacksmith | RUN, sticky disks (snapshot clone → commit) | No | Actions (`setup-docker-builder`, `stickydisk`) | GitHub App | SaaS | Proprietary | Active | **none** (GHA label) | V (useblacksmith GitHub) |
| 55 | RunsOn | Ephemeral self-hosted GHA runners on EC2 in your own account | No | Labels | AWS IAM + license key (free for non-commercial use; 15-day trial) | Self-host in AWS | MIT repo + commercial license key | Active | **none** | V (runs-on.com, GitHub) |
| 56 | Actuated | Firecracker microVM runners (GHA, GitLab), one job per VM | No | Labels; SaaS control plane | GitHub App | Bring-your-own hosts + SaaS control plane | Proprietary | Active; roadmap still says "pilot" | **none** | V (docs.actuated.com) |
| 57 | Actions Runner Controller (ARC) | Kubernetes autoscaling runner scale sets (ephemeral, JIT token, one job per pod) | No | Helm `gha-runner-scale-set` | GitHub App / PAT | OSS self-host | Apache-2.0 | Active (GitHub) | **none** | V (actions/actions-runner-controller) |

### 2.5 Pipeline engines, command runners, toolchain providers

| # | Name | Role(s) | REAPI? | Surface | Auth | Deploy | License | Maintenance | Priority and reason | Verif |
|---|---|---|---|---|---|---|---|---|---|---|
| 58 | Dagger | PE (BuildKit-based, SDKs) + Dagger Cloud traces | No | `dagger call`; `dagger cloud traces list/status/view`; engine API (T) | Cloud token | OSS + SaaS traces | Apache-2.0 | Active (Cloud v3, 2025-01) | **P2**: run as an opaque local command; possible future backend | V (GitHub, dagger.io blog, CLI reference) |
| 59 | Earthly | PE (Earthfile) | No | `earthly` CLI | — | OSS | MPL-2.0 (T) | **Not actively maintained**; Cloud shut down | **none** | V (status) |
| 60 | Task | Command runner (Taskfile) | No | `task --list-all --json` (T) | — | OSS | MIT (T) | Active | **none**: opaque recipe | T |
| 61 | Just | Command runner | No | `just --dump --dump-format json` (T) | — | OSS | CC0-1.0 (T) | Active | **none**: opaque recipe. OMP²'s own `CargoJust` adapter already treats `just` this way (OBS ADR 0040 :440) | T |
| 62 | Nix | Package/build system, binary cache (substituters), remote builders | No (own protocol) | `nix build --json`, `nix log` (T) | Substituter signing keys | OSS | LGPL-2.1 (T) | Active | **P2**: toolchain provider (resolves `ToolchainId`), not an execution backend | T |

---

## 3. Role classification notes (do not conflate)

Many products in §2 have more than one role code. Each role maps to a different place in the ADR 0040 contract. Collapsing the roles produces wrong adapters. The notes below draw on the role codes in §2, the cache semantics in §4.5 and the exclusions in §9. Where a note is a design judgment, it is tagged [INFERENCE].

| Role | What it is | Where it lands in OMP² | Do not confuse with |
|---|---|---|---|
| **BS** (build system) | Turns a project into build and test commands. Owns dependency graphs and incremental state. | A `BuildSystemAdapter`. It emits `ExecutionRequest`s (argv, env allowlist, output paths, toolchain, named caches, needs) and parses reports. It never chooses a backend. | **REAPI clients.** Bazel, Buck2, Pants and Please *contain* a REAPI client. They are still BS adapters, placed Local, and OMP² must never re-route their actions (§6.4, §9). |
| **CI** (CI orchestrator) | Runs a *declared* workflow or pipeline at a *ref*. Reports a conclusion, not an exit code. | An `ExecutionBackend` with `SubmitShape::DeclaredEntry` and `InputTransport::GitRef` (§6.2). | **Runners.** A CI chooses *where* through labels or pools. Runner vendors are not CI backends. |
| **RUN** (runner) | Machines that execute CI jobs. | Not modeled. They sit behind `runs-on` labels of the CI adapter (Depot runners, Namespace, Blacksmith, RunsOn, Actuated, ARC). | **Depot CI**, which is a separate CI product with its own `Patch` submit (#51). |
| **RE** (remote-exec server) | Executes content-addressed Actions over REAPI v2. | One generic REAPI `ExecutionBackend`. Vendor differences are profile data plus capability-gated modules (OBS ADR Part A.3). | **Remote caches.** Develocity speaks the REAPI cache protocol but does not execute (#6). |
| **RC** (remote cache, tool-owned) | Task or output caches keyed by the tool's own hash (Turborepo, Nx, Gradle, Maven extension, Develocity). | Env/config passthrough only. OMP² records links and never reads or writes entries (§4.5, §9). | **REAPI action cache**, which is backend-level and keyed by a fully declared Action. |
| **CC** (compiler cache) | Wraps compiler invocations (sccache, ccache). | A way to *realize* a cache hint through wrapper env (`RUSTC_WRAPPER`, `*_COMPILER_LAUNCHER`). Not a provider. | **Incremental state** (`target/`), which is mutable and stale-prone. |
| **AS** (artifact store) | Storage for job outputs (GHA artifacts, Buildkite redirect URLs, S3/GCS). | `ExecutionResult.artifacts`. Fetched outputs are re-addressed into the CAS. | **CAS**, which is a self-verifying transport, not a "store" with retention policy. |
| **PE** (pipeline engine) | Programmable pipelines (Dagger, Earthly). | An opaque Local command when supported (Dagger, P2). | **CI orchestrators**, which add triggers, refs and hosted execution. |
| Command runners (Task, Just) | Named recipes that call real tools. | Opaque. The underlying tool's adapter interprets the commands. | Build systems. |
| Toolchain providers (Nix) | Resolve toolchains and binary caches. | Resolve `ToolchainId`. Not an execution backend. | Remote execution, even though Nix has remote builders. |

---

## 4. Matrices

### 4.1 Provider capability matrix

**Cancel semantics**

| Term | Meaning |
|---|---|
| **Immediate** | The provider guarantees the process stops, or OMP² kills it itself. |
| **Best-effort** | An asynchronous request; completion is not guaranteed. |
| **None** | No cancel API. |

Entries are V unless tagged otherwise.

| Mechanism | Submit unit | Stream logs | Status | Cancel | Artifacts | Cache | Persistent workspace | Isolation |
|---|---|---|---|---|---|---|---|---|
| Local process (OMP² envd `ExecHost`) | argv | Live | Push | **Immediate**: TERM, 250 ms grace, then KILL to the pgid (OBS ADR 0040 Part I) | Local paths | Whatever the tool has | Yes (the checkout itself) | Sandbox optional (OBS) |
| SSH host, persistent workdir | argv | Live (PTY/pipe) | Push | Immediate while the session is alive (kill the remote pgid); otherwise best-effort [INFERENCE] | scp/rsync | Tool-native dirs | **Yes, mutable** | Whatever the host gives (usually none) |
| Generic REAPI (Buildbarn, BuildGrid, NativeLink, EngFlow, BuildBuddy) | Action = Command + input root in CAS | **Post-hoc** (stdout/stderr digests). Live streaming is not in the standard (BuildBuddy live is UNVERIFIED, OBS ADR :1093) | Push (Execute/WaitExecution stream) | **Best-effort or None.** LRO `CancelOperation` is optional, and no capability field announces it. BuildBuddy uses a vendor RPC | **Typed** (`output_paths` → digests) | **Action cache + CAS** | **None** in raw REAPI (OBS audit Slices 0b/0c: never recycled) | Server-defined (BuildBuddy: Firecracker, OBS audit :14) |
| BuildBuddy Run API (`bb remote`) | Script or Bazel command over a git ref + mirrored local diff | Live (CLI) | Poll `GetInvocation` | Best-effort (invocation cancel) | Invocation outputs | AC/CAS + runner home | **Snapshot keyed by every platform property** (OBS audit :89-97) | Firecracker VM |
| EngFlow `ef` run | Command | T | T | T | CAS download | AC/CAS | T | Server-defined |
| GitHub Actions | **Pre-declared workflow at a ref** (`workflow_dispatch` inputs, max 25, V changelog 2025-12-04) | Post-job download (T); live only in the UI (T) | Poll (run ID returned since 2026-02) | Best-effort (`cancel`); hard (`force-cancel`, which bypasses `always()`) | Archive (zip) API | `actions/cache` (advisory, keyed) | None on hosted runners; runner vendors add it | Fresh VM per job on hosted (T) |
| GitLab CI | Pipeline at a ref + variables (T) | Trace endpoint, incremental while running (T) | Poll | Best-effort (`force=true` for stuck jobs) | Archive or single file | `cache:` keys (T) | Runner-dependent | Runner-dependent |
| Jenkins | Parameterized job (T) | `progressiveText` (T) | Poll | Best-effort (T) | Per build | Plugin-dependent | Agent workspace often persistent (T) | Agent-dependent |
| Azure Pipelines | Pipeline run at a ref | Logs API with line ranges | Poll | Best-effort (`cancelling` state) | Signed URL | Cache task (T) | Self-hosted agents persist (T) | Hosted VM (T) |
| CircleCI | Pipeline at a branch + parameters | Post-step (v1.1 `output_url`) | Poll | Best-effort (202) | Paths + URLs | `save_cache` (T) | None (T) | Container/VM (T) |
| Buildkite | Build at a commit + env (T) | **Ranged tail polling** | Poll / webhooks (T) | Best-effort | Redirect URLs | Plugins / agent-local | Self-hosted agents can persist | Agent-defined |
| TeamCity | Queued build config | Full log download | Poll | Best-effort (queued builds: abort only) | Per build (T) | Agent-local (T) | Agent checkout persists (T) | Agent-defined |
| Bitbucket Pipelines | Pipeline at a ref (T) | Step logs (T) | Poll | Stop (T) | T | `caches:` (T) | None (T) | Container (T) |
| AWS CodeBuild | Project + overrides | CloudWatch, near-live (T) | Poll (`BatchGetBuilds`) | Best-effort ("attempts to stop") | S3 | S3/local cache (T) | None (T) | Container (T) |
| Google Cloud Build | Build spec | GCS log object | Poll / LRO | Best-effort | GCS | T | None (T) | Container (T) |
| Harness / Semaphore / Travis | Pipeline at a ref (T) | T | Poll | T | T | T | T | T |
| Woodpecker | Pipeline at a ref | **Live stream endpoint** | Poll | Best-effort (2025 bug #5003) | Plugins (T) | Volumes (T) | Agent-dependent | Container |
| Tekton | PipelineRun CR | Pod logs, follow (T) | Kubernetes watch | **3 modes** (`Cancelled`, `CancelledRunFinally`, `StoppedRunFinally`) | Workspaces/results (T) | PVC (T) | PVC workspace (T) | Pod |
| Argo Workflows | Workflow CR | Pod logs (T) | Kubernetes watch | Stop (graceful) vs terminate (hard) | Artifact repository (T) | T | PVC (T) | Pod |
| Bitrise / Codemagic | Workflow at a branch/tag | Post-build log / T | Poll | Best-effort (abort; Codemagic returns 208 if done) | Expiring URLs | T | None (T) | macOS/Linux VM (T) |
| Depot CI | GHA-compatible workflow + **uploaded local patch** | `--follow` | Poll / `status --wait` | API cancel | T | Depot cache (T) | T | VM (T) |
| Dagger | Function call on the local engine | Live (engine) | Local | Immediate (local process) | Exported paths | BuildKit layers + cache volumes (T) | Engine cache volumes (T) | Containers |
| Nix remote builders | Derivation (T) | `nix log` (T) | Local | Local kill (T) | Store paths | Binary cache | Store persists | Build sandbox (T) |

### 4.2 Build-system compatibility matrix

| Build system | Ecosystems | Machine-readable build events | Test results format | BEP | JUnit | SARIF | Native remote cache | REAPI | OMP² stance |
|---|---|---|---|---|---|---|---|---|---|
| Cargo | Rust | **cargo JSON** (`--message-format=json`, stable) | libtest JSON **unstable**; nextest JUnit (T) | — | via nextest (T) | via third-party clippy-sarif (T) | None (sccache wrapper) | No | P0 adapter |
| Go | Go | `go build -json` (T) | **test2json** (stable NDJSON: start/run/pass/fail/skip/output, `FailedBuild`) | — | Converters (T) | `govulncheck -format sarif` (T) | Local GOCACHE; `GOCACHEPROG` (T) | No | P0 adapter |
| Bazel | Polyglot | **BEP** (JSON file or BES gRPC) | `testResult` / `testSummary` + `test.xml` | **Yes** | `test.xml` | Rule-dependent | Yes (gRPC/HTTP) | **Client** | P1, never wrap |
| Buck2 | Polyglot | Build report JSON (T) | T | No (T) | T | — | Via RE | **Client** | P2 |
| Pants | Python, JVM, Go, shell | T | JUnit (T) | — | Yes (T) | — | Yes | **Client** | P2 |
| Please | Polyglot | T | XML results (T) | — | T | — | Via RE | **Client** (v2.1) | P2 |
| Maven | JVM | Console only (T) | Surefire JUnit XML (T) | — | Yes | Via plugins (T) | Build Cache Extension (HTTP) | No | P1 |
| Gradle | JVM, Kotlin, Android | Build Scan (Develocity) | JUnit XML (T) | — | Yes | Via plugins (T) | HttpBuildCache | No | P1 |
| MSBuild / dotnet | .NET, C++ | Binlog (T) | **TRX / JUnit** (MTP reporters) | — | Yes | Compiler `ErrorLog` (T) | No | No | P1 |
| CMake / Ninja / Meson | C, C++ | File API / `.ninja_log` / introspection JSON (T) | ctest JUnit / Meson JUnit (T) | — | Yes (T) | Analyzer-dependent | ccache/sccache launcher | No (recc can bridge, T) | P2 |
| npm / pnpm / Bun | JS, TS | pnpm `ndjson` reporter | Comes from the test runner (Bun JUnit V; vitest/jest JUnit T) | — | Yes (runner) | ESLint SARIF formatter (T) | Turborepo / Nx caches | No | P1 |
| uv | Python | — | pytest `--junitxml` (T) | — | Yes | Ruff/Bandit SARIF (T) | — | No | P1 |

**Takeaway.** Only Go (test2json) and Bazel (BEP) give stable, first-party, per-test structured events. Cargo's test JSON is unstable. In JS, Python and the JVM, test output comes from the test runner, so those adapters need a "report sub-adapter" that reads JUnit XML. JUnit XML is the lowest common denominator, so it is the right normalized artifact type. SARIF is for diagnostics, not tests.

### 4.3 CI integration matrix

| CI | Dispatch API | Run monitoring | Log API | Artifact API | Rate limits | Queue-latency notes |
|---|---|---|---|---|---|---|
| GitHub Actions | `workflow_dispatch` (returns run ID, V); `repository_dispatch` (T) | Runs/jobs REST; webhooks (T) | Job log download after the job ends (T) | Zip artifacts | REST primary limit ~5,000 req/h per user token; `GITHUB_TOKEN` ~1,000 req/h per repo (T). See vendor; parameterize | Hosted: variable. ARC scale-from-zero adds pod start time (T). microVM vendors claim seconds (V, Actuated design) |
| GitLab | Create pipeline / trigger tokens (T) | REST + webhooks | Trace (incremental, T) | Archive or file; `--location` for CDN redirects (V) | GitLab.com per-user limits (T); see vendor | Shared runners variable (T) |
| Buildkite | REST/GraphQL create build (T) | REST, webhooks, GraphQL | Raw log + byte-range tail (V) | Redirect URLs (V) | REST ~200 req/min per org (T); see vendor | Depends on your agent pool; you own scale-out |
| Azure Pipelines | Runs API (V) | Runs get (V) | Logs with line ranges (V) | Signed URL (V) | Throttling by TSTU (T) | Hosted pool variable (T) |
| CircleCI | v2 trigger (legacy) / newer trigger API (V) | v2 | **v1.1 only** (V) | v2/v3 (V) | See vendor | T |
| Jenkins | `buildWithParameters` (T) | `/api/json` | `progressiveText` | Per build | Bound by self-hosting | Executor availability |
| TeamCity | `buildQueue` (V) | REST | Full download (V) | REST (T) | Bound by self-hosting | Agent availability |
| CodeBuild | `StartBuild` (V) | `BatchGetBuilds` (V) | CloudWatch (V) | S3 | AWS API throttling (T) | The PROVISIONING phase comes before logs (V) |
| Cloud Build | `builds.create` (T) | LRO / get | GCS (V) | GCS | GCP quotas (T) | T |
| Tekton / Argo | Kubernetes create | Kubernetes watch | Pod logs | PVC / artifact repository | Kubernetes API server | Pod scheduling |
| Woodpecker | REST (T) | REST | **Live stream** (V) | — | Self-host | Agent availability |
| Bitrise / Codemagic | REST (V) | REST | REST (V for Bitrise) | Expiring URLs (V) | See vendor | macOS capacity (T) |
| Depot CI | `CIService.Run` (V) | `status --wait` (V) | `--follow` (V) | T | See vendor | T |

### 4.4 Remote-execution protocol comparison

| Protocol / API | Unit of work | Addressing | Executes? | Cancel | Live stream | Auth | Portability |
|---|---|---|---|---|---|---|---|
| **REAPI v2** (`remote_execution.proto`) | Action (Command digest + input root digest + platform + timeout) | Content (Digest = hash + size) | Yes | Optional LRO `CancelOperation`, best-effort, final status `CANCELLED` (V proto) | No standard live stdout; `WaitExecution` streams operation state (V) | Out of band (headers, mTLS) | **High.** The same client works across Buildbarn, BuildGrid, NativeLink, EngFlow and BuildBuddy. Platform property names differ (`OSFamily`/`ISA` vs vendor names, OBS ADR :442-445) |
| BuildBuddy Run API | Repo ref + steps (scripts) + patchset | Invocation ID | Yes (VM) | Invocation cancel | CLI streams | API key | Vendor-only |
| EngFlow `ef` | Command / whole build | Cluster | Yes | T | T | mTLS/JWT | Vendor-only |
| Depot BuildKit / CI APIs | BuildKit Solve / CI run | buildId / runId | Yes | CI cancel (V) | BuildKit status channel (V) | Bearer | Vendor-only |
| Turborepo cache v8 | Artifact blob | Task hash | No | n/a | n/a | Bearer | Open spec; multiple servers |
| Nx self-hosted cache | Tar archive | Task hash | No | n/a | n/a | Bearer | Open spec (OpenAPI) |
| Gradle HttpBuildCache / Maven cache extension | Cache entry | Tool key | No | n/a | n/a | Basic/bearer (T) | Tool-specific layouts; never mix them (V) |
| sccache / ccache backends | Compile output | Compiler-invocation hash | No (sccache-dist executes, V) | n/a | n/a | Backend credentials | Per tool |
| Nix binary cache / remote builders | Derivation / store path | Store hash | Remote builders execute (T) | T | T | Signing keys / SSH | Nix-only |
| GitHub Actions cache API | Tarball | User key + scope (branch/PR) | No | n/a | n/a | Runtime token | GHA-only |

### 4.5 Cache semantics comparison (these are not equivalent)

| Kind | Key | Value | Correctness invariant | Poisoning risk | Examples | How OMP² should model it |
|---|---|---|---|---|---|---|
| **Action cache (AC)** | Digest of a fully declared Action (command, inputs, platform) | ActionResult (exit code, output digests) | Hermetic action: the declared inputs determine the outputs | **High** if untrusted clients can write. Mitigations: read-only AC for CI/PR, and a split where clients can write CAS but only read AC (Develocity V; BuildBuddy read-only keys OBS ADR :1089) | REAPI servers; Bazel `--remote_cache` | Backend capability `action_cache`; `do_not_cache` for non-hermetic intents |
| **CAS** | Content hash | Bytes | Self-verifying (hash check) | **Low** if digests are verified; the risk is disclosure instead | REAPI CAS; OMP² `BlobStore` (OBS ADR :132) | Transport for inputs and outputs; not a "cache hint" |
| **Task/output cache** (tool-level) | Tool-computed task hash | Archived outputs | The tool's hash correctly covers all inputs | Medium/high (shared writable bucket) | Turborepo, Nx, Gradle, Maven extension, Develocity | Env/config passthrough; OMP² never reads or writes it |
| **Compiler cache** | Hash of the compiler invocation and preprocessed input | Object file | The compiler is deterministic for that hash | Medium (shared S3/Redis that PRs can write) | sccache, ccache | Cache hint → wrapper env; reported as advisory |
| **Compiler incremental state** | Previous build's fingerprints (mtime/checksum) | Mutable target dir | Same toolchain and paths; trusts mtimes unless checksum mode is on | **Silent staleness**; REAPI rewrites timestamps (OBS audit :114) | Cargo `target/`, Gradle daemon, MSBuild obj | Named cache dir (`CacheDecl`), honored only by backends with `warm_caches` |
| **Dependency cache** | Lockfile hash | Downloaded packages | Lockfile pins + registry integrity hashes | Low/medium (integrity hashes help) | `~/.cargo/registry`, `~/.m2`, pnpm store, uv cache, GOMODCACHE | Named cache dir keyed by lockfile digest |
| **Workspace reuse / snapshot** | Runner identity or snapshot key (BuildBuddy: every platform property, OBS audit :89) | Whole disk/VM state | The previous run left a consistent state | **High**: mutable, no content addressing, cross-branch bleed | BuildBuddy Run API snapshots, Blacksmith sticky disks, Namespace cache volumes, persistent SSH/agents | Separate `snapshot` capability; never assumed for correctness; keys per trust domain |

### 4.6 Auth and security comparison

| Mechanism | Secret exposure to workers | Fork-PR safety | Cache-poisoning surface | Notes |
|---|---|---|---|---|
| Generic REAPI | Every uploaded byte is disclosed to the provider (OBS ADR Part H). Secrets reach actions only through env/properties | n/a (OMP² is the client) | AC writes | Allow AC writes only for the trusted principal; `do_not_cache` for actions that carry secrets |
| BuildBuddy Run API | **`bb` hands the API key to every remote process** (OBS audit :144-147). Stored secrets via `include-secrets`; `secret-env-overrides` are redacted from logs and the AC (V) | n/a | AC + snapshots | Use a revocable key and an org that no release depends on (OBS) |
| EngFlow | The `engflow:secrets` platform property puts secrets into the action env and requires `secret:Execute` (V) | n/a | AC | Service JWTs for agents (V) |
| GitHub Actions | Fork `pull_request` runs do not get secrets (T). `pull_request_target` runs in the base context with secrets ("pwn request") (V) | Fork caches write only to the PR scope. Default-branch caches are **read-only** for `pull_request_target`, `issue_comment` and `workflow_run` (V) | `actions/cache` default-branch scope | Never check out fork code under `pull_request_target` (V). Self-hosted runners on public repos are dangerous (T) |
| GitLab / Buildkite / Jenkins / TeamCity | Agents hold secrets; persistent agents leak across jobs (T) | Protected branches / variables (T) | Shared caches on agents | Self-hosted persistent agents are the same risk class as persistent SSH |
| Runner vendors (Depot, Namespace, Blacksmith, RunsOn, Actuated, ARC) | Same as GHA, plus the vendor | Same as GHA | **Sticky disks / cache volumes shared by key** (V) | One key per trust domain (main vs PR) [INFERENCE] |
| sccache / ccache / Turborepo / Nx / Gradle caches | Bucket credentials on the worker | Read-only for PRs is recommended (Maven: only CI writes, V; Gradle `isPush` on CI, V) | Writable shared bucket | Give writers and readers different credentials |
| SSH persistent | Host credentials; anything left on disk | n/a | Mutable workdir | Treat the host as one trust domain |

### 4.7 Cost and ops complexity (parameterized)

No vendor rate is assumed except the verified GitHub self-hosted fee. Every term is a parameter that the operator supplies from the vendor's own pricing.

```
cost_per_month =
    Σ_jobs minutes × rate_compute(class, os)       # see vendor; parameterize
  + platform_fee_per_min × self_hosted_minutes     # GHA private repos: $0.002/min since 2026-03-01 (V)
  + cache_GB_month × rate_storage                  # CAS/AC, snapshots (BuildBuddy sizes a snapshot ≈ RAM + disk, OBS audit :129)
  + transfer_GB × rate_egress                      # snapshot save/restore, CAS upload of working trees
  + license_fee                                    # RunsOn yearly; NativeLink enterprise; see vendor
  + ops_hours × loaded_rate                        # self-hosting
```

| Class | Ops complexity | Main cost driver |
|---|---|---|
| Local | None | Developer machine time (the OMP² motivation, OBS ADR :9) |
| SaaS REAPI (BuildBuddy, EngFlow, NativeLink Cloud) | Low | Executor minutes + cache transfer |
| Self-hosted REAPI (Buildbarn, BuildGrid, NativeLink) | **High** (scheduler, workers, storage, toolchain images) | Infrastructure + ops hours |
| SaaS CI (GHA, CircleCI, Bitrise, Codemagic, Travis) | Low | Minutes by OS (macOS is the premium tier, T) |
| Bring-your-own-agent CI (Buildkite, TeamCity, Jenkins, GitLab self-managed) | Medium/high | Agent fleet |
| Kubernetes CI (Tekton, Argo, ARC) | High | Cluster ops |
| GHA runner vendors | Low (a label change) | Vendor rate vs hosted rate; the GHA self-hosted fee applies to private repos (V) |

---

## 5. Prioritization rationale (P0 / P1 / P2)

This section collects the "Priority and reason" column of §2 and the recommendations of §7–§9 into one place. The criteria are the survey's own. Where they are generalized, they are tagged [INFERENCE].

**P0: initial implementations and their conformance targets.**
- **Generic REAPI v2 backend.** One adapter covers five servers.
  - **Buildbarn** (#4) is the spec-pure, Apache-2.0 reference for hermetic conformance tests.
  - **BuildBuddy** (#1) is the first live host. It also exercises real vendor deviations: no LRO cancel, vendor property names, unverified live stdout.
- **GitHub Actions** (#29). Since 2026-02-19, dispatch returns the run ID (V), which removes the old correlation race. Through `runs-on` labels, this one adapter covers six runner vendors.
- **Cargo** (#16) has stable cargo JSON diagnostics and a clear cache-hint model.
- **Go** (#19) has a first-party, stable per-test stream (test2json) and a cache model unlike Cargo's (content-addressed GOCACHE). It is a second ecosystem that differs from the first, so it proves the adapter boundary.

**P1: roadmap.** These fill new shapes or large ecosystems:
- REAPI conformance breadth: EngFlow, NativeLink.
- An independent CI with the most complete log and cancel API: Buildkite.
- Enterprise CI: GitLab, Azure Pipelines.
- Working-tree submit without a push: Depot CI.
- A warm remote workspace: the BuildBuddy Run API.
- Major ecosystems:
  - JS: npm, pnpm, Bun.
  - JVM: Gradle, Maven.
  - Python: uv.
  - .NET: dotnet, MSBuild.
- Monorepo build systems: Bazel, placed Local and read through BEP.
- sccache, as one way to realize Cargo/C++ cache hints.

**P2: on demand.**
- Niche or redundant REAPI servers: BuildGrid.
- Tool-owned caches that need only env passthrough: Develocity, Nx Cloud, Turborepo, Vercel, ccache.
- Further REAPI-client build systems, which follow the Bazel pattern: Buck2, Pants, Please.
- C/C++ meta-builds: CMake, Ninja, Meson.
- Lower-share or specialized CI:
  - Jenkins, CircleCI, TeamCity, Bitbucket, Harness, Semaphore, Woodpecker.
  - Kubernetes CI: Tekton, Argo.
  - Mobile: Bitrise, Codemagic.
  - Cloud batch: AWS CodeBuild, Google Cloud Build.
- Dagger, as an opaque Local command.
- Nix, as a toolchain provider.

**none.** These get no adapter. Each is excluded for a specific reason in §9.

[INFERENCE] The general rule behind the ordering: cover the most portable surface first (REAPI over vendor APIs), cover the most different execution shapes early (argv/CAS vs declared-entry/GitRef), and never build an adapter whose work another layer already does (runner labels, tool-owned caches, opaque recipe runners).

---

## 6. Architecture stress test

### 6.1 Selection

| Slot | Choice | Why it is a hard case |
|---|---|---|
| REAPI provider | **BuildBuddy, REAPI surface only** | Real vendor deviations: no LRO cancel, vendor property names, unverified live stdout |
| GitHub-native CI | **GitHub Actions** | Cannot accept argv or a working tree. The unit is a declared workflow at a ref |
| Independent CI | **Buildkite** | Bring-your-own agents and the best log/cancel API, but still ref-based |
| Local mechanism | **Local process group** (envd `ExecHost`) | Immediate cancel and a live stream: the baseline |
| Persistent remote mechanism | **SSH host with a persistent workdir**. Cross-check: BuildBuddy Run API snapshot | Mutable state; no content addressing |
| Build systems | **Cargo** and **Bazel** | Cargo has no RE and unstable test JSON. Bazel owns its own remote execution, so OMP² must not wrap it |
| Language ecosystems | **Rust** and **TypeScript/Node** (pnpm workspace + vitest JUnit) | Different cache dirs; test output comes from a third-party runner |

### 6.2 Minimal capability model

This is ADR 0040's core plus descriptor fields. The verbs are unchanged. The sketch below is a proposal, not implemented code.

```rust
// Verbs = ADR 0040 traits, unchanged (OBS 0040…md:534-556)
// submit            → ExecutionBackend::start(ExecutionRequest) -> Run
// status + stream   → ExecutionRun::next_event(): Queued | Uploading | Accepted{operation} | Executing | Output(frame) | Settled(result)
// cancel            → ExecutionRun::cancel() -> CancelOutcome { Stopped, Requested, Unsupported, AlreadySettled }
// artifacts         → ExecutionResult.artifacts: [Artifact{path, blob, fetched}] (+ stdout/stderr BlobRef)
// cache-hint        → ExecutionRequest.caches: [CacheDecl] + ExecutionPolicy.cache; result reports cache_hit
// workspace-snapshot→ hint = SnapshotKey (derived: toolchain id + lockfile digests + trust domain); result reports reused: bool
// reattach          → ExecutionBackend::reattach(Reattach)

pub struct BackendCapabilities {          // ADR fields kept; the four fields marked NEW are the additions
  /* ADR: platforms, needs, max_timeout, action_cache, cancel, max_blob_bytes, warm_caches, enforced */
  pub submit:   SubmitShape,     // NEW: Argv | DeclaredEntry{max_inputs} (CI workflow/pipeline taking argv as an input)
  pub input:    InputTransport,  // NEW: Cas | Patch | GitRef | LocalFs   (the router matches it against R4 "working tree, not HEAD")
  pub stream:   StreamSupport,   // NEW: Live | Tail{ranged} | PostHoc
  pub snapshot: SnapshotSupport, // NEW: None | WarmByKey{mutable: bool} | Persistent{mutable: true}
  // cancel: CancelSupport = Immediate | BestEffort | Unsupported   (the ADR already has CancelSupport)
}
```

Three rules keep the core vendor-neutral:
1. **Every verb has a defined degraded mode.** The router (`place`, a pure function) filters backends by capability. No caller branches on a vendor.
2. **Hints are advisory, and their effect is reported back** (`cache_hit`, `reused`). Correctness never depends on a hint.
3. **Cancel always settles locally.** The remote side's state is reported as `Requested` or `Unsupported` (ADR Part I).

### 6.3 Fit check

F = full, P = partial, — = none.

| Mechanism | submit | status | stream | cancel | artifacts | cache-hint | snapshot | Fits without core change? |
|---|---|---|---|---|---|---|---|---|
| Local process group | F (argv) | F | F (Live) | F (Immediate) | F (LocalFs) | P: honored only through tool dirs | F (it is the checkout) | **Yes** |
| BuildBuddy, REAPI | F (Action; input = Cas) | F (Execute/WaitExecution) | **P (PostHoc)** | **P**: vendor RPC behind the backend; LRO answers `Unsupported`; merged actions may run until `Action.timeout` (OBS ADR :870) | F (typed output paths) | **F for the AC**; **— for named caches** (no recycling, OBS Slice 0c) | **—** | **Yes.** Vendor cancel stays inside the backend, as the ADR amendment requires |
| GitHub Actions | **P**: `DeclaredEntry`. Needs an installed generic dispatch workflow that takes argv as an input, and a **pushed ref** (`GitRef`). For a dirty tree, the router must either push a scratch ref (an effect that needs operator approval) or return `Unplaceable` | F (run ID returned since 2026-02) | **P (PostHoc)** (T) | P (BestEffort; force-cancel exists) | P (zip archives, re-addressed into the CAS on fetch) | P (advisory, through the workflow's `actions/cache`) | — (runner vendors add it, but the API cannot see it) | **Yes**, as a partial backend. The exit code is synthesized from `conclusion` (see §6.4) |
| Buildkite | P: `DeclaredEntry` + `GitRef` (create a build with commit + env, T) | F | **P (Tail, ranged)** (V) | P (BestEffort) | F (per-job URLs) | P (agent-local) | P (persistent self-hosted agents → `Persistent{mutable}`) | **Yes** |
| SSH persistent | F (argv) | F | F (Live) | F/P (Immediate while connected; BestEffort after disconnect) | F (rsync) | F (dirs really persist) | **F (`Persistent{mutable:true}`)** | **Yes.** The router must isolate trust domains by `SnapshotKey` |
| BuildBuddy Run API (cross-check) | P (script + `Patch`) | F (poll) | F (CLI) | P | P | P | **P (`WarmByKey{mutable}`; key = all properties)** | **Yes**, behind its own backend (ADR amendment) |
| Cargo adapter | Emits argv + `CacheDecl{cargo-target, cargo-registry}` + the toolchain from `rust-toolchain.toml` | — | Parses cargo JSON from stdout | — | Reports: a nextest JUnit artifact (libtest JSON only with a pinned nightly) | Hints `CARGO_TARGET_DIR`, optionally `RUSTC_WRAPPER=sccache` | `SnapshotKey` from `Cargo.lock` + toolchain | **Yes** |
| Bazel adapter | One Local-placed `bazel test …` request; remote offload is configured in Bazel's own `.bazelrc` | — | BEP JSON file → progress events | SIGINT to the client = Local Immediate; remote actions are Bazel's job | `test.xml` + BEP as artifacts | Reports `cache_hit` from BEP; never sets the AC itself | — | **Yes**, because Bazel is deliberately **not** routed to REAPI (wrapping it would execute twice; matches ADR Part A.4) |
| Rust ecosystem | — | — | — | — | — | Registry + target dirs | Lockfile key | **Yes** |
| TS/Node (pnpm + vitest) | The pnpm adapter emits argv; `--reporter ndjson` for install | — | — | — | A report sub-adapter reads vitest JUnit (T) | pnpm store dir + optional Turborepo/Nx env passthrough | `pnpm-lock.yaml` key | **Yes** |

### 6.4 Where providers can claim only partial capability

1. **REAPI streaming is post-hoc only.** Live output is not part of the standard (OBS ADR :1093).
2. **REAPI cancel is optional and not advertised.** A server that lacks it returns `UNIMPLEMENTED`, which maps to `Unsupported`. BuildBuddy needs a vendor RPC, and actions merged with another request may keep running (OBS ADR :870).
3. **REAPI has no warm state.** Raw REAPI never recycled runners in the probes (OBS audit Slices 0b/0c), so `CacheDecl` named dirs are unsupported there. Only the AC works.
4. **CI submit works only through a pre-declared entry point.**
   - GHA allows at most 25 dispatch inputs (V changelog 2025-12-04).
   - CI providers cannot ingest a working tree: GHA, Buildkite and GitLab are all `GitRef`. Depot CI (`Patch`, V) and the BuildBuddy Run API (`Patch`) are the exceptions.
5. **CI reports a conclusion, not an exit code** (success, failure, cancelled, timed_out, skipped). Mapping it to `Outcome::Exited{code}` loses information. Per-job detail travels as artifacts (JUnit, logs).
   - **Core-model risk:** if OMP² later needs per-job results as first-class data, `Outcome` will need an additive `Concluded{…}` variant. Today it fits.
6. **CI log access varies:**
   - GHA: post-job only (T).
   - CircleCI: v1.1 only (V).
   - Buildkite: tail (V).
   - Woodpecker: live (V).
7. **Snapshots are mutable and keyed by vendor-defined inputs.** OMP² can hint at them but must not rely on them.
   - BuildBuddy keys on every platform property, including the save policy (OBS audit :89-93).
   - Blacksmith keys on a cache key, with clone/commit (V).
8. **Bazel exposes only BEP to OMP².** OMP² cannot cancel or relocate individual remote actions. This is by design.
9. **Cargo test JSON is unstable** (V). Structured per-test results need nextest or a pinned nightly.

### 6.5 Verdict

All seven mechanisms, both build systems and both ecosystems fit the seven verbs without changing the trait shapes. What they needed was four new descriptor fields (`submit`, `input`, `stream`, `snapshot`) plus the existing `cancel`. One extension point is still latent: an `Outcome` variant for CI conclusions. This is a paper fit check against the ADR 0040 contract. Nothing was implemented or run.

---

## 7. Recommended initial implementations

The limit was 2 providers and 2 build systems. Local is the baseline backend and does not count toward the limit.

**Providers**
1. **Generic REAPI v2 backend.** One adapter covers BuildBuddy, EngFlow, NativeLink, Buildbarn and BuildGrid. Vendor differences are profile data (header name, property names), plus a capability-gated vendor-cancel module where needed (OBS ADR Part A.3). Conformance tests run locally against **Buildbarn** (Apache-2.0, spec-pure) and live against **BuildBuddy**.
2. **GitHub Actions**: dispatch, monitor, logs, artifacts, cancel. Since 2026-02-19, dispatch returns the run ID (V), which removes the old correlation race. Runners are chosen by `runs-on` labels, so this one adapter also covers Depot runners, Namespace, Blacksmith, RunsOn, Actuated and ARC.

**Build systems**
1. **Cargo**: stable cargo JSON for diagnostics, nextest JUnit for tests, and a clear cache-hint model.
2. **Go**: first-party, stable test2json, and a content-addressed GOCACHE, which is a different cache model from Cargo's. A second ecosystem that differs from the first proves the adapter boundary.

Go comes before pnpm because in JS the test runners produce the test output. A JS adapter therefore needs a report sub-adapter, so it comes second.

**Reconciliation with ADR 0040.** The ADR 0040 amendment puts the **BuildBuddy Run API** first for OMP²'s *own* builds, because of the measured warm snapshots (OBS `0040…md:1495-1500`; audit Slice 0c decision at `remote-verification-trial.md:174-176`). For other projects, this survey recommends the generic REAPI backend first, because it is portable across five servers. The Run API remains a P1 "persistent remote workspace" backend behind the same contract. The two orders do not conflict: both build on `omp-exec`, which the amendment requires to be built first. Recording this order as an ADR 0040 follow-up is open question 3.

---

## 8. Extension roadmap

| Phase | Add | Rationale |
|---|---|---|
| R1 | JUnit report normalizer; pnpm/npm/Bun adapter (with vitest/jest/bun JUnit); SSH persistent backend | Breadth: JS is the most common ecosystem. SSH is the vendor-neutral warm path |
| R2 | Buildkite (independent CI); Gradle + Maven adapters (JUnit, HTTP build-cache env passthrough); uv/pytest adapter | A second CI shape; the JVM and Python ecosystems |
| R3 | Bazel adapter (BEP, placed Local); BuildBuddy Run API backend (snapshot capability); dotnet adapter (TRX/JUnit) | Monorepos; warm remote execution; .NET |
| R4 | GitLab CI, Azure Pipelines; Depot CI (patch submit) | Enterprise CI; working-tree submit without a push |
| R5 (on demand) | AWS CodeBuild / Cloud Build as "cloud batch" backends; Tekton/Argo; CMake/Meson adapter; Buck2/Pants/Please (same pattern as Bazel); Bitrise/Codemagic for mobile; Dagger as an opaque Local command | Driven by demand |

---

## 9. Technologies that must NOT get adapters

| Technology | Reason |
|---|---|
| **Earthly** | Not actively maintained; Cloud shut down on 2025-07-16 (V). |
| **BuildJet** | Shut down on 2026-03-31 (V). |
| **Travis CI** | Low share. |
| **GoCD** | Small maintainer group and EOL dependencies (V). |
| **Bamboo** | Data Center only (V). |
| **Drone** | In maintenance mode, with a successor still in beta (V). |
| **AWS CodePipeline, Harness CD** | Release and deploy orchestration, not build execution. |
| **GHA runner vendors**: Depot runners, Namespace, Blacksmith, RunsOn, Actuated, ARC | Transparent behind `runs-on` labels. An adapter would duplicate the GitHub Actions one. Depot **CI** is a separate product and stays P1. |
| **sccache, ccache** | Compiler wrappers that are configured, not providers. They realize Cargo/CMake cache hints through env. |
| **Develocity, Nx Cloud, Turborepo Remote Cache, Vercel Remote Cache** | Tool-owned caches. OMP² passes credentials and env and records links. It never reads or writes their entries. |
| **Task, Just** | Opaque command runners. The adapter for the underlying tool interprets the commands. |
| **Nix** | A toolchain provider (resolves `ToolchainId`), not an execution backend. |
| **Ninja** | Reached through CMake/Meson. |
| **Bazel, Buck2, Pants, Please as REAPI-wrapped workloads** | OMP² must never re-route their actions. Their adapters are placed Local and only read BEP or reports. |

Users who need Travis CI, GoCD, Bamboo or Drone can run OMP² inside those CIs instead.

---

## Confidence

- **High:**
  - REAPI semantics.
  - GitHub Actions dispatch, cancel and fee.
  - The Buildkite, GitLab, Azure, TeamCity, CodeBuild, Cloud Build, Bitrise and Codemagic endpoints.
  - Vendor shutdowns and license changes.
  - The OMP² ADR facts.
- **Medium:** rows tagged T. These cover the licenses of several OSS tools, the Jenkins, Bitbucket, Harness and Travis APIs, rate limits, and GHA log timing.
- **Low:** queue-latency claims. They all depend on the vendor. Only BuildBuddy boot times were measured (OBS audit :115).

## Open questions

1. Should OMP² allow pushing a scratch ref so that ref-based CI backends can satisfy R4 "working tree, not HEAD"? This is an effect that needs operator approval. Without it, those backends are `Unplaceable` for dirty trees.
2. Should `Outcome` gain an additive `Concluded{conclusion}` variant now? The alternative is to keep synthesizing exit codes from CI conclusions and defer the change.
3. Should the generic-REAPI-first order for other projects be recorded as an ADR 0040 follow-up? The amendment sets a BuildBuddy-Run-API-first order for OMP² itself.
4. Licensing: is NativeLink's FSL acceptable for a *test target*? It would not be bundled, so [INFERENCE] probably yes, but this needs a check. The license review for the BuildBuddy vendor-cancel protos is already pending (OBS ADR A.3).
5. Cargo per-test results: should OMP² standardize on nextest JUnit, or pin a nightly to get libtest JSON?

## Sources

All searched on 2026-10-11, grouped by area.

**GitHub Actions and runner vendors**
- github.blog changelog 2026-02-19
- docs.github.com: workflow-runs and dependency-caching
- github.com/resources: 2026 pricing
- depot.dev docs and changelogs
- buildjet.com blog
- namespace.so blog
- useblacksmith GitHub
- runs-on.com
- docs.actuated.com
- actions-runner-controller GitHub

**Remote execution and caches**
- buildbuddy.io/docs/remote-bazel; buildbuddy LICENSE
- docs.engflow.com
- nativelink LICENSE; nativelink.com/license
- github.com/buildbarn
- pypi: buildgrid
- docs.develocity.ai/bazel/2026.3
- nx.dev blog and docs
- turborepo.dev openapi
- sccache docs
- ccache manual

**Build systems and language toolchains**
- bazel.build BEP
- buck2 remote_execution.md
- pantsbuild docs
- please.build/remote_builds
- rustc book: tests
- go.dev test2json
- Maven build-cache extension
- docs.gradle.org build_cache
- learn.microsoft.com: MTP reports
- bun.sh reporters
- docs.astral.sh: uv build-backend

**CI services**
- learn.microsoft.com: Azure DevOps REST 7.1
- docs.gitlab.com api
- circleci.com docs: api v2
- buildkite.com/docs/apis
- JetBrains TeamCity REST
- AWS CodeBuild API
- Google Cloud Build cancel
- readme.drone.io: gitness FAQ
- semaphore.io
- travis-ci.com
- woodpecker-ci.org/api
- tektoncd pipelineruns.md
- argo-workflows docs
- gocd.org status
- atlassian.com: DC EOL
- devcenter.bitrise.io
- docs.codemagic.io

**Pipeline engines**
- dagger GitHub and blog
- earthly.dev blog

**Local evidence (OBS)**
- `docs/adr/0040-remote-build-and-test-execution.md`
- `docs/audits/remote-verification-trial.md`
- `docs/parked/2026-10-09-remote-execution-next-steps.md`
- `docs/adr/STATUS.md:48`
