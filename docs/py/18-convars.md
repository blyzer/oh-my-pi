# Convars: extension-declared settings on the shared control plane

This document owns `omp.convars` — `declare`, `get`, `observe`, and the two values they
exchange, `omp.convars.Snapshot` and `omp.convars.Observation`. Convars themselves are an
engine concept older than this surface: typed, named configuration variables owned by
`crates/con`, persisted through `~/.o2/config.cfg`, and described as a design in
[ADR 0012](../adr/0012-convars.md). What this page defines is the *extension-facing* slice of
them — how a Python extension declares one, reads it, and follows it as it changes.

The distinction matters because the three operations here are CONTROL, not DATA. They travel
the control socket, Core enforces their `minimum_phase` ([00-overview.md](00-overview.md)),
and none of them touches the Environment. Declaring a setting is not a filesystem effect and
does not wait on `EFFECTS_AUTHORIZED`.

## Identity

An extension never names a convar globally. `declare` takes a bare `key` and the host
qualifies it with the authenticated extension identity, so two extensions may both declare
`verbosity` without collision. The qualified name is what `Snapshot.name` carries, and it is
the name `get` and `observe` expect. Read the name back from the declaration rather than
constructing it.

Keys are validated by the host: a key must be trimmed and must not contain `::`. A key that
is not is rejected with `InvalidConvarDeclaration`.

## `omp.convars.Snapshot`

One effective value at a monotonic change sequence. Frozen, slotted dataclass.

| Attribute | Type | Semantics |
|---|---|---|
| `name` | `str` | Qualified convar name, as the host resolved it. |
| `kind` | `str` | Declared type: `boolean`, `number`, `string`, or `enum`. |
| `value` | `object` | The effective value, already typed by the engine. |
| `sequence` | `int` | Monotonic, non-negative. Increments on each committed change. |

`sequence` is the cursor `observe` resumes from; it is not a timestamp and carries no meaning
across different convars.

## `omp.convars.declare(key, *, kind, default, description=None, values=(), ui=None) -> Snapshot`

Declares one extension-owned dynamic convar and returns its current snapshot.

| Parameter | Type | Semantics |
|---|---|---|
| `key` | `str` | Unqualified, non-empty, trimmed, no `::`. |
| `kind` | `str` | One of `boolean`, `number`, `string`, `enum`. |
| `default` | `object` | Required. Must match `kind`. |
| `description` | `str \| None` | Shown in settings UI. Defaults to a generated sentence naming the key and the declaring extension. |
| `values` | `Sequence[str]` | Required for `enum`, rejected for every other kind. |
| `ui` | `Mapping[str, object] \| None` | Presentation metadata for the settings surface. |

The declared variable carries the `ARCHIVE`, `SESSION` and `REPLICATED` flags, so a *value*
set against it persists to the user's configuration and replicates like any other convar. The
*declaration* does not persist: it registers a spec in the running host, and an extension
repeats it on each activation.

**Idempotent, and strict about it.** Re-declaring an identical spec is a no-op that returns
the current snapshot — which is what makes repeating the call after a reconnect safe. A
declaration that differs from the admitted one in any field is rejected with
`ConvarDeclarationConflict`; the host does not silently adopt the new shape.

```python
setting = await omp.convars.declare(
    "verbosity",
    kind="enum",
    default="normal",
    values=("quiet", "normal", "loud"),
    description="How much detail the reviewer writes.",
)
```

### Errors

| Condition | Raised |
|---|---|
| `key` empty or not a string | `TypeError` |
| `kind` not one of the four | `ValueError` |
| `enum` without `values`, or `values` on a non-enum | `ValueError` |
| `values` containing a non-string or empty string | `TypeError` |
| `description` not `str \| None`, `ui` not a mapping | `TypeError` |
| No control backend bound | `omp.NotWiredError` |
| Key untrimmed or containing `::` | `InvalidConvarDeclaration` |
| Spec differs from the admitted declaration | `ConvarDeclarationConflict` |

## `omp.convars.get(name) -> Snapshot`

Reads one declared convar by canonical name. The name may be a harness convar or another
extension's — `get` is a read of the shared control plane, not of your own declarations only.

```python
snapshot = await omp.convars.get("sv_interrupt_grace")
```

Raises `TypeError` for an empty or non-string name, `omp.NotWiredError` with no control
backend, and a convar error if the name is not declared.

## `omp.convars.observe(name) -> Observation`

Returns an async iterator over the current value and every subsequent committed change.
`observe` itself is synchronous and performs no request; each `__anext__` issues one
long-polling `omp.convars.observe` request.

The cursor is the `sequence` of the last snapshot yielded. The first iteration returns
immediately with the current value. Each later iteration blocks until a change is committed
with a higher sequence, so a consumer never misses a commit and never re-reads one.

```python
async for snapshot in omp.convars.observe(setting.name):
    reconfigure(snapshot.value)
```

The stream is a broadcast subscription. A consumer that falls far behind is skipped past the
lag rather than disconnected. If the host's change stream closes, `__anext__` raises
`ConvarObservationClosed`, marked retryable — re-entering the loop is the correct response.

## Specification

| Operation | Phase | Durability | Cost | Authority |
|---|---|---|---|---|
| `omp.convars.declare` | `OPEN` | Ephemeral | Metered | Core |
| `omp.convars.get` | `OPEN` | Ephemeral | Metered | Core |
| `omp.convars.observe` | `OPEN` | Ephemeral | Metered | Core |

All three are CONTROL, so Core enforces the phase ([00-overview.md](00-overview.md)). The
authority that owns them enforces operation ownership only and gates no phase, which is what
`OPEN` records: nothing prevents a declaration at import time, and extensions declare during
activation.

None is durable. `get` and `observe` are reads. `declare` mutates only the running host's
registry — the flags it sets govern how a *value* is persisted, not the declaration — and the
contract that an extension repeats the call after reconnect is what a non-durable
registration means. Consistently, none of the three carries the idempotency key that
[00-overview.md](00-overview.md) requires of a durable request; `declare`'s idempotency is
structural, by spec equality, not by key.

Each is a host round trip returning a bounded snapshot, which is what `Metered` records.
