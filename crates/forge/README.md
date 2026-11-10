# omp-forge

Forge-neutral contract for change requests, issues, and repository refs
(ADR 0049 D2).

A caller hands a `pr://` or `issue://` URL to `parse_resource_url` and gets a
host, an owner, a repository name, and a number. `ForgeOp::mutates` says which
operations write. `required_checks` and `required_satisfied` decide whether the
checks on a change request are green. A forge implements `ForgeHost`; the
trait is the read and write surface for repositories, change requests, and
issues.

## Structural philosophy

The crate is pure. It performs no I/O, opens no connections, and holds no
credentials. Host strings are opaque data. The crate names no vendor, so a
second forge can implement the same trait without a vocabulary change.

`pr://` is the change-request scheme and `issue://` is the issue scheme. Those
spellings stay as they are; the parsed value is a forge resource, not a
vendor object. A host is present only when the path carries one
(`pr://code.example/acme/widget/42`). The shorthand `pr://acme/widget/42`
leaves the host empty. This crate never fills a default host.

Mutation flags live on `ForgeOp`, not on an implementor. Creating, checking
out, and pushing a change request mutate, and so does creating an issue. Reads
do not. Approval uses the flag, so an adapter cannot mark a write as a read.

Checkout and push are repository mutations. The types name the request and the
receipt. Performing the worktree update stays with the core composition that
implements the trait; this crate does not touch a worktree.

The trait is not object-safe. Futures are unboxed (RPITIT), the same shape as
`ExecHost`. A composing crate holds a concrete host.

## Why this crate is T1

ADR 0049 D2 places the forge contract in core. It meets the T1 test because
`pr://` and `issue://` are first-class schemes with selectors, and checkout and
push coordinate repository mutation. The crate does not hold credentials.
Vendor adapters stay outside the workspace and are not part of this crate.
