# fuzzy-index — warm fuzzy path/content index (ADR 0049 D5 rescue)

Warm Environment worker builds a trigram index via `omp.env.find.walk` + document
leases; soft devices `ffind` / `fgrep` return ranked matches (spill above
`RESULT_SPILL_BYTES`).

Historical source: `git show 00611047c1^:examples/fuzzy-index/`.

## Surface

- Worker `fuzzy-index` (site env, warm, `idle_ttl=0s`)
- Devices `ffind@1` / `fgrep@1` (family `fff`, place `worker:fuzzy-index`)
- Pure helpers: `omp_fuzzy_index.score`

## Gaps vs frozen API

- Workspace-wide watch / `WATCH_RESCANNED` still open — index rebuilds on worker
  boot only.

## How to test

```sh
PYTHONPATH=extensions/fuzzy-index/src python3 -m unittest discover -s extensions/fuzzy-index/tests -v
```
