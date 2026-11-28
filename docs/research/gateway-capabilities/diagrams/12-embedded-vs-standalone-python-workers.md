# Diagram 12: embedded versus standalone Python workers

> Part of the [OMP2 gateway-capabilities research](../README.md). Deliverable 8 (diagram 12). Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status lines: the first diagram is **CURRENT** (verified at the audited commit, see [Python runtime audit](../evidence/omp2-python-runtime.md)); the second is **PROPOSED** (no runtime topology change; additions are measurement probes, documentation and a non-Python path through existing MCP stdio).

## 1. CURRENT

```mermaid
flowchart TB
    subgraph PARENT["omp agent process (Rust, no extension Python on its thread)"]
        ENVD["envd: supervision, bridge, CONTROL and DATA authority"]
    end
    subgraph EVAL["Eval session child"]
        EE["omp __omp-eval-child\nstatic CPython 3.14t, frozen stdlib\nnative _omp module"]
        EX["OPT-IN: external python 3.10+\nexternal_runner.py staged in temp dir"]
    end
    subgraph EXT["Extension host child (sandboxed or trusted tier)"]
        EH["omp __omp-ext-host\nstatic CPython 3.14t\nomp._host, own site tree"]
    end
    subgraph WRK["Named worker child"]
        WK["omp __omp-py-worker\nstatic CPython 3.14t\nomp._worker"]
    end
    ENVD -->|"JSON lines on stdin and stdout"| EE
    ENVD -.->|"same JSON lines, OMP_PYTHON_INTERPRETER"| EX
    ENVD -->|"CONTROL fd 3: u32 length plus JSON\nDATA unix socket"| EH
    ENVD -->|"varint length plus protobuf WorkerFrame"| WK
    EH -. "dlopen native wheels\ncp314t only" .-> SITE[("OMP_PY_SITE")]
    EE -. "dlopen native wheels" .-> SITE
```

```text
CURRENT (audited commit 85d219ae7f)

 omp agent process (Rust)
 |  envd supervises, never runs extension Python on its own thread
 |
 +--JSON lines (stdin/stdout)--> [omp __omp-eval-child]   embedded CPython 3.14t (default)
 |                         `..--> [external python3.10+ + external_runner.py]  (opt-in, Eval only)
 +--CONTROL fd3 u32+JSON, DATA unix socket--> [omp __omp-ext-host]  embedded CPython 3.14t
 |                                             tiers: sandboxed (no network) | trusted
 +--varint+protobuf WorkerFrame--> [omp __omp-py-worker]  embedded CPython 3.14t
 
 All embedded children are the same executable re-entered with a hidden argv[1].
 Native wheels load from $OMP_PY_SITE (cp314t, abi3t or none tags only).
 One interpreter per process; a second interpreter is a second process.
```

## 2. PROPOSED

```mermaid
flowchart TB
    subgraph PARENT["omp agent process (unchanged)"]
        ENVD["envd supervision (unchanged)"]
        PROBE["PY-02 and PY-03 probes: child startup, RSS, callback cost, binary delta"]
    end
    EE["Eval child: embedded CPython 3.14t (default, unchanged)"]
    EX["Eval child: external CPython (opt-in, now documented in ADR 0036 status, PY-10)"]
    EH["Extension host: embedded CPython 3.14t (unchanged, PY-01)"]
    WK["Named worker: embedded CPython 3.14t (unchanged)"]
    MCP["Non-Python tools: MCP stdio servers in any language, including Rust and Go (PY-07)"]
    ENVD --> EE
    ENVD -.-> EX
    ENVD --> EH
    ENVD --> WK
    ENVD -->|"existing mcp stdio transport"| MCP
    PROBE -.->|"measures"| EE
    PROBE -.->|"measures"| EH
    GATE["Requires ADR reconsideration, no work scheduled: PY-05 external CPython for ext hosts, PY-06 RustPython, PY-08 Wasm components"]
    GATE -.-> EH
```

```text
PROPOSED (research proposal, nothing implemented)

 Topology unchanged. Additions only:
   + measurement probes on embedded children (PY-02 startup/RSS/binary delta, PY-03 callback cost)
   + ADR 0036 status note recording the opt-in external Eval interpreter (PY-10)
   + non-Python tools keep using MCP stdio (PY-07); no new plugin runtime
 Held behind ADR reconsideration, evidence missing: PY-05, PY-06, PY-08
```

Back to the [index](../README.md).
