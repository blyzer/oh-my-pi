# ADR 0049 D5 — rescue wave 1

Owner: repo maintainers / this tree.  
Historical sources: `git show 00611047c1^:examples/<name>/` (parent of delete
commit `00611047c1`).

| Port | Surface | Status | Historical |
|---|---|---|---|
| [`pr-review/`](pr-review/) | Parallel budgeted PR reviewers + publish approval | restored (minor Gaps: subtool overrides, incidental UI rail) | `00611047c1^:examples/pr-review/` |
| [`green-loop/`](green-loop/) | Affected-test feedback after edit/idle | restored with Gaps (`state_dir` replaces removed `omp.state` / entry kinds) | `00611047c1^:examples/green-loop/` |
| [`workflows/`](workflows/) | Resumable subagent DAG waves | restored with Gaps (`state_dir` resume cache replaces journal receipts) | `00611047c1^:examples/workflows/` |
| [`remote-approve/`](remote-approve/) | Remote approval ticket relay | restored (Gap: GET-only `http_get` webhook shape) | `00611047c1^:examples/remote-approve/` |
| [`search-provider/`](search-provider/) | Brave search provider + `web_search` | restored (Gap: `Api.SEARCH_HTTP` stand-in; device-owned parse) | `00611047c1^:examples/search-provider/` |
| [`provider-pack/`](provider-pack/) | Moonshot / Z.ai / Alibaba / OVH catalog data | restored cleanly | `00611047c1^:examples/provider-pack/` |
| [`fuzzy-index/`](fuzzy-index/) | Warm fuzzy `ffind` / `fgrep` worker | restored (Gap: no workspace watch rescan event) | `00611047c1^:examples/fuzzy-index/` |
| [`spec-flow/`](spec-flow/) | Spec brainstorm→plan→apply→archive | restored with Gaps (`state_dir` phase store; Component rewrite deferred) | `00611047c1^:examples/spec-flow/` |

UI-bound ports (`sidebar`, `hover-cards`, `welcome-chrome`) are **not** in this
wave — they wait for ADR 0046 item 4.

Conformance: `python3 scripts/check-extension-conformance.py` (or
`just check-extensions`).
