# scripts/exec-image

Remote execution image for `omp adw` (ADR 0040, Part G).

## What is baked in

The image carries every dependency that `just test-pkg omp-envd` needs, because
remote actions run with networking disabled (`REMOTE_POSTURE.network = Disabled`
in `crates/driver/src/adw/remote.rs`).

| Layer | Contents |
|-------|----------|
| System packages | `build-essential`, `cmake`, `ninja-build`, `pkg-config`, `ca-certificates`, `git`, `python3`, `curl`, `zstd`, `rsync` |
| Rust tooling | `rustup` + `nightly-2026-08-08` + components from `rust-toolchain.toml`; `just`; `cargo-nextest 0.9.146` |
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

## Build, pin, and publish

Run this when `rust-toolchain.toml`, `fetch-python.sh` `TAG`/`VER`, or the
nextest pin changes.

```sh
# 1. Build
docker build -t omp-exec-remote:candidate scripts/exec-image/

# 2. Get the new digest
DIGEST=$(docker inspect --format='{{index .RepoDigests 0}}' \
         omp-exec-remote:candidate | sed 's/.*@//')
# Or, after pushing:
# DIGEST=$(docker manifest inspect ghcr.io/blyzer/omp-reapi:latest \
#          | jq -r '.config.digest')

# 3. Push
docker push ghcr.io/blyzer/omp-reapi:candidate
DIGEST=$(docker inspect --format='{{index .RepoDigests 0}}' \
         ghcr.io/blyzer/omp-reapi:candidate | sed 's/.*@//')

# 4. Update the convar and commit
#    Set sv_exec_remote_image to:  ghcr.io/blyzer/omp-reapi@<DIGEST>
#    Commit rust-toolchain.toml or fetch-python.sh change together with the
#    digest update so the image version and the repo pin stay in sync.
```

## Running the G1 omp-envd measurement

With the rebuilt image pinned in `sv_exec_remote_image`:

```sh
omp adw run g1 \
  --exec-policy remote-only \
  sv_exec_remote_endpoint=<REAPI endpoint> \
  sv_exec_remote_image=ghcr.io/blyzer/omp-reapi@<DIGEST> \
  sv_exec_remote_profile=buildbuddy \
  sv_exec_remote_credential=!<credential-env-var>
```

Record timing and pass/fail in `docs/audits/remote-verification-trial.md`.
