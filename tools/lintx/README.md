# lintx

omp's repo lint engine, a standalone Cargo workspace (its `ra_ap_syntax`
dependency tree must not enter the root workspace). It parses each `.rs` file
with `ra_ap_syntax` and runs syntax-level rules that clippy cannot express.

```sh
just lintx [paths]          # default pass over `crates`: path/style/model-name rules
just lintx-fix [paths]      # conservative autofix of path rules
just lintx-ratchet          # error-formatting ratchet (below); also part of `just lint`
just lintx-ratchet-update   # lower the ratchet baseline
just lintx-test             # lintx's own unit tests
```

Structure: one module per rule family under `src/lints/`, each implementing
`Lint` (typed findings, erased by the engine); `src/ratchet.rs` is the count
ratchet; `src/main.rs` is the CLI.

## Error-formatting ratchet (ADR 0035)

`AGENTS.md` ("Composition/errors/state") rejects string-payload error variants
and errors passed through formatters. About 175 variants and several hundred
sites predate the rule, so two lintx rules **count** them per crate and
compare with a committed baseline, `baselines/error-formatting.toml`. New
violations fail; old ones migrate when the code is touched. The rules are not
in the default `lintx` pass (hundreds of findings would drown it).

### `error-str-payload`

A struct, or an enum variant, annotated `#[error("…")]` (not
`#[error(transparent)]`) whose **only field is a string**:

- a tuple field typed `Str`, `String`, `&str` or `&'static str`:
  `#[error("bad {0}")] Bad(Str)`;
- a record field of such a type named like a catch-all (`reason`, `message`,
  `msg`, `detail`, `details`, `text`, `error`, `cause`, `description`,
  `context`, `why`, `info`): `#[error("bad: {reason}")] Bad { reason: Str }`.

Not counted: a field marked `#[from]`/`#[source]`; a record variant with an
identifying field name (`Missing { id: Str }`); a variant with more than one
field; variants without `#[error]`. Some tuple payloads are identifiers rather
than stringified errors, so the count is an upper bound.

### `error-format`

An error value passed through a formatter. An *error binding* is the first
parameter of a closure given to `map_err`, `or_else`, `unwrap_or_else`,
`inspect_err` or (first closure only) `map_or_else`, or the identifier in an
`Err(ident)` pattern of a `match` arm or `if let`. Inside that binding's scope
(closure body, arm expression, `then` block) one finding is counted per:

- `<binding>.to_string()`;
- `format!`, `sf!` or `fmts!` whose arguments name the binding
  (`format!("x: {}", e)`) or inline-capture it (`sf!("x: {e}")`, `{e:?}`).

Not counted: `tracing` macros, `write!` into a `Display` impl, formatting of
values that were not bound as an error, `.map_err(|e| format!("static"))`.

### Scope and escapes

Counted per crate (the directory under `crates/`). Test code is out of scope:
`#[test]` functions, items gated by `#[cfg(test)]`, `tests/`, `benches/` and
`examples/` trees, `build.rs`, and `tests.rs` / `*_tests.rs` files. A site
that is the genuine render-once boundary may carry
`// lintx-allow: error-format <reason>` (or `error-str-payload`) on its line or
the line above; such sites are not counted.

### Baseline and how to lower it

`baselines/error-formatting.toml` holds `[rule]` sections of `crate = count`;
a crate or rule not listed has a baseline of zero.

- `just lintx-ratchet` (CI job "Error-formatting ratchet (lintx)", and part of
  `just lint`) exits 1 when any crate's count is above its baseline and prints
  that crate's findings. Counts below the baseline pass with a reminder.
- After migrating sites, run `just lintx-ratchet-update`. It rewrites the file
  with the lowered counts (crates reaching zero are dropped) and **refuses,
  writing nothing, if any count is above the baseline**: the file can only go
  down. A missing file is created from the current tree, which is the one-time
  bootstrap and not a way to raise an existing baseline. Commit the lowered
  file in the same change as the migration.
- Raising a baseline by hand is a review-reject; fix the new site, or mark a
  true boundary with `lintx-allow` and a reason.
