# omp-py-link

Build-script support for the final link of any binary that embeds `omp-py`'s
static CPython.

## Why this crate exists

Two link arguments cannot propagate from `omp-py`'s build script to the
binaries that depend on it, because Cargo's `rustc-link-arg` applies to the
crate being built and nothing downstream:

- `--ld-path=<shim>`, when the vendored Python tree carries LLVM LTO bitcode
  that Xcode's `ld64` cannot read (the `needs-lld` marker).
- the CPython export list, so native wheels resolve the C-API from the host
  executable at `dlopen`.

Every consumer therefore has to emit them itself. Before this crate, four
build scripts — `omp-py`, `omp-app`, `omp-tools`, `omp-e2e` — each carried
its own copy of the same forty lines, and the copies had already drifted: two
of them emitted `ld64`'s `-export_dynamic` spelling on every platform, which
GNU `ld` and LLD parse as `-e xport_dynamic` and link a binary with no valid
entry point. That defect is the structural argument for this crate. A link
contract replicated by hand in four places is a contract that will be wrong in
at least one of them.

## Structural philosophy

One function, `emit`, and no configuration. Everything it needs it derives:

- The **vendor tree** comes from `PYO3_CONFIG_FILE`, the same single env var
  that pins pyo3, resolved leniently — a consumer that has not run
  `crates/py/scripts/fetch-python.sh` gets no flags rather than a panic,
  because `omp-py` itself already fails that case loudly and with a better
  message.
- The **link inputs** are found relative to this crate's own compile-time
  manifest directory, never the caller's, so every consumer resolves the same
  files regardless of where it sits.
- The **consumer's name**, used in diagnostics, comes from `CARGO_PKG_NAME`,
  which Cargo sets to the package whose build script is running.

The inputs themselves stay in `crates/py`, which owns the vendored
interpreter: `link/cpython.dynamic-list`, `link/cpython.macho-list` and
`scripts/ld64.lld`. This crate applies them; it does not own them.

## Use

```toml
[build-dependencies]
omp-py-link.workspace = true
```

```rust
fn main() {
    omp_py_link::emit();
}
```
