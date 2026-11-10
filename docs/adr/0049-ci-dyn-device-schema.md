# Dyn `ci/*` device schema (ADR 0049 follow-up to D2.3)

Source of truth for the T2 CI device bridge. Rustdoc on
`omp_envd::github_adapter` (`dyn_ci`) and `omp-ci` point here.

T2 CI backends will implement the [`CiHost`](../../crates/ci/src/host.rs) leaf
set by answering `omp.devices.call` on `ci/<op>` paths. Core holds a
[`DynCiAdapter`](../../crates/envd/src/github_adapter/dyn_ci.rs) that
dispatches each `CiHost` method to those paths. The production router holds
an enum `CiBackend { Github(...), Dyn(...) }` so heterogeneous hosts stay
unboxed (AGENTS: enums before `dyn`).

This is the CI counterpart of [`0049-forge-dyn-device-schema.md`](0049-forge-dyn-device-schema.md).
Flipping `github.com` CI fully onto Dyn waits on a T2 CI extension body; until
then in-core [`GithubCiAdapter`](../../crates/envd/src/github_adapter/mod.rs)
remains the default for `github.com`.

## Device paths

Primary paths are `ci/` + [`CiOp`](../../crates/ci/src/op.rs) snake_case
(`strum` / serde), one path per `CiHost` method that names an op. The
`capabilities` method is not a `CiOp`; it uses a dedicated path so an
extension can report flags without inventing a fake op.

| Device path | `CiOp` (approval / caps) | `CiHost` method | Args (JSON) | Result (JSON) |
|---|---|---|---|---|
| `ci/capabilities` | *(not a `CiOp`)* | `capabilities` | `{ "provider": CiProvider }` | `Capabilities` |
| `ci/status` | `status` | `status` | `{ "run": RunRef }` | `Pipeline` |
| `ci/artifacts` | `artifacts` | `artifacts` | `{ "run": RunRef }` | `ArtifactPage` |
| `ci/log_stream` | `log_stream` | `log_stream` | `{ "run": RunRef }` | `LogPage` |
| `ci/cancel` | `cancel` | `cancel` | `{ "run": RunRef }` | `Pipeline` |
| `ci/rerun_failed` | `rerun_failed` | `rerun_failed` | `{ "run": RunRef }` | `Pipeline` |
| `ci/rerun_job` | `rerun_job` | `rerun_job` | `{ "run": RunRef }` | `Job` |
| `ci/trigger` | `trigger` | `trigger` | `{ "request": Trigger }` | `Pipeline` |
| `ci/play_manual` | `play_manual` | `play_manual` | `{ "run": RunRef }` | `Job` |

Types are the serde shapes of `omp_ci`. Prefer those structs; do not invent a
parallel DTO vocabulary.

## Capabilities stay in core Delegated

Registration stores a [`Capabilities`](../../crates/ci/src/provider.rs)
ceiling. `omp-ci-delegated` enforces `Capabilities::allows` **before** the
adapter runs (and before write approval). Dyn device answers cannot widen
that ceiling: a leaf that implements cancel still yields `NotCapable` when
registration did not set `cancel`. Production GitHub registration remains
`log_stream` only (status always allowed). Dyn hosts registered today use
the same ceiling unless a caller passes a different set to
`register_dyn_ci_host`.

## Registration seam

`register_dyn_ci_host(builder, host, client, capabilities)` registers
`CiBackend::Dyn(DynCiAdapter::new(client))` for an ASCII host name
(e.g. `gitlab.com`, a test host).

`CiDeviceClient` is the injectable invoke surface. Production uses
`LiveCiDeviceClient` (late-bound `CiDeviceInvoker`);
`DeviceControlFactory::ci_invoker` looks up mounted leaves and dispatches
`omp.devices.call` (same destination as `DeviceControlFactory::invoke`, via
`NestedCallbackDispatcher::dispatch_host`). Tests may inject a fake client or
a recording invoker.

Production `ci_router`:

- keeps `github.com` (+ unlisted checkout host) on the **Github** arm so
  `ci@1` is unchanged;
- registers documented Dyn host [`CI_DYN_HOST`](../../crates/envd/src/github_adapter/dyn_ci.rs)
  (`gitlab.com`) on the Dyn arm with the same live client (Unavailable until
  a T2 CI extension mounts `ci/*`);
- does **not** flip `github.com` CI to Dyn (T2 CI extension body is out of
  scope for this bridge PR).

## Evidence

Unit tests in `crates/envd/src/github_adapter/dyn_ci.rs` prove Delegated
routes a dyn host through a fake / live client for `status` / `log_stream`
(the ops `GithubCiAdapter` supports today). `ci_router` still advertises
`log_stream` only for `github.com`.

## Status vs deleting `github.rs`

- **DONE:** `ci/*` dyn bridge (`DynCiAdapter`, `CiBackend`, live client,
  `register_dyn_ci_host`, production invoker under `cap-ci`).
- **Remaining before deleting `envd/src/github.rs`:** T2 CI extension body +
  flipping `github.com` CI to Dyn; Dyn forge checkout/push (ADR D2.4);
  list-URL / `GithubResolver` parity. In-core `GithubCiAdapter` /
  `GithubForgeAdapter` / `github.rs` / `github_url.rs` stay until those land.
