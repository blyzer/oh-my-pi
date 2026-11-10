# Dyn `ci/*` device schema (ADR 0049 follow-up to D2.3)

Source of truth for the T2 CI device bridge. Rustdoc on
`omp_envd::github_adapter` (`dyn_ci`) and `omp-ci` point here.

T2 CI backends implement the [`CiHost`](../../crates/ci/src/host.rs) leaf
set by answering `omp.devices.call` on `ci/<op>` paths. Core holds a
[`DynCiAdapter`](../../crates/envd/src/github_adapter/dyn_ci.rs) that
dispatches each `CiHost` method to those paths. The production router holds
an enum `CiBackend { Github(...), Dyn(...) }` so heterogeneous hosts stay
unboxed (AGENTS: enums before `dyn`).

This is the CI counterpart of [`0049-forge-dyn-device-schema.md`](0049-forge-dyn-device-schema.md).

**Mount collision:** `DynCiAdapter` calls fixed paths `ci/<op>`. Only
[`extensions/ci/`](../../extensions/ci/) (`omp.ci`) declares the `ci` parent
and mounts leaves. [`extensions/github/`](../../extensions/github/) and
[`extensions/gitlab/`](../../extensions/gitlab/) are backends under
`omp_github.ci` / `omp_gitlab.ci` — no competing `ci` parents.

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

**Mounted by `omp.ci` today:** `ci/capabilities`, `ci/status`,
`ci/log_stream`. Other paths stay unmounted →
`CiError::Unavailable` via the live client.

## Capabilities stay in core Delegated

Registration stores a [`Capabilities`](../../crates/ci/src/provider.rs)
ceiling. `omp-ci-delegated` enforces `Capabilities::allows` **before** the
adapter runs (and before write approval). Dyn device answers cannot widen
that ceiling: a leaf that implements cancel still yields `NotCapable` when
registration did not set `cancel`. Production registration remains
`log_stream` only (status always allowed). Dyn hosts registered today use
the same ceiling unless a caller passes a different set to
`register_dyn_ci_host`.

## Registration seam

`register_dyn_ci_host(builder, host, client, capabilities)` registers
`CiBackend::Dyn(DynCiAdapter::new(client))` for an ASCII host name
(e.g. `github.com`, `gitlab.com`, a test host).

`CiDeviceClient` is the injectable invoke surface. Production uses
`LiveCiDeviceClient` (late-bound `CiDeviceInvoker`);
`DeviceControlFactory::ci_invoker` looks up mounted leaves and dispatches
`omp.devices.call` (same destination as `DeviceControlFactory::invoke`, via
`NestedCallbackDispatcher::dispatch_host`). Tests may inject a fake client or
a recording invoker.

Production `ci_router`:

- registers [`CI_GITHUB_HOST`](../../crates/envd/src/github_adapter/dyn_ci.rs)
  (`github.com`) and [`CI_DYN_HOST`](../../crates/envd/src/github_adapter/dyn_ci.rs)
  (`gitlab.com`) on the **Dyn** arm with the same live client;
- keeps unlisted checkout hosts on the **Github** arm (GHE);
- does **not** delete in-core [`GithubCiAdapter`](../../crates/envd/src/github_adapter/mod.rs)
  / `github.rs` (list-URL, forge checkout/push, GHE CI still need them).

## Evidence

Unit tests in `crates/envd/src/github_adapter/dyn_ci.rs` and
`github_adapter/mod.rs` prove Delegated routes dyn hosts through a fake /
live client for `status` / `log_stream`, and that production `ci_router`
registers `github.com` on Dyn. Python mapping tests under
`extensions/github/tests` and `extensions/gitlab/tests` cover Actions /
GitLab fixtures → `Pipeline` / `LogPage`. `ci_router` still advertises
`log_stream` only.

## Status vs deleting `github.rs`

- **DONE:** `ci/*` dyn bridge (`DynCiAdapter`, `CiBackend`, live client,
  `register_dyn_ci_host`, production invoker under `cap-ci`).
- **DONE:** T2 CI extension body (`extensions/ci/` + GitHub Actions /
  GitLab CI backends) and flipping `github.com` CI to Dyn.
- **Remaining before deleting `envd/src/github.rs`:** Dyn forge checkout/push
  (ADR D2.4); list-URL / `GithubResolver` parity. In-core `GithubCiAdapter` /
  `GithubForgeAdapter` / `github.rs` / `github_url.rs` stay until those land.
