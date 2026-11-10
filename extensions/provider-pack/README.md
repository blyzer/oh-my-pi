# provider-pack — Moonshot / Z.ai / Alibaba / OVH catalog (ADR 0049 D5 rescue)

Pure-data `ProviderSpec` rows for four OpenAI-/Anthropic-compatible providers.
Zero request-body hooks.

Historical source: `git show 00611047c1^:examples/provider-pack/`.

## Surface

- Providers: `moonshot@0`, `zai-glm@0`, `alibaba-dashscope@0`, `ovhai@0`
- Pure helpers: `omp_provider_pack.validate`

## Gaps vs frozen API

None — symbols this port needs are frozen. Import of `omp_provider_pack.device`
registers providers and requires the frozen `omp` package (covered by
conformance smoke under native stubs).

## How to test

```sh
PYTHONPATH=extensions/provider-pack/src python3 -m unittest discover -s extensions/provider-pack/tests -v
```
