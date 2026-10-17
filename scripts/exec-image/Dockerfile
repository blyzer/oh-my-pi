FROM rust:1-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git python3 pkg-config build-essential \
 && rm -rf /var/lib/apt/lists/*

RUN cargo install --locked just \
 && cargo install --locked cargo-nextest --version 0.9.146 \
 && ln -sf /usr/local/cargo/bin/just /usr/local/bin/just \
 && ln -sf /usr/local/cargo/bin/cargo-nextest /usr/local/bin/cargo-nextest

ENV PATH=/usr/local/cargo/bin:/usr/local/rustup/bin:/usr/local/bin:/usr/bin:/bin
WORKDIR /workspace
CMD ["just", "--version"]
