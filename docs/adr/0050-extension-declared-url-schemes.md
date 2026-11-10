# 0050. Extension-declared URL schemes

Status: proposed; requires owner review
Date: 2026-11-10
Area: tools

## Context

[ADR 0027](0027-read-materializes-resources.md) makes `Read` the one materializer for resources, and [ADR 0049](0049-tiered-integration-architecture.md) D2.5 keeps `pr://` and `issue://` forge-neutral. The 2026-11-09 text of both records said the scheme vocabulary does not grow, and that an extension which needs a readable resource uses `mcp://` or a device. That overstates the code. This change softens those sentences to the built-in vocabulary and points the extension path here.

The built-in vocabulary is closed. Schemes an RPC host declares are already readable, through one unknown-scheme fallback, with line selection in core. An installed extension cannot declare a scheme today. This record is the missing declaration path. It does not add a built-in scheme, and it does not add a second resolver.

Prior product behavior being recovered: a read of a resource URI outside the built-in schemes could be served from a mounted MCP server. The omp1 sources for that fallback are not in this repository. This record cites no path or line for them.

### Evidence

Citations in the table are paths on this docs tip, `omp2` at `580d3fb9fb`. The same facts were re-checked at `bf39a439a0` (phase4-scheme, the envd-03 tip after #557). Where that tip moved a symbol, the later location is in the last column. Those split paths are not on this docs tip.

| Fact | This docs tip (`580d3fb9fb`) | At `bf39a439a0` |
|---|---|---|
| Built-in `Scheme` is a closed enum. `Http` accepts the `https` alias. `Unknown` is the variant for a syntactically valid name outside the vocabulary. `ALL` is the dense built-in list. `parse` maps an unrecognized name to `Unknown` | `crates/tools/src/read/resolver.rs:181-222` (`https` at 185-186, `Unknown` at 222); `ALL` at 227; `parse` at 231-232. Frozen copy: `crates/tools/url-vocab.json`, packed by `crates/py/build.rs` | Same lines |
| `install_unknown_fallback` installs one raw-scheme fallback and does not register `Scheme::Unknown` in the dense table | `crates/tools/src/read/resolver.rs:608-612` | Same lines |
| Production installs `HostUriResolver` as that fallback | `crates/envd/src/tool_url.rs:532-534` | `crates/envd/src/tool_url.rs:545-546` |
| `HostUriResolver` is documented as the resolver for schemes declared by the RPC host. Its read calls `resolve_read`, then `select_bytes` | `crates/envd/src/tool_url/host.rs:40-44` and 78-84 | Same lines |
| `HostResources::resolve_read` is the host broker | `crates/envd/src/tools.rs:3483` | `crates/envd/src/tools/bridges.rs:426-428` |
| `select_bytes` applies a line selector in core and returns the body unchanged for any other selector | `crates/envd/src/tool_url.rs:877-885` | `crates/envd/src/tool_url.rs:890-895` |
| A control read of `Scheme::Unknown` dispatches `read_unknown` | `crates/envd/src/server.rs:1494-1495` | `crates/envd/src/server/control_factories.rs:433-434` |
| An RPC host declares schemes from Python with `HostUri`. The bridge replaces one generation through `set_host_uri_schemes`. Names match `^[a-z][a-z0-9+.-]*$`, are unique after normalization, and a generation holds at most 32 | `crates/py/python/omp_rpc/host_uris.py:53-62`; `crates/rpc/src/client.rs:1387-1434`; `crates/rpc/src/protocol.rs:477` | `HostUri` at the same lines |
| Chat `/reload-plugins` posts `Mutation::ReloadExtensions`. `ExtensionReloadHandle::reload` drains idle extension hosts and starts the next generation. `quarantine_extensions` stops revoked hosts and keeps their static routes | `crates/chat/src/commands/tools.rs:543-547`; `crates/envd/src/lib.rs:677-681`; `crates/envd/src/server.rs:3405-3408` | Reload handle at `crates/envd/src/lib.rs:698-706`; quarantine at `crates/envd/src/server/session.rs:1102-1105` |
| Extensions cannot declare schemes. The deployment manifest has no `schemes` field. `omp.urls.schemes` returns the frozen built-in table | `crates/ext/src/config.rs:1356-1372`; `crates/py/python/omp/urls.py:114-124` | Same gap |

`Scheme::accepts_selectors` is false for `Mcp` and `Unknown` (`resolver.rs:236-238`). `parse_uri("custom://pending:raw")` therefore keeps `pending:raw` in the resource (`crates/tools/src/read/selector.rs:679-680` and the test at 887-889). `select_bytes` still runs for a host read, and it applies a range only when the parsed selector is already `Lines`. A declared scheme does not receive the shared line grammar until this record says it does.

## Decision

### D1. One canonical spelling; aliases are an owner choice

A declared scheme has one canonical spelling. The spelling matches the host-URI grammar already enforced by `set_host_uri_schemes`: `^[a-z][a-z0-9+.-]*$`, normalized to lowercase, at most `MAX_HOST_URI_SCHEME_BYTES`.

The only built-in alias pair is `http` / `https` (`Scheme::Http`). A declared spelling that collides with any built-in wire name in `url-vocab.json`, including `https`, is refused.

**Owner choice, recorded before S2.** Either:

- a declaration carries one spelling, or
- a declaration may list extra spellings that parse to that same scheme, on the pattern of `https` → `Http`.

The proposed default is one spelling. An alias is a second prompt surface and a second key in the frozen alias map (`omp/urls.py` `_SCHEME_ALIASES`). Extra spellings, if the owner allows them, still cannot reuse a built-in wire name, and they count toward the 32-scheme generation cap.

### D2. The built-in vocabulary does not grow

`Scheme`, `Scheme::ALL`, and `crates/tools/url-vocab.json` stay the closed built-in vocabulary. `Scheme::parse` keeps mapping every other syntactically valid name to `Unknown`.

No vendor concept adds a built-in scheme. A GitLab merge request stays `pr://…`, as [ADR 0049](0049-tiered-integration-architecture.md) D2.5 already requires. `gitlab://` and `mr://` are not built-ins. An extension may document its own resources under a scheme it declares; it may document a merge request as `pr://…`.

### D3. Declared schemes are served by the existing unknown-scheme fallback

Production keeps one fallback: `install_unknown_fallback` → `HostUriResolver`. A read of `Scheme::Unknown` keeps dispatching `read_unknown`. The handler result keeps passing through `select_bytes` in core.

An extension does not call `ResolverTableBuilder::register`, does not add a `Scheme` variant, and does not install a second fallback. `register` already rejects `Scheme::Unknown`.

The extension-facing registration is additive. The host bridge underneath stays the generation replace that `set_host_uri_schemes` already performs (`client.rs:1387-1388`). The environment publishes one generation that is the union of the RPC host's schemes and every live extension's schemes. One extension's registration does not erase another's.

### D4. Manifest field `schemes` and additive `omp.schemes`

An installed extension declares its schemes in the manifest field `schemes`. Each entry names the canonical spelling, a short description, whether the resource is immutable, and whether it is writable. Effects (fetch hosts, credentials) ride the manifest's existing capability declaration ([ADR 0049](0049-tiered-integration-architecture.md) D4).

At activation the extension registers those schemes through `omp.schemes`, an additive function on the frozen Python surface. Registration accepts a scheme only when the loaded manifest claimed it. A claim the manifest omitted is refused. The API adds a symbol; it does not break the ABI ([ADR 0036](0036-python-for-extensions.md)).

`omp.urls.schemes` continues to return the built-in table. Declared schemes are a separate listing, so the frozen `Scheme` enum stays closed.

### D5. Core owns selectors, the byte ceiling, and effect judgment

For a scheme this extension declared, core splits the shared line-selector grammar before dispatch and applies it with `select_bytes` after the handler returns the body. The handler sees the resource without that selector.

`mcp://` stays selector-opaque. An undeclared `Unknown` URI stays selector-opaque too: a trailing `:12` or `:raw` remains part of the resource, which is what `parse_uri` does today.

The byte ceiling is the central read bound ([ADR 0009](0009-bound-output-once.md)). An extension does not truncate the model-facing body itself.

Fetch and credential judgment uses the declaration. A declared scheme with no fetch and no credentials is a local read. Anything the declaration omits stays `Effects::unknown()`, which is the exec approval tier. The existing `read_unknown_fetch` hook (`resolver.rs:764-768`) is where that judgment is reported for the fallback.

### D6. Lifetime is the extension generation

A declared scheme is live only while the extension generation that registered it is live.

`/reload-plugins` posts `Mutation::ReloadExtensions`. `ExtensionReloadHandle::reload` starts the next generation, and that generation re-registers the extension's manifest schemes. `quarantine_extensions` drops the quarantined extension's schemes and keeps static routes, including every built-in resolver and any scheme still owned by a live extension or the RPC host.

Two extensions claiming one spelling: the first claim stands, and the later claim is refused. The refusal names both extensions. A quarantined scheme does not fall through to another extension.

### D7. Native MCP resource URIs are an owner choice

Mounted MCP resources stay readable as `mcp://`, via `omp.mcp.mount`, as ADR 0027 says. That path is unchanged under either owner choice.

**Owner choice, recorded before S2.** The prior product behavior served some MCP resource URIs that used the server's own scheme. Either:

- those native URIs stay an MCP-manager concern, and an extension that wants them readable tells the model to use `mcp://…`, or
- an extension may declare the server's native scheme, and a `read` of that URI is served through the unknown-scheme fallback by the extension that mounted the server.

The proposed default is the first. A native scheme is a second grammar beside `mcp://`, and a declared native spelling can occupy a name the built-in vocabulary might later need. The second choice, if the owner takes it, still refuses spellings that collide with built-ins, and the handler is the extension's declared read, not a new resolver.

### Completion

This record is complete when the owner has accepted it, D1 and D7 are recorded, and S1–S5 have landed. Until acceptance, the status stays proposed and no production change starts.

### Non-goals

- No new built-in scheme, and no new `Scheme` variant, for a vendor concept or for an extension.
- No second resolver table and no second unknown-scheme fallback.
- No runtime mutation of the latched wire roster or of tool schemas ([ADR 0024](0024-small-permanent-roster.md)).
- No JavaScript or TypeScript plugin runtime ([ADR 0036](0036-python-for-extensions.md)).
- No extension-held raw provider tokens ([ADR 0049](0049-tiered-integration-architecture.md) D4).
- Declaring a scheme does not replace `pr://`, `issue://`, or `mcp://`.

### Phase placement

ADR 0049 Phase **2S**: after Phase 2 (locality, the table-driven roster) and before Phase 3 (capability features). Phase 2S does not satisfy a Phase 2 or Phase 3 exit criterion, and it does not wait on Phase 4's forge-neutral `pr://` / `issue://` work.

One PR per slice. Every slice is blocked until the owner accepts this record.

| Slice | PR | Exit |
|---|---|---|
| S1 | Manifest field `schemes` and additive `omp.schemes`. Refuse a built-in wire name, an unclaimed name, and a grammar miss | A fixture manifest round-trips; `https` and `pr` are refused; `omp.urls.schemes` is unchanged |
| S2 | Fold live claims into one `set_host_uri_schemes` generation served by `HostUriResolver` | `read` of a declared scheme returns the handler body; a second extension's claim on the same spelling is refused; built-in `pr://` still resolves |
| S3 | Core line-selector split for declared schemes only, plus effect judgment on the declaration | `widget://id:2-3` returns those lines from core; `custom://pending:raw` for an undeclared scheme still keeps `pending:raw`; `mcp://` stays opaque |
| S4 | Rebind on `ExtensionReloadHandle::reload`; drop on `quarantine_extensions` | After `/reload-plugins` the scheme still reads; after quarantine it does not, and a built-in route still reads |
| S5 | Model-facing listing of declared schemes, without a roster or schema change, plus the joined conformance test | The conformance test covers install, read, selector, reload, quarantine, and a stable built-in route |

S2 starts only after D1 and D7 are recorded. S3 depends on S2. S4 depends on S2. S5 depends on S3 and S4.

### Acceptance tests

- A built-in spelling, including the `https` alias, is refused at declaration.
- A declared scheme is readable through `read` and through `omp.urls.read` of an `Unknown` scheme, and the body passes through `select_bytes`.
- A declared scheme's line selector is applied in core. An undeclared unknown URI and `mcp://` keep their current opaque grammar.
- Reload re-registers the scheme. Quarantine removes it and leaves static routes in place.
- Two extensions cannot own one spelling. One extension's registration cannot erase another's.
- The latched tool roster and the `Scheme` enum are unchanged by installing the fixture extension.
- With the proposed D7 default, a server-native MCP URI is not implicitly a declared scheme. `mcp://` still reads the mounted resource.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Grow `Scheme` and `url-vocab.json` for each extension | Reopens the closed built-in vocabulary, edits the frozen Python enum, and invalidates the prompt prefix that ADR 0024 latches |
| Vendor-specific built-in schemes (`mr://`, `gitlab://`) and vendor tools (`gitlab@1`) | The rejection in ADR 0049 stands. An adapter documents merge requests as `pr://…` |
| Leave extensions on `mcp://` and devices only | That remains available. It does not recover a scheme the extension owns and the model can `read` directly; the RPC host can already do that through `HostUri` |
| A second resolver table beside `HostUriResolver` | The production table already has one unknown-scheme fallback. A second table splits selector, effect, and byte-ceiling behavior |
| Extension-local reader tools | ADR 0027 prohibits a roster sibling for a format `Read` already projects |

## Consequences

**Easier:**

- An installed extension documents `widget://<id>` and `Read` materializes it, with line selection and the central byte ceiling.
- Adding that scheme edits no `Scheme` variant and no `url-vocab.json` row.
- Forge adapters keep documenting merge requests as `pr://…`.

**Prohibited:** a new built-in scheme for a vendor concept or an extension; registering a built-in wire name; a second unknown-scheme fallback; an extension replacing another extension's generation; raw provider tokens in the extension.

**Costs accepted:**

- A manifest field and one additive Python symbol, `omp.schemes`.
- A generation-scoped union published through `set_host_uri_schemes`.
- A line-selector split that applies to declared schemes and leaves undeclared `Unknown` URIs and `mcp://` opaque.
- Two owner choices (D1 aliases, D7 native MCP URIs) recorded before S2.

**Unchanged:** the `Scheme` enum, `url-vocab.json`, the latched wire roster, journal schema, and `mcp://` via `omp.mcp.mount`.

## Implementation gate

- Status is proposed. This record authorizes no production change before the owner accepts it.
- The owner records D1 and D7 before S2.
- S1–S5 each land as one PR under ADR 0049 Phase 2S, after Phase 2 and before Phase 3.
- Phase 2S does not start Phase 3 and does not close Phase 4.

## Status in omp

**Not started.** The unknown-scheme fallback and the RPC-host `HostUri` path exist, as the Context table records, on `omp2` at `580d3fb9fb` and at `bf39a439a0`. An installed extension has no `schemes` manifest field and no `omp.schemes` API. No slice of this record is implemented. Implementation waits on owner acceptance.

## References

- ADRs 0009, 0024, 0027 (amendment 2026-11-09), 0036 (amendment 2026-11-09), 0049 D2.5 and Phase 2S.
- `crates/tools/src/read/resolver.rs`, `crates/tools/url-vocab.json`, `crates/envd/src/tool_url.rs`, `crates/envd/src/tool_url/host.rs`, `crates/py/python/omp_rpc/host_uris.py`, `crates/rpc/src/client.rs`.
