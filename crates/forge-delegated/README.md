# omp-forge-delegated

In-core host router over the forge contract (ADR 0049 D2.2, D4).

A composer registers one adapter per forge host. `Delegated` implements
`ForgeHost` and forwards each call to the adapter whose host matches the
request's `RepoRef`. Host lookup is ASCII case-insensitive. A port, when the
host string has one, is part of the key.

Writes consult an `ApprovalGate` before the adapter runs. The router picks
the `ForgeOp` from the method it is implementing and uses
`ForgeOp::mutates` from `omp-forge`. Reads do not ask the gate. An adapter
cannot mark a write as a read, because the router never reads that
classification from adapter data.

## Structural philosophy

The crate performs no I/O. It opens no connections, mints no credentials,
and does not talk to a project environment or a tool roster. Adapters are
values the composer supplies. Two in-memory hosts in the tests prove that a
request for one host does not reach the other. They do not prove a second
product.

`ForgeHost` returns an unboxed future per method, so the trait is not
object-safe. `Delegated<H, G>` keeps that surface and stores `Arc<H>`.
Every registered adapter is the same `H`. A composer with more than one
concrete adapter type erases those types into one `H` at registration (an
enum of the adapters it actually holds). The `Arc` is taken once there.
A call clones the `Arc` and does not box a future.

The router is not a stable contract until a second real extension host
exists (ADR 0049 D2.8). Until then the in-memory hosts prove the router
only.

## Why this crate is T1

ADR 0049 D2 puts one delegated adapter per neutral contract in core. This
crate is that adapter for forge. Mutation flags stay on the contract, and
the approval check stays here, so vendor code cannot reclassify a write.
Checkout and push still name repository mutation; performing the worktree
update stays with the core composition that calls this router. The crate
does not hold credentials. Vendor adapters stay outside the workspace.
