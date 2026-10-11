# omp-exec

The backend-neutral domain for running build and test commands: where a
command may run and why, decided in one place (ADR 0040, Part B).

A caller hands a `BuildCommand` (argv, cwd, intent, the phase's
`omp_adw::Requirement`) to a `BuildSystemAdapter`, which uses build-system
knowledge and the repository pins (`RepoPins`) to produce an `ExecutionPlan`
of `ExecutionRequest`s. `place()` then decides, as a pure function, whether
each request runs locally or on one of the offered backends, from the
request's `Eligibility`, the operator's `ExecutionPolicy`, and each backend's
`BackendCapabilities`. A backend implements `ExecutionBackend`; its
`ExecutionRun` streams `ExecutionEvent`s and settles with an
`ExecutionResult` or a typed `ExecutionError`.

Slice 1 ships one adapter, `CargoJust`, with a checked-in needs table for this
repository's `justfile` recipes, plus the default upload deny globs that
working-tree capture applies.

## Structural philosophy

The crate is pure: no I/O, no clock, no spawning, and no vendor names.
Capture and blob reads live in `omp-envd`; remote clients and the backend enum
are composed by `omp-driver`. Keeping the domain here means placement and
failure classification are testable as plain functions, and a new backend or
a new build system is added without touching the other side.

Two separations are load-bearing:

- **Adapters never name a backend, and backends never inspect argv.** The
  adapter derives eligibility, needs, toolchain, and named caches from the
  command and the repository; the backend maps those to images and platform
  properties. A Maven or Go adapter is a new adapter, not a backend change; a
  new remote service is a new backend, not an adapter change.
- **Admission uses what a backend enforces, never what it intends.** `place()`
  admits a backend only when its enforced `omp_adw::Posture` satisfies the
  request's `Requirement` and it offers the platform, every need, and the
  network the command needs.

Failures are typed. A non-zero exit is an `Outcome`, not an error.
`fallback_eligible` classifies every `(ExecutionError, Stage)` pair in one
exhaustive match with no wildcard arm, so a new error variant does not compile
until it is classified, and nothing is eligible after the backend accepted the
work.

The traits are never used as `dyn`: futures are unboxed (RPITIT), and the
composing crate holds one enum over its concrete backends.
