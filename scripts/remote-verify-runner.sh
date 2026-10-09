#!/usr/bin/env bash
# remote-verify-runner.sh: the script scripts/remote-verify.sh sends to a BuildBuddy remote runner
# (ADR 0040, Slice 0). It runs on the runner, in the mirrored checkout: it prepares the toolchain
# the repository pins, then runs `just $OMP_REMOTE_JUST_ARGS`.
#
# Everything the runner should keep between runs lives under $HOME, which the VM snapshot carries:
# `bb remote` runs `git clean -x -d --force` in the checkout before every run, so target/ and vendor/
# inside it would be rebuilt from scratch each time.
set -euo pipefail
# bb remote exports these to every process of the run (ADR 0040, Part H.2); drop them first.
unset BUILDBUDDY_API_KEY REPO_TOKEN REPO_USER

stamp() { printf '\n=== %s %s\n' "$(date -u +%H:%M:%S)" "$*"; }

stamp "runner: $(uname -sm), $(nproc) cpus, $(free -g | awk '/Mem:/ {print $2 " GB"}')"
git status --short | head -20 # the local changes bb mirrored, if any

stamp "toolchain"
if ! command -v cmake >/dev/null || ! command -v ninja >/dev/null; then
    sudo apt-get update
    sudo apt-get install -y build-essential cmake ninja-build zstd curl git python3 rsync
fi
if [ ! -x "$HOME/.cargo/bin/rustup" ]; then # the runner user is the non-root `buildbuddy`
    curl --proto '=https' -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
fi
. "$HOME/.cargo/env" # rust-toolchain.toml installs the pinned nightly on first use
command -v just >/dev/null || cargo install --locked just
# Prebuilt: building nextest pulls aws-lc-sys, which refuses older images' gcc (GCC bug 95189).
cargo nextest --version 2>/dev/null | grep -q '0\.9\.146' ||
    curl -LsSf https://get.nexte.st/0.9.146/linux | tar zxf - -C "${CARGO_HOME:-$HOME/.cargo}/bin"
export PATH="$HOME/.local/bin:$PATH"
command -v uv >/dev/null || curl -LsSf https://astral.sh/uv/install.sh | sh # fetch-python.sh needs uv

export CARGO_TARGET_DIR="$HOME/.cache/omp-target"
mkdir -p "$HOME/.cache/omp-vendor"
[ -L vendor ] || ln -sfn "$HOME/.cache/omp-vendor" vendor
[ -f vendor/python/pyo3-config.txt ] || crates/py/scripts/fetch-python.sh

stamp "just ${OMP_REMOTE_JUST_ARGS}"
start=$(date +%s)
status=0
# shellcheck disable=SC2086 # the recipe and its arguments are words on purpose
just ${OMP_REMOTE_JUST_ARGS} || status=$?
stamp "exit ${status} after $(($(date +%s) - start))s; target $(du -sh "$CARGO_TARGET_DIR" | cut -f1)"
exit "$status"
