# ABI-independent extension boundaries

> Back to the [index](../README.md). Part of deliverable 12. Companion to [`07-abi-independent-extension-assessment.md`](../07-abi-independent-extension-assessment.md).

## A. Current boundaries

Status: CURRENT, verified at 85d219ae7f

```mermaid
flowchart LR
  subgraph engine["omp process (Rust)"]
    sup["ExtHostSupervisor (envd)"]
    env["Environment DATA service"]
    mcp["MCP manager"]
    del["Delegated adapters (ACP, Codex)"]
  end
  subgraph host["omp __omp-ext-host child (same binary, embedded CPython)"]
    py["omp._host + extension wheel"]
  end
  subgraph wrk["omp __omp-py-worker child"]
    pw["named Python worker"]
  end
  ext["external MCP server or agent CLI"]
  sup -- "CONTROL: u32-BE length + JSON frames, fd 3" --> py
  py -- "DATA: protobuf omp.env.v1 over scoped Unix socket" --> env
  sup -- "worker CONTROL: protobuf omp.toolhost.v1" --> pw
  mcp -- "JSON-RPC over stdio or HTTP" --> ext
  del -- "ACP or app-server JSON-RPC" --> ext
  idx["signed index + lock + grants.toml"] -. "admits and verifies" .-> sup
```

```
+------------------------- omp process (Rust) --------------------------+
|  ExtHostSupervisor --CONTROL (u32-BE len + JSON, fd 3)--> ext host     |
|  Environment DATA <--protobuf omp.env.v1, scoped Unix socket-- ext host|
|  worker supervisor --protobuf omp.toolhost.v1--> python worker         |
|  MCP manager --JSON-RPC stdio/HTTP--> external MCP server              |
|  Delegated adapters --ACP / app-server JSON-RPC--> external agent CLI  |
|  signed index + lock + grants.toml admit and verify extensions         |
+------------------------------------------------------------------------+
ext host and worker = same omp binary re-entered (embedded CPython);
no shared library, no extern "C" plugin export, no Wasm host.
```

Evidence
- `crates/envd/src/exthost/spawn.rs:46, 543, 746`: hidden argument, spawn, child entry.
- `crates/app/src/main.rs:107-130`: same-binary re-entry for worker and extension host.
- `crates/envd/src/exthost/control.rs:2549, 3814-3834`: JSON CONTROL frame and u32 big-endian length.
- `crates/py/python/omp/_host.py:224, 240`: Python end of the frame codec.
- `crates/proto/proto/omp/env/v1/env.proto:12-40`: DATA framing and `ClientHello`.
- `crates/proto/proto/omp/toolhost/v1/toolhost.proto:15-36`: worker protocol and `WorkerHello`.
- `crates/envd/src/mcp/transport/`: MCP transports. `crates/driver/src/delegated_acp.rs:1-33`: delegated launch profiles.
- `crates/ext/src/index.rs:180`, `crates/ext/src/lock.rs:103`, `crates/ext/src/trust.rs:84`: index, lock, grants.

Legend: solid arrows are message channels; dashed arrow is admission and verification; every box in the engine process is Rust, every child is an OS process.

## B. Proposed boundary ladder

Status: PROPOSED, not implemented

```mermaid
flowchart TB
  need["new extension-shaped need"]
  data["1 data row: provider row, static content row"]
  pyext["2 Python extension via signed index"]
  extproc["3 external process: MCP, Delegated, configured gateway endpoint"]
  crate["4 first-party crate behind cap-* feature (ADR 0049 T1 test)"]
  adr["stop: ADR 0036 decision 2 needs superseding (Wasm host or non-Python runtime)"]
  need --> data
  data -- "not expressible" --> pyext
  pyext -- "needs another language or process" --> extproc
  extproc -- "needs raw credentials or hot path" --> crate
  extproc -- "needs in-engine runtime" --> adr
```

```
need -> 1 data row            (provider row, static content row)
     -> 2 Python extension    (signed index, lock, sandboxed child)
     -> 3 external process    (MCP, Delegated, configured gateway endpoint)
     -> 4 first-party crate   (cap-* feature, ADR 0049 T1 test)
     -> STOP: Wasm host / non-Python runtime = ADR 0036 decision 2 superseded
Common wire rule: messages only; schema_rev, api_level and capability strings;
receivers skip unknown fields. Common distribution rule: signed index, hashes, lock.
```

Evidence
- Ladder source: `docs/adr/0049-tiered-integration-architecture.md:41-54` (lowest tier that satisfies the requirement) and `docs/adr/0036-python-for-extensions.md:40-41`.
- Wire rule: `crates/proto/proto/omp/env/v1/env.proto:17-23`, `crates/proto/proto/omp/gateway/v1/gateway.proto:9-17`.
- Cards: EXT-01 to EXT-12 in [`07-abi-independent-extension-assessment.md`](../07-abi-independent-extension-assessment.md).

Legend: boxes are decision points; the last box is an owner decision, not a design.
