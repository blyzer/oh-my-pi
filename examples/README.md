# Extension examples

This directory currently holds no examples. It used to hold what the old index counted as 119 ported
extensions (one directory each: an `omp.toml`, the Python module, and a README with a **Gaps**
section), but commit `00611047c1` ("removed legacy example scripts and related configuration
files") deleted them all (367 files). Only this README remains.

Several ports were written against APIs that no longer exist (`@omp.regime`, `omp.journal.append` /
`@omp.entry_kind`; see `docs/py/15-directors.md` and `docs/py/09-journal.md`), and none was
re-verified against the current surface, so treat them as history rather than working examples.
They are still readable:

```sh
git show 00611047c1^:examples/README.md   # the old index
git ls-tree -r --name-only 00611047c1^ -- examples/   # every removed file
```

## Where extension code lives now

- **QA fixture extensions**: `scripts/qa/fixtures/extensions/` holds 37 small extensions in 13
  groups (`agents`, `context`, `devices`, `env`, `hooks`, `introspect`, `policy`, `provider`,
  `regimes`, `root`, `storage`, `telemetry`, `ui`), each with an `omp.toml` and a Python package
  under `src/`. They are test fixtures driven by `scripts/qa/cases/`, not documentation, and some
  target removed APIs (`regimes/qaregime`, `context/journal`). Read `scripts/qa/README.md` before
  relying on one.
- **Frozen surface contract**: `crates/py/tests/*.rs` declare and exercise the current package
  (`crates/py/python/omp`), including `@omp.director` and `@omp.component`
  (`crates/py/tests/extension_registrar_contract.rs`).
- **API reference**: `docs/py/` (start at `docs/py/00-overview.md`).

Add a new example here only if it runs against the current frozen surface, and keep an `omp.toml`
manifest beside the module so it can be installed as an extension.
