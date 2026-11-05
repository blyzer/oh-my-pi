# omp-minimizer

`omp-minimizer` condenses a shell command's captured stdout/stderr before the
model reads it. It is a port of oh-my-pi's `pi-shell` minimizer; `NOTICE`
records the provenance and the RTK attribution that travels with it.

`apply(command, captured, exit_code, &config)` returns a `MinimizerOutput`
whose text borrows the capture whenever nothing changed. The engine is inert
unless `MinimizerConfig::enabled` is set, and no output path calls it yet.

## Structure

- `plan` classifies the command (single, piped, chain, compound, unsupported)
  with `omp_shell::parser`, so chain segments split where the engine splits
  them.
- `detect` names the program and subcommand used for dispatch.
- `engine` guards size and structure, then dispatches the capture to a Rust
  filter or a declarative pipeline; a panicking filter means passthrough.
- `filters` holds the Rust filters behind one dispatch table: `cargo`, and
  `generic` as the fallback.
- `pipeline` compiles TOML filter definitions. `build.rs` concatenates
  `src/defs/*.toml` into the built-in set; the `sv_minimizer_filters` file
  adds user definitions, which win.
- `primitives` holds the pure `&str` transforms the filters share.
- `config` resolves the `sv_minimizer_*` convars into a `MinimizerConfig`.

## Philosophy

Minimization must never cost the agent output it needs. Shapes the engine
cannot reason about pass through unchanged, filters fail safe, and every
rewrite hands back the original capture so the caller can keep it.
