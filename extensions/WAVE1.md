# Wave-1 first-party extensions

Owner decision 2026-11-10: home = in-repo `extensions/`; owners = repo
maintainers / this tree.

## Starts with

| Extension | Role | ADR |
|---|---|---|
| **gitlab** | GitLab forge adapter proving `omp-forge` (D2.8) via dyn `forge/*` over toolhost | [0049](../docs/adr/0049-tiered-integration-architecture.md), [schema](../docs/adr/0049-forge-dyn-device-schema.md) |

**D2.8 progress:** read-path + write-create adapter body + live
`ForgeDeviceClient` wiring landed under [`gitlab/`](gitlab/). Mapping unit
tests cover fixture → `ChangeRequest`/`Issue` shapes and draft → GitLab create
bodies. Mounted creates: `forge/change_request_create`, `forge/issue_create`.
`gitlab.com` is registered on the dyn forge arm. **DONE:** self-hosted host
registration via `sv_forge_dyn_hosts` + adapter env `GITLAB_HOST` (checkout
hosts in that set route to Dyn). SessionHost bound-client threading for
`forge@1` / numbered `pr://` is DONE (same client as CheckoutForge / CONTROL).
Checkout / push stay unmounted (ADR D2.4: repository work in core). Remaining:
full automatic device-catalog host-claim discovery (smaller TODO), search
leaves, GitHub T2 + delete `github.rs`. Full contract stability still awaits
GitHub T2 parity. See [`gitlab/README.md`](gitlab/README.md).

## May grow

Further wave-1 ports (rescued v1 surfaces, GitHub T2 after GitLab parity, CI
vendors, BuildBuddy behind execution `Delegated`, …) are TBD. Candidates from
ADR 0049 D5 remain: `pr-review`, `green-loop`, `workflows`, `remote-approve`,
`search-provider`, `provider-pack`, `fuzzy-index`, `spec-flow`. UI-bound ports
wait for ADR 0046 item 4.
