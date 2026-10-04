# 0027. `Read` materializes any resource; internal URL schemes

Status: accepted
Date: 2026-09-02
Area: tools

## Context

The obvious implementation of a read tool is one line:

```ts
return await Bun.file(path).text();
```

Harnesses that ship that line then grow `Ls`, `ReadNotebook`, `web_fetch`, an image tool, a
database tool — each a separate roster entry (0024) with its own truncation and error style — or
they leave the gap and the model finds shell workarounds (`cat`, `unzip -p`, `sqlite3`, `curl`)
while extension authors write their own readers. That is not less complexity: it is the same
complexity copied into shell commands, prompts, extensions, and failed tool calls, where nobody
owns it and everybody implements 30% of it slightly differently (0002).

Path errors were a second, less visible cost: an absolute path from another machine, an
unexpanded `~` on Windows, a wrong-but-unique suffix — each burned a turn on a recoverable mistake.

From the model's perspective every one of these is one operation: *materialize this resource into
the most useful representation I can reason about.*

## Decision

`Read` is the single materialization primitive. It MUST accept a `path` that is a local path, an
internal URL, or an HTTP(S) URL, and return the most useful projection for that resource kind:

- Directories → listing (no `Ls`). `.ipynb` → cells. `.pdf`/`.docx`/`.pptx`/`.xlsx`/`.epub` →
  extracted markdown. `.cpuprofile` / `sample.txt` → bottleneck summary.
- SQLite (`.sqlite`, `.sqlite3`, `.db`, `.db3`) → tables; `file.db:table` → schema and rows;
  `file.db:table:key` → by primary key; `?limit=`/`?where=`/`?q=SELECT` → query.
- Images → the image, or metadata when the model has no vision; `:img` rasterizes an SVG.
- Archives addressed without unpacking: `archive.ext:member` for zip, tar, jar, wheel, asar.
- `http(s)://` → reader-mode markdown (what `web_fetch` did), with ranges read on demand.
- Parseable code with no selector → structural summary with declaration bodies elided, and a
  footer naming the recovery ranges.

Selectors are appended after `:` and compose with every kind:

```text
:raw            bytes verbatim, projections bypassed
:conflicts      one line per unresolved merge-conflict block
:50  :50-  :50-200  :50+150  :5-16,960-973
:raw:50-100  :50-100:raw
```

Internal schemes share one resolver, one selector grammar, and one byte/entry ceiling:

```text
artifact://<id>  agent://<id>  history://<id>  issue://123  pr://123/diff/2
skill://react  rule://foo  memory://…  local://…  vault://…  security://…
omp://…  ssh://host/path  mcp://…
```

`Read` MUST also perform path recovery that would otherwise waste a turn: resolve a wrong absolute
path from a unique workspace suffix, expand `~` on Windows, and return actionable guidance for
binary or oversized resources instead of failing.

New resource kinds are added as projections behind `Read`, NEVER as new roster tools.

## Consequences

- One roster slot covers what other harnesses spend twenty on; the model learns one grammar.
- Extensions, subagent output, GitHub state, documentation, and remote machines are all readable
  without teaching the model anything new — a new scheme is a resolver entry.
- Prohibited: `Ls`, `ReadNotebook`, `web_fetch`-style siblings; extension-local readers for
  formats `Read` already projects.
- Cost accepted: `Read` is a large, deep module (parsers, converters, a URL resolver, a range
  grammar). `Read` is complicated so reading isn't.

## Status in omp

**Status: Implemented.** `Read` materializes local, internal, web, archive, SQLite, notebook, document, image and structural resources; I found no missing resource kind. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Resolver and selectors: `crates/tools/src/read.rs`, `read/{selector,resolver,archive,sqlite,notebook,pdf,image,profile,conflicts,web}.rs`, document extraction in `read/markit/{docx,pptx,xlsx,epub,...}.rs`.
- Internal schemes: `crates/envd/src/tool_url/{artifact,attachment,docs,local,mcp,memory,ssh,vault}.rs`, `crates/envd/src/github_url.rs` (`issue://`, `pr://.../diff`), `rule://` in `crates/driver/src/discovery/rules.rs`; `security://` is served by `crates/envd/src/security_scan.rs`.
- Path recovery and `~` expansion: `crates/tools/src/path.rs` and `read.rs`; `:raw`, ranges, `:conflicts` and `:img` in `read/selector.rs`.
- The prior note said 'Partial' without naming a gap and I could not find one; treat that label as resolved unless the owner knows of a missing kind. Not exercised here: each scheme's behavior.

### Implementation notes (carried over)

Primary implementation: `crates/tools/src/read.rs`. Read materializes local, internal, web, archive, SQLite, notebook, image, and structural resources. Focused image inspection is the optional `Read.question` path: local, archive-member, internal-URL, and HTTP(S) images become bounded blob parts plus a typed vision request for the active route, with metadata fallback when media is unavailable. `crates/tools/src/read/image.rs` bounds encoded bytes, decoded pixels, raster dimensions, and cached normalized output; `crates/chat/src/cards/read.rs` owns the combined Read/Inspect card. `crates/envd/src/{vault.rs,tool_url/vault.rs,tool_document.rs}` implements configured and Obsidian-discovered `vault://` roots with project/user/CLI precedence, strict URL decoding and selectors, bounded directory/file reads and CLI output, symlink confinement, cancellable and deadline-bounded CLI process-tree cleanup, atomic filesystem writes, search/read CLI queries, and create/move/delete/open mutations routed through Read/Write. Device discovery is not a Read scheme: `dyn` owns it (0025).

## References

- The Harness Playbook, "The tool surface" — "Deep builtins: Read"
- 0002 (one owner for the complexity), 0009 (central output bounds), 0024, 0025
- `crates/tools/src/read.rs`, `crates/tools/src/read/*.rs`
