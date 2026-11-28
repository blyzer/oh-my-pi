# Independent spot check of sampled claims

> Part of the [OMP2 gateway-capabilities research](../README.md). This is a check of form-plus-substance on a sample, not a full review.

**Method.** 32 capability cards were sampled at random (Python `random.seed(7)`, four per area across the eight areas: provider, authentication, extensions, Python, IPC, swarm, local inference, code intelligence). For each, a separate read-only `reviewer` agent opened the cited files at the cited lines and judged whether the code supports the card's `Existing implementation evidence` and `Actual missing capability`. Four reviewers took eight claims each. They were not given the area agents' reasoning and could not edit.

**Result.** 27 SUPPORTED, 5 PARTLY, 0 NOT SUPPORTED, 0 could not be checked (the reviewers noted that a few reference-repository citations inside those cards were outside what they were asked to check).

| Card | Verdict | What the reviewer found | Action taken |
|---|---|---|---|
| PRV-08 | PARTLY | the cited compiled aliases (`crates/catalog/src/compile.rs:1048`, `snapshot.rs:716`) have no provider field; provider-scoped selection is `AliasSelector` (`crates/catalog/src/resolve.rs:51`, `:1001-1010`) | card text corrected in deliverable 5; the class (`Unnecessary or redundant`) is unchanged because fallback chains and roles are as cited |
| PY-07 | PARTLY | MCP stdio and ADR citations hold; "no native Rust executable extension kind and no Go adapter" could not be confirmed by the reviewer's own survey (only `PluginLaunchKind::Hook` and `McpServer` found in `crates/ext`) | claim weakened in deliverable 8 to rest on the extension audit's search, not re-confirmed |
| CIX-04 | PARTLY | `Store::build` merges producer SCIP into `combined`; the raw producer SCIP is not persisted, so "nothing is missing" for an exporter was overstated | text corrected in deliverable 11; the decision (do not write an exporter) is unchanged |
| IPC-08 | PARTLY | `memmap2` is also used in `crates/shell-builtins/src/jq.rs:644` and `sed.rs:5305`, never for IPC | text corrected in deliverable 6b |
| IPC-02 | PARTLY | each embedding frame is serialized twice per direction (one real encode, one size-check encode), not "twice per side"; the cited constants all match | text corrected in deliverable 6b |
| All other 27 sampled cards | SUPPORTED | cited lines and symbols match the statements (examples: AUTH-01 `store.rs:1196-1199` and `registry.rs:1381`; EXT-01 `spawn.rs:46`, `main.rs:119-130`; INF-01 `rebuild_local_embeddings` has no caller and recall passes `None` as the query vector; SWM-07 no `--worker` or `Fleet` type found) | none |

**What this does and does not show.** It samples 32 of 103 cards, about 31 percent, and the five partial findings are all precision errors in a detail, none reversing a conclusion. It does not cover the reference-repository claims, the 154 audit rows, or the claims in deliverables 2, 3 and 4, and the reviewers were themselves language-model agents reading code, so the check reduces but does not remove the chance of a wrong statement elsewhere. The unsampled 71 cards were not individually re-read.

Back to the [index](../README.md).
