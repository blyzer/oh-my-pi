# First-party T2 extensions

This tree is the **first-party Tier-2 extensions home** for omp2 (ADR 0049 D5,
owner decision 2026-11-10).

## What this is

Vendor adapters and maintained long-tail workflows live here as Python
extensions (wheels locked and signed via `crates/ext`). The layout matches
early omp2 / can1357 upstream: **same git repository**, not a separate GitHub
remote.

## Zero Cargo weight

`extensions/` is **not** a Cargo workspace member. Root `Cargo.toml` keeps:

```toml
members = ["crates/*"]
```

There is no `extensions/Cargo.toml`. Adding or editing trees here must not
change workspace membership or pull Rust crates into the `omp` build graph.

## Install path

Development installs use `path:` sources and `omp ext link` against a directory
under this tree (see `docs/pyx/getting-started/installation.md`). Reproducible
installs package wheels; the registry/lock/signing path stays in `crates/ext`.

## Wave 1

See [WAVE1.md](WAVE1.md). The first adapter is the GitLab forge host (ADR 0049
D2.8) under [`gitlab/`](gitlab/). Further ports are TBD.
