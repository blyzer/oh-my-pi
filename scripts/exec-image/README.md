# scripts/exec-image

Remote execution image for `omp adw` (ADR 0040, Part G).

## What is baked in

The image carries every dependency that `just test-pkg omp-envd` needs, because
remote actions run with networking disabled (`REMOTE_POSTURE.network = Disabled`
in `crates/driver/src/adw/remote.rs`).

| Layer | Contents |
|-------|----------|
| System packages | `build-essential`, `cmake`, `ninja-build`, `pkg-config`, `ca-certificates`, `git`, `python3`, `curl`, `zstd`, `rsync` |
| Rust tooling | `rustup toolchain install` of the channel and components in `rust-toolchain.toml`; `just`; `cargo-nextest 0.9.146` (prebuilt). The toolchain must be in the image because the remote action has no network |
| uv | Used by `fetch-python.sh` to install pure-Python bundled packages |
| CPython bundle | `fetch-python.sh /opt/omp-python` — `python-build-standalone` archive at `TAG=20260807 VER=3.14.7`; sets `PYO3_CONFIG_FILE=/opt/omp-python/python/pyo3-config.txt` |

**cmake and ninja** are required because `.cargo/config.toml` sets
`CMAKE_GENERATOR = "Ninja"`, and `audiopus_sys` builds vendored Opus via the
`cmake` crate when no system `libopus` is found by `pkg-config`.

**The CPython bundle cannot be fetched at run time.** It must be in the image.

## Drift guard

`scripts/exec-image/check-pkg-drift.sh` verifies that every package installed
by `apt-get install` in the Dockerfile is also present in the
`apt-get install -y` call in `scripts/remote-verify-runner.sh`, and vice versa.
Run it after editing either file:

```
scripts/exec-image/check-pkg-drift.sh
```

It exits non-zero and lists the offending packages on failure.

## Build, pin and publish

Run this when `rust-toolchain.toml`, the `fetch-python.sh` `TAG`/`VER`, or the nextest pin changes.
The build context MUST be the repository root: the CPython layer copies `crates/py`.

```sh
# 1. Build (from the repository root; linux/amd64, which is what the runner executes)
docker build --platform linux/amd64 -f scripts/exec-image/Dockerfile \
  -t ghcr.io/blyzer/omp-reapi:candidate .

# 2. Push, then read the registry digest of what was pushed
docker push ghcr.io/blyzer/omp-reapi:candidate
docker buildx imagetools inspect ghcr.io/blyzer/omp-reapi:candidate | sed -n 's/^Digest: *//p'
```

Pin it in the operator's `~/.o2/config.cfg` (never a project file; remote placement is
operator-controlled):

```
sv_exec_remote_image docker://ghcr.io/blyzer/omp-reapi@sha256:<digest>
```

## Running the G1 omp-envd measurement

With the image pinned, in an operator config that also sets `sv_exec_remote_endpoint`
(`grpcs://remote.buildbuddy.io`), `sv_exec_remote_profile buildbuddy` and
`sv_exec_remote_credential env:BUILDBUDDY_API_KEY`, and a workflow whose phase is
`command = ["just", "test-pkg", "omp-envd"]`:

```sh
omp adw run <workflow> --exec-policy remote-only --max-attempts 1
```

Record wall time, client CPU and peak memory (`scripts/measure-recipe.py`) in
`docs/audits/remote-verification-trial.md`.

## Verified 2026-10-24

- The Dockerfile builds. First build ever, on an Apple-silicon host with Apple's
  `container` CLI (`container build --arch amd64 -f scripts/exec-image/Dockerfile
  -t ghcr.io/blyzer/omp-reapi:candidate .`, 15.6 minutes), including
  `fetch-python.sh` inside the build. Pushed as
  `ghcr.io/blyzer/omp-reapi@sha256:772594c52a49c67ba4545f8ce8a1de30a592300afd97e08a28975dc143ca0cc0`
  (tag `candidate`). The first push attempt failed at the last blob with a ghcr
  `BLOB_UPLOAD_UNKNOWN` 404; an immediate retry succeeded.
- The image unblocks the `omp-envd` recipe: `audiopus_sys` builds (cmake is
  present) and the whole suite compiles and runs remotely through the omp REAPI
  backend. The run record is in `docs/audits/remote-verification-trial.md`,
  including the process-lifecycle tests that fail nondeterministically inside
  the remote sandbox.

## Not verified

- The previously published image (`2026-10-16-path`) already carried
  `/usr/local/rustup/toolchains/nightly-2026-08-08-x86_64-unknown-linux-gnu`, so it was built some
  other way and that recipe is not in the repository.
