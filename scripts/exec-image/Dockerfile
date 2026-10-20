# Remote execution image for omp ADW (ADR 0040, Part G).
#
# Every tool needed by `just test-pkg omp-envd` must be baked in: the remote
# action runs with networking disabled (REMOTE_POSTURE.network = Disabled), so
# nothing may be fetched at run time.
#
# Build and pin workflow (run when rust-toolchain.toml, fetch-python.sh TAG/VER,
# or the nextest pin changes):
#
#   docker build -t omp-exec-remote:candidate scripts/exec-image/
#   DIGEST=$(docker inspect --format='{{index .RepoDigests 0}}' omp-exec-remote:candidate \
#            | sed 's/.*@//')
#   docker tag omp-exec-remote:candidate ghcr.io/blyzer/omp-reapi@$DIGEST
#   docker push ghcr.io/blyzer/omp-reapi@$DIGEST
#
# Then update sv_exec_remote_image to the new digest and commit.
# See scripts/exec-image/README.md for the full procedure.

FROM rust:1-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6

# ── System packages ─────────────────────────────────────────────────────────
# Required packages and why:
#   build-essential   C toolchain (cc, ar) for crates with C build scripts
#   cmake             audiopus_sys: builds vendored Opus via cmake::build();
#                     .cargo/config.toml forces CMAKE_GENERATOR=Ninja
#   ninja-build       the Ninja generator cmake uses (CMAKE_GENERATOR=Ninja)
#   pkg-config        audiopus_sys: pkg-config probe for system libopus first
#   ca-certificates   curl TLS verification (fetch-python.sh, uv install)
#   git               cargo build scripts, workspace discovery
#   python3           fetch-python.sh interpreter and pack-pymodules.py
#   curl              fetch-python.sh downloads python-build-standalone archive
#   zstd              fetch-python.sh: `zstd -d` decompresses the .tar.zst
#   rsync             remote-verify-runner.sh: rsync input root into workspace
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        git \
        ninja-build \
        pkg-config \
        python3 \
        rsync \
        zstd \
    && rm -rf /var/lib/apt/lists/*

# ── Rust tooling ─────────────────────────────────────────────────────────────
# just and cargo-nextest are installed from crates.io so their versions match
# the repo pins.  nextest is the prebuilt binary (avoids building aws-lc-sys
# which fails on older gcc, per the comment in remote-verify-runner.sh).
RUN cargo install --locked just \
    && ln -sf /usr/local/cargo/bin/just /usr/local/bin/just

RUN curl -LsSf https://get.nexte.st/0.9.146/linux \
        | tar zxf - -C /usr/local/cargo/bin \
    && ln -sf /usr/local/cargo/bin/cargo-nextest /usr/local/bin/cargo-nextest

# ── uv ───────────────────────────────────────────────────────────────────────
# fetch-python.sh calls `uv pip install` to populate the bundled Python
# packages from requirements.txt.  Install uv before running the script.
RUN curl -LsSf https://astral.sh/uv/install.sh | sh \
    && ln -sf /root/.local/bin/uv /usr/local/bin/uv

# ── CPython bundle ────────────────────────────────────────────────────────────
# omp-envd links CPython statically.  PYO3_CONFIG_FILE must point at a
# pyo3-config.txt produced by fetch-python.sh before cargo begins linking.
#
# The remote action runs with networking disabled (REMOTE_POSTURE.network =
# Disabled), so the bundle cannot be downloaded at run time.  It must be
# baked here.  fetch-python.sh is idempotent: re-running it only refetches
# when the .archive.stamp name does not match (fetch-python.sh:56-64).
#
# The script needs the crates/py subtree and a writable destination.  We
# copy only the subtree needed to avoid a full git clone in the image layer.
COPY crates/py /build/crates/py
RUN /build/crates/py/scripts/fetch-python.sh /opt/omp-python \
    && rm -rf /build

# ── Action environment ────────────────────────────────────────────────────────
# PATH makes just/cargo/rustup findable by REAPI without shell sourcing.
# PYO3_CONFIG_FILE overrides the workspace [env] relative value; the absolute
# path survives rsync into any working directory.
ENV PATH=/usr/local/cargo/bin:/usr/local/rustup/bin:/usr/local/bin:/usr/bin:/bin
ENV PYO3_CONFIG_FILE=/opt/omp-python/python/pyo3-config.txt

WORKDIR /workspace
CMD ["just", "--version"]
