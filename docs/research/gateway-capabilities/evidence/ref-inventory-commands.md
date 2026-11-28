# Commands behind `ref-inventory.md`

```sh
# per repository (xargs -P 6 over a 39-line group<TAB>owner/repo list)
GIT_TERMINAL_PROMPT=0 git clone --depth 1 --no-tags https://github.com/$repo.git /private/tmp/omp-research-refs/$group/${repo//\//_}
git -C $dir rev-parse HEAD; git -C $dir rev-parse --abbrev-ref HEAD; git -C $dir log -1 --format=%cI
gh api repos/$repo --jq '[.license.spdx_id,.archived,.pushed_at,.stargazers_count,.default_branch,.fork,.size,.description]'
```

Outcome on 2026-11-28: 37 of 39 cloned. `AmethystDev-Labs/QuickCLI` and `microsoft/onnxruntime` returned `ERROR: Repository not found` from `git ls-remote` and HTTP 404 from `gh api`. For ONNX Runtime the sibling repositories `microsoft/onnxruntime-genai`, `-extensions` and `-inference-examples` also returned not found, while `microsoft/vscode` and `pykeio/ort` resolved, so the failure is specific to those names and not a network or credential fault. `gh api users/AmethystDev-Labs/repos` lists no repository named QuickCLI.
