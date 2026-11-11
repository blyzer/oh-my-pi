# omp-ci

CI-neutral contract for pipelines, jobs, and details-URL routing
(ADR 0049 D2).

A caller hands an absolute `http` or `https` details URL to `route` and gets
the opaque provider host plus the path an adapter interprets. `CiOp::mutates`
says which operations write. `Capabilities` says which of those operations a
provider implements. A backend implements `CiHost`. The trait splits reads
(status, artifacts, log stream) from writes (cancel, rerun, trigger, play
manual).

## Structural philosophy

The crate is pure. It performs no I/O, opens no connections, and holds no
credentials. Provider ids and hosts are opaque data. The crate names no
product, so a second backend can implement the same trait without a
vocabulary change.

`route` does not invent a URL scheme and does not match the host against a
product list. It rejects userinfo so a credential cannot ride inside a route.
A host is lowercased. A port is recorded only when the URL named one; a
default port is never filled in.

Mutation flags live on `CiOp`, not on an implementor. Cancel, rerun, trigger,
and play-manual write. Status, artifacts, and log stream read. Approval uses
the flag, so an adapter cannot mark a write as a read. `Capabilities::allows`
is the same vocabulary: status is always allowed, and every other operation
is gated by the matching flag.

## Why this crate is T1

ADR 0049 D2 places the CI contract in core. The evidenced reason is the
mutation vocabulary a delegated adapter approval-gates, and `route` as the
host selector for that adapter. The crate does not hold credentials. Vendor
adapters stay outside the workspace and are not part of this crate.
