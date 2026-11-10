# Wave-1 first-party extensions

Owner decision 2026-11-10: home = in-repo `extensions/`; owners = repo
maintainers / this tree.

## Starts with

| Extension | Role | ADR |
|---|---|---|
| **gitlab** | GitLab forge adapter proving `omp-forge` (D2.8) via dyn `forge/*` over toolhost (D2.3) | [0049](../docs/adr/0049-tiered-integration-architecture.md) |

Scaffold only today: see [`gitlab/README.md`](gitlab/README.md). The adapter
body is **not** implemented yet.

## May grow

Further wave-1 ports (rescued v1 surfaces, GitHub T2 after GitLab parity, CI
vendors, BuildBuddy behind execution `Delegated`, …) are TBD. Candidates from
ADR 0049 D5 remain: `pr-review`, `green-loop`, `workflows`, `remote-approve`,
`search-provider`, `provider-pack`, `fuzzy-index`, `spec-flow`. UI-bound ports
wait for ADR 0046 item 4.
