#!/usr/bin/env python3
"""Read-only GitHub reference inventory; prints JSON, never alters repositories."""
import concurrent.futures
import datetime
import json
import subprocess

REPOSITORIES = """
antonellof/saetta antonellof/frink antonellof/s0-cli antonellof/peerclaw
antonellof/vectX antonellof/MARS llvm/llvm-project nagisa/rust_libloading
rodrimati1992/abi_stable_crates ZettaScaleLabs/stabby alphastorm/fast-abi
Xuanwo/xabi dtolnay/cxx mozilla/uniffi-rs rust-lang/compiler-team rust-lang/rfcs
python/cpython PyO3/pyo3 tokio-rs/prost stepancheg/rust-protobuf
google/flatbuffers capnproto/capnproto capnproto/capnproto-rust rkyv/rkyv
bincode-org/bincode jamesmunns/postcard 3Hren/msgpack-rust apache/arrow-rs
serde-rs/serde pyfisch/cbor minicbor/minicbor enarx/ciborium
nnethercote/dhat-rs rafaelcaricio/stats_alloc tikv/tracking-allocator tokio-rs/tracing
bheisler/criterion.rs nvzqz/divan iai-callgrind/iai-callgrind KDE/heaptrack
koute/bytehound jemalloc/jemalloc microsoft/mimalloc microsoft/snmalloc
bytecodealliance/wasmtime WebAssembly/component-model bytecodealliance/wasm-tools
anthropic-experimental/sandbox-runtime ml-explore/mlx ggml-org/llama.cpp
stanford-iris-lab/meta-harness-tbench2-artifact awslabs/aws-sdk-rust
twittner/minicbor tobz/tracking-allocator gungraun/gungraun
neoeinstein/stats_alloc apple/coremltools
""".split()


def get(path):
    result = subprocess.run(["gh", "api", path], capture_output=True, text=True, timeout=40)
    if result.returncode:
        return {"unavailable": True, "error": result.stderr.strip()[:300]}
    return json.loads(result.stdout)


def inspect(repo):
    metadata = get("repos/" + repo)
    if metadata.get("unavailable"):
        return {"requested_repository": repo, **metadata}
    head = get("repos/" + repo + "/commits/" + metadata["default_branch"])
    license_meta = metadata.get("license") or {}
    return {"requested_repository": repo, "canonical_repository": metadata["full_name"],
            "url": metadata["html_url"], "default_branch": metadata["default_branch"],
            "sha": head.get("sha"), "head_error": head.get("error"),
            "archived": metadata.get("archived"), "disabled": metadata.get("disabled"),
            "pushed_at": metadata.get("pushed_at"), "license_spdx_metadata": license_meta.get("spdx_id"),
            "license_metadata_url": license_meta.get("url"),
            "note": "Metadata is not a source audit or legal conclusion; inspect pinned license text before reuse."}


if __name__ == "__main__":
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        records = list(pool.map(inspect, REPOSITORIES))
    print(json.dumps({"retrieved_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                      "command": "python3 references/collect_repositories.py", "repositories": records}, indent=2))
