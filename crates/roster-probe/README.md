# omp-roster-probe

Test capability for the built-in construction table (ADR 0049 Phase 2, ADR 0046 item 1).

`register_probe` is a roster extension. `compose_kernel` selects the roster and passes it on `RegistryBridges`. envd walks the table and calls the extension. This crate is the only place that names `roster_probe`.

The probe is not a production built-in. `SelectedRoster::production()` carries no extension, and `builtin_tool_identities()` does not list it.

## Re-run the exit proof

From the worktree:

```sh
cargo test -p omp-roster-probe --locked
git diff --stat <roster-infrastructure-commit>..HEAD -- crates/envd
```

The test attaches an embedded project environment with `SelectedRoster::production().with_extension(register_probe)` and asserts `registry.live_identity("roster_probe")`. The diff must be empty: adding the probe edits no file under `crates/envd`.
