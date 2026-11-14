# 0070. Platform sandbox and CI: no VM-backed macOS sandbox and no Windows test job now; the existing fallbacks stand

Status: proposed
Date: 2026-11-13
Area: runtime

Research: [docs/research/platform-sandbox-ci.md](../research/platform-sandbox-ci.md) (Gap 19 of the upstream gap closure). This record states a research-only decision. It has no implementation slices; it lists the measurements and owner answers that would reopen it.

## Context

### What OMPv1 does

- No VM sandbox. A search of `crates`, `docs` and `packages/coding-agent/src` of the pinned checkout for Seatbelt, `sandbox-exec`, Virtualization.framework, Tart, Lima, Firecracker, krunkit and Apple `container` found only unrelated hits. Its isolation crate `pi-iso` is copy-on-write filesystem isolation (APFS, Btrfs, ZFS, overlayfs, Linux reflink, Windows ProjFS, Windows block clone), not process confinement (`upstream:crates/pi-iso/src/apfs.rs`, `upstream:crates/pi-iso/src/overlayfs.rs`, `upstream:crates/pi-iso/src/projfs.rs`). The only VM advice is for the `computer` tool: use a dedicated account, or a dedicated VM where nobody can answer a control request (`upstream:docs/computer-use.md:6`, `upstream:docs/computer-use.md:208`). The user brings the VM.
- No Windows test CI. Release binaries are cross-built on Linux, then a release-only job runs `--version` and `--smoke-test` on each platform, including `windows-2025` and `windows-11-arm` (`upstream:.github/workflows/ci.yml:863-868`, `upstream:.github/workflows/ci.yml:872-873`, `upstream:.github/workflows/ci.yml:887-889`).

### What omp2 has

- `omp-sandbox` supports Seatbelt, Bubblewrap, Landlock with seccomp, gVisor, Docker, Docker with runsc and Windows AppContainer (`crates/sandbox/README.md:3`). `Backend` is a closed enum with fixed capability sets (`crates/sandbox/src/capability.rs:22-50`, `crates/sandbox/src/capability.rs:400-423`).
- On macOS the candidates are Seatbelt, then Docker with runsc, then Docker (`crates/sandbox/src/runner.rs:1113-1116`); the fallback is Seatbelt (`crates/sandbox/src/runner.rs:1128-1135`). So a VM-bounded fallback already exists in one form: Docker Desktop and its relatives run containers in a Linux VM. The Docker backend needs an image named in `OMP_SANDBOX_DOCKER_IMAGE` and runs the command inside it with host paths only through declared bind mounts (`crates/sandbox/src/backends/docker.rs:24`, `crates/sandbox/src/backends/docker.rs:72-76`, `crates/sandbox/src/backends/docker.rs:207-208`). The command runs on Linux, not macOS. [INFERENCE: Docker Desktop on macOS runs Linux images, from general knowledge.]
- Seatbelt's launcher is `/usr/bin/sandbox-exec` (`crates/sandbox/src/backends/seatbelt.rs:21`). On this host, macOS 27.2.1, `man sandbox-exec` reads "sandbox-exec – execute within a sandbox (DEPRECATED)" and points developers at App Sandbox. It is deprecated, not removed. Seatbelt has no IPC restriction, no `proc.noexec` and no CPU, memory or process limits (`crates/sandbox/src/capability.rs:320-332`), and records its own caveats: path-based scopes that an allowed hardlink can bypass, ephemeral writes as an APFS clone of the working directory, no disk limit, loopback blocked with network off (`crates/sandbox/src/backends/seatbelt.rs:325-398`).
- ADR 0006 puts a VM at the environment-target level: "The same host MUST be able to point the stub at a local process, a container, a VM, or a remote machine"; today every target runs the full `omp-envd`, and a minimized stub is deferred (`docs/adr/0006-host-policy-sandbox-stub.md:44-47`, `docs/adr/0006-host-policy-sandbox-stub.md:150`). A guest with an SSH server is reachable by the SSH execution backend for build and test commands (`crates/exec-ssh/README.md:1-6`); whether general tools can target an SSH host was not traced (UNVERIFIED).
- VM tool terms, read from the GitHub API on 2026-11-13: Tart is under the Functional Source License (FSL-1.1-ALv2, copyright line naming OpenAI in the file), and its "Competing Use" clause bars making the software available in a commercial product that substitutes for it or offers substantially similar functionality, while internal use is permitted. Lima, Apple `container`, Apple `containerization` and UTM are Apache-2.0; Colima is MIT. Apple `container` runs Linux containers as lightweight VMs, needs Apple silicon and is designed for macOS 26 or later (its README).
- Windows: code exists in 19 crates under `cfg(windows)`, including an AppContainer sandbox backend and a `win32` desktop backend (`crates/sandbox/src/backends/appcontainer.rs`, `crates/desktop/src/win32`, `Cargo.toml:325`). There is no Windows CI job; the workflows are Ubuntu and macOS only (`.github/workflows/p8-baseline.yml:50-56`). A whole-workspace Windows build is blocked: `omp-py` is an ungated dependency of `omp-app` and `omp-envd` (`crates/app/Cargo.toml:103`, `crates/envd/Cargo.toml:67`), and its fetch script supports only macOS arm64 and Linux x86_64 and aarch64 (`crates/py/scripts/fetch-python.sh:153-168`), with link lists for ELF and Mach-O only. The `justfile` uses `bash` (`justfile:5`) and the toolchain is a pinned nightly (`rust-toolchain.toml:2`).
- CI economics: the self-hosted Mac runs the one job that benefits from its persistent target directory; everything else is hosted (`docs/research/ci-macos-runner-fallback.md:5-21`). Ten named checks are required and new jobs must not rename them (`docs/BRANCH_PROTECTION.md:42`, `docs/BRANCH_PROTECTION.md:84`).

### Rules that bind

- ADR 0006: policy on the host, an obedient stub in the sandbox; a VM is an environment target.
- ADR 0036: Python-only extensions and eval, which is why embedded Python cannot simply be made optional. ADR 0024: no new roster family. Additive only.
- No new language runtimes; no `pi` identity.

## Decision

**D1. Do not add a VM-backed macOS sandbox backend now.** Seatbelt stays the macOS default. The Docker forms stay the fallbacks. Reasons: `sandbox-exec` is deprecated but present; a Linux VM boundary already exists through Docker; a macOS-guest design needs an entitlement, an image distribution story, host-side egress filtering and a shared-directory story, none of which has been measured; and ADR 0006 places a VM at the environment-target level, not in the sandbox crate.

**D2. Do not bundle or redistribute Tart.** Its licence limits making it available in a competing product. Any documented recipe names Apache-2.0 tools (Lima, Apple `container`) first and treats Tart as an optional user-installed tool, pending the owner's view (O4).

**D3. Reopen D1 on any of these triggers:** Apple announces removal of `sandbox-exec`; a measured spike shows a guest starts fast enough and a shared-directory build is fast enough; or the owner wants hardlink-style scope bypasses closed. Then the preferred shape is a VM as an environment target through the ADR 0039 work, not a new `Backend` variant.

**D4. Do not add a Windows test job now.** A whole-workspace job is blocked by the embedded Python. The staged route is recorded here for later: (a) list the crates that do not depend on `omp-py` for a Windows target, (b) a non-required compile check of those crates, switched on by a repository variable and run on demand and nightly, (c) their unit tests on a hosted Windows runner, (d) only on an owner decision, embedded Python for Windows and a release smoke like upstream's.

**D5. No convar, feature or CI change is made by this record.** If D3 or D4 is later accepted, the gates are a `cap-sandbox-vm` feature with a default-off `sv_sandbox_vm` convar for any VM work, and a `vars.CI_WINDOWS` repository variable, unset by default, for Windows CI.

## Rejected options

- **A new `Backend::VmGuest` variant now.** Weeks of work and an entitlement, with no measurement showing the need. It also touches every exhaustive `match` on `Backend` (`crates/sandbox/src/runner.rs:41`, `crates/sandbox/src/runner.rs:240`, `crates/sandbox/src/runner.rs:318`, `crates/sandbox/src/runner.rs:406`, `crates/sandbox/src/runner.rs:458`).
- **Bundle Tart or any VM tool.** No redistribution right for Tart; the others are not needed to decide.
- **A full-workspace `windows-latest` job.** Blocked by `omp-py`.
- **Add Windows support to `omp-py` now.** Large, and it widens a locked area (ADR 0036); only with an owner decision for a Windows release.
- **Do nothing and say nothing.** The Seatbelt deprecation and the missing Windows check deserve a recorded decision and triggers.

## Slices

None. This record is research only and authorizes no implementation. If the owner accepts D3 or D4, each stage gets its own record with its own slices and tests. The measurements that would inform them are:

- **V0 (spike, no repository change).** On the self-hosted Mac, time a clean guest start and `true` for Tart, Lima and Apple `container`; time a shared-directory `cargo build` against a native one; record what egress filtering each offers; record whether each can run a macOS-native command. Until measured, every VM speed claim is UNVERIFIED.
- **W0 (list, no repository change).** List the workspace crates that do not depend on `omp-py` for a Windows target, using the dependency graph. Until listed, the claim that a subset can build on Windows is UNVERIFIED.

## Upstream origin and license

Nothing from upstream is reused: it has no VM sandbox. For Windows CI the useful idea is the release smoke matrix and the comment explaining why binaries are cross-built and then run on their own platform (`upstream:.github/workflows/ci.yml:863-868`). Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`); workflow YAML is configuration and copying it keeps that notice. Third-party VM tools were checked only for licence: Tart FSL-1.1-ALv2; Lima, Apple `container`, Apple `containerization` and UTM Apache-2.0; Colima MIT.

## Consequences

- macOS sandboxing is unchanged: Seatbelt with its documented caveats, Docker as the Linux fallback.
- The `sandbox-exec` deprecation is recorded with explicit triggers instead of being a surprise.
- Windows code stays unchecked in CI. `cfg(windows)` paths can rot unseen until D4 is taken up.
- Nothing in the repository, its CI or its dependencies changes.

## Open questions for the owner

- **O1.** Apple's macOS licence for guests: how many macOS guests a Mac may run, and under what terms. Not read (UNVERIFIED). Recommendation: read it before any macOS-guest work.
- **O2.** Nested virtualization: GitHub-hosted macOS runners are VMs themselves; whether Virtualization.framework runs inside them is unknown. Recommendation: assume VM work is testable only on the self-hosted Mac.
- **O3.** Entitlements and signing for Virtualization.framework use. Not checked. Recommendation: check against `package-macos.yml` before any VM backend.
- **O4.** Is a recipe that names Tart acceptable, given its FSL licence and Competing Use clause? Recommendation: name Lima and Apple `container` first; mention Tart only as a user-installed option, or not at all.
- **O5.** Seatbelt's removal date. None found. Recommendation: the triggers in D3 stand until Apple states one.
- **O6.** Can general tools, not only build runs, target an SSH host? Not traced. Recommendation: trace it before writing any guest-over-SSH recipe.
- **O7.** Does the owner want a Windows release? Recommendation: decide before spending on `omp-py` for Windows; until then take D4 steps (a) and (b) only if cheap.
- **O8.** Hosted Windows minute rates and the cost of one run: not checked. NOT MEASURED. Recommendation: measure with the compile check before adding unit tests.

## Status in omp

**Status: Not started; research only.** Reusable today: the Docker backend as the macOS VM-boundary fallback, the SSH execution backend, the CI variable pattern (`vars.MACOS_RUNNER`, `vars.CI_LINUX_MODE`).

## References

[docs/research/platform-sandbox-ci.md](../research/platform-sandbox-ci.md); ADR 0006 (policy on the host, a stub in the sandbox); ADR 0036 (Python-only extensions and eval); ADR 0039 (remote control and factory workers); [docs/research/ci-macos-runner-fallback.md](../research/ci-macos-runner-fallback.md).

