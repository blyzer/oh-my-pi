# omp-ci-delegated

In-core host router over the CI contract (ADR 0049 D2.2, D4).

A composer registers one adapter per provider host, together with the
`Capabilities` that adapter advertises. `Delegated` implements `CiHost` and
forwards each call to the adapter whose host matches the request's
`CiProvider`. Host lookup is ASCII case-insensitive.

A call site that has a details URL uses `Delegated::resolve`. That calls
`route` from `omp-ci` and then the same registry. `route` lowercases the
host and keeps a named port beside it, so the registry key is the host and
not `host:port`. `CiHost` methods take a `CiProvider`, a `RunRef`, or a
`Trigger` and route on that embedded provider id. A provider id that
literally contains a port is a different key, and `route` does not produce
it.

Writes consult an `ApprovalGate` before the adapter runs. The router picks
the `CiOp` from the method it is implementing and uses `CiOp::mutates` from
`omp-ci`. The gate runs only when the capabilities stored at registration
`allows` the operation. Reads do not ask the gate. `artifacts` and
`log_stream` still require their capability flags. `status` is always
allowed. `capabilities` is forwarded and is not a `CiOp`.

The registered flags are the ceiling. The router does not call
`capabilities` again to decide a write or a gated read. An adapter cannot
widen that ceiling by reporting a broader set later. The composer passes
the advertisement the adapter reports at registration.

Scope checks (`RunRef::pipeline_scope`, `RunRef::job_scope`) stay on the
adapter. The router forwards the ref it was given.

## Structural philosophy

The crate performs no I/O. It opens no connections, mints no credentials,
and does not talk to a project environment or a tool roster. Adapters are
values the composer supplies. Two in-memory hosts in the tests prove that a
request for one host does not reach the other. They do not prove a second
product.

`CiHost` returns an unboxed future per method, so the trait is not
object-safe. `Delegated<H, G>` keeps that surface and stores `Arc<H>`.
Every registered adapter is the same `H`. A composer with more than one
concrete adapter type erases those types into one `H` at registration (an
enum of the adapters it actually holds). The `Arc` is taken once there.
A call clones the `Arc` and does not box a future.

The approval gate is local to this crate. It is not shared with the forge
router.

The router is not a stable contract until a second real extension host
exists (ADR 0049 D2.8). Until then the in-memory hosts prove the router
only.

## Why this crate is T1

ADR 0049 D2 puts one delegated adapter per neutral contract in core. This
crate is that adapter for CI. Mutation flags stay on the contract, and the
approval check stays here, so vendor code cannot reclassify a write.
Capability flags stay on the contract too. The router enforces the set the
composer registered, so vendor code cannot advertise a broader ceiling
after the fact. The crate does not hold credentials. Vendor adapters stay
outside the workspace.
