# Gap 17 — forge URL schemes (`forge-url-schemes`)

Scope: `pr://` and `issue://`. Brief: move their resolution out of core per ADR 0050 so each forge extension owns its own change- and repository-management schemes; existing URLs (`pr://<N>`, `pr://<N>/diff`, `issue://<N>`, host-qualified forms) keep working unchanged.

- **omp2 pin:** `omp2@f4186b783b`, the tip of `origin/omp2` (checked with `git ls-remote` and `gh api`). The research worktree HEAD carries only docs commits on top of it. Every citation was first read at `b8032a94ad`, two commits earlier; section 10 records the check that none of the cited files changed in between.
- **Upstream pin:** `omp-work/upstream` at `c308970263` (`chore: bump version to 18.20.0`). Every upstream citation was re-read there.
- **Method:** static reading only. Nothing was compiled or run and no code was changed; BuildBuddy was not used. A claim marked **UNVERIFIED** rests on reading or on the absence of a hit, not on a run.

## 1. Headline findings

1. **Recommended disposition: class a (already covered) for the goal, class b (not needed) for the literal reading.** The architecture the brief asks for already exists. Forge-neutral `pr://` and `issue://` stay built-in spellings in core; *vendor* resolution is owned by forge extensions behind `forge/*` devices (ADR 0049 D2.5, shipped by Phase 4 PR6 #559 and Phase 5, `docs/adr/TASKS.md:161-177`). The "compatibility shim routing to the owning extension" the brief asks for is today's `Delegated` router plus the dyn device bridge (section 3.4).
2. **ADR 0050 does not authorize moving `pr`/`issue`; it forbids declaring them.** D1 refuses any declared spelling that collides with a built-in wire name and lists `issue` and `pr` (`docs/adr/0050-extension-declared-url-schemes.md:42`). Non-goal: "Declaring a scheme does not replace `pr://`, `issue://`, or `mcp://`" (`:103`). Decision record: "**Unchanged:** the `Scheme` enum, `url-vocab.json`, the latched wire roster, journal schema, and `mcp://`" (`:169`). The S1 exit says "`https` and `pr` are refused" (slice table, `:105-119`) and S1 enforces it (`crates/py/python/omp/schemes.py:79`, `crates/py/tests/schemes_contract.rs:14`).
3. **ADR 0050 progress is S1 only, checked against origin and not only the local clone.** S1 merged as #593; S2 is OPEN with no PR and no branch; S3-S5 are BLOCKED (`docs/adr/TASKS.md:201-205`). `origin/omp2` is `f4186b783b`; the 14 most recent merged PRs (#599-#612) have no scheme work by title; `gh pr list --state all --search "ADR 0050"` returns only #558 (propose), #588 (accept), #593 (S1) and #559 (Phase 4 forge reads); `git ls-remote origin '*adr0050*' '*scheme*'` returns only `docs/adr0050-accept` and `feat/adr0050-s1-schemes`; `git grep declared_scheme_fold -- crates` (the S2 exit test, ADR `:180`) finds nothing. The brief's "S1/S2 already merged" is S1. `TASKS.md:155` and `:196` still say "S1 may start" (stale).
4. **Re-homing the two spellings on the extension-declared path would change behaviour and schema.** Six deltas, section 6.2: effect judgment (`credentials: true` becomes `false`), selector grammar, core cache and stale diagnostics, admission host naming, search-tool judgment, and the frozen vocabulary (`url-vocab.json`, Python `_SCHEME_ALIASES`, the ADR 0024 prompt prefix).
5. **What core still holds is neutral by design, plus a small vendor residue.** Neutral: grammar (`crates/forge/src/scheme.rs`, `forge_url.rs` `Target::parse` and `numbered_ref`), cache, markdown and diff rendering, selectors, effect judgment and admission naming; ADR 0049 D2.2 and D2.5 say core owns these. Residue in `crates/envd/src/forge_url.rs`: about 173 lines in 8 functions reached only by the legacy GitHub REST branch (regex call graph, approximate), plus GitHub constants that ADR 0049 D2.7 calls adapter data, plus GitHub-named strings (section 3.5). The REST branch looks unreachable but this is **UNVERIFIED**, and three existing tests assert its `api_url` strings, so deleting it needs an owner ruling.
6. **The two hand-kept scheme lists are not equivalent, and one is not live.** `crates/tools/src/path.rs` `is_internal_resource` (13 names, includes `issue` and `pr`) is reachable only through `aggregate_file_write_approval`, which has no production caller in the repo (definition plus three unit-test calls). `crates/edit/src/path_policy.rs` `INTERNAL_PREFIXES` (10 names) is live and omits `pr`, `issue`, `history` and `memory`; consequence in 3.1. Neither list needs a change for behaviour parity.
7. **Upstream makes the same choice as ADR 0050 D1.** Upstream registers `issue`, `pr` and `mr` in the router constructor as "OMP-owned, never replaced by hosts" (`packages/coding-agent/src/internal-urls/router.ts:104-105,125-127,133,177-180`), and both doors through which another party could add a scheme refuse built-in names: RPC host URIs (`modes/rpc/host-uris.ts:104-108`) and extension `registerLinkScheme` (`extensibility/extensions/loader.ts:347-349`; it claims links for activation, it does not resolve content). Upstream has no extension-owned resolution of forge schemes. Upstream-only features: `mr://` (GitLab diff family) and the `?at=<fetchedAt>` snapshot pin (section 5).
8. **Stop rule.** The mission's hard constraint is additive-only for tools, skills, convars, hooks, URL schemes and CLI (quoted from a memory excerpt cut at "If a gap seems to require changing one, stop work on ..."). The literal Gap 17 needs a vocabulary change, so it must not proceed without an owner amendment of ADR 0049 D2.5 and ADR 0050 D1, D2, Non-goals and Unchanged. Section 7 lists the behaviour-preserving residue and section 9 the owner decisions.

## 2. ADR and constraint table

| Source | Rule | Consequence for Gap 17 |
|---|---|---|
| ADR 0049 D2.1-D2.3 (`0049-tiered-integration-architecture.md:59-65`) | Pure `omp-forge` contract with no I/O and no vendor names; in-core `Delegated` adapter routes by host and enforces security in core; vendors are T2 extensions behind `forge/*` | Neutral front stays in core; vendor code lives under `extensions/` |
| ADR 0049 D2.5 (`:67-70`) | `pr://` and `issue://` keep their names; resolved by `Delegated` to the adapter owning the host, with core-side caching and selectors; vocabulary does not grow; a GitLab MR is `pr://gitlab.example.com/group/project/42` | The shim already exists; no `mr://`; no per-extension spellings |
| ADR 0049 D2.7 (`:72-75`) | Credential order stays in core; env-var names and canonical hosts are adapter data | Defines the vendor residue (3.5) |
| ADR 0049 D3.5-D3.6 (`:94-98`) | `cap-forge` default-on in `omp-app`; a compiled-out cap never becomes a runtime toggle; vendor code leaves the workspace | `cap-forge` must stay; no runtime toggle |
| ADR 0050 D1 (`:36-42`) | One canonical spelling, no aliases in v1; a declared spelling equal to a built-in wire name (incl. `issue`, `pr`) is refused | An extension cannot claim `pr` or `issue`; an alias shim is also ruled out |
| ADR 0050 D2 (`:44-48`) | `Scheme`, `Scheme::ALL` and `url-vocab.json` stay the closed vocabulary; "A GitLab merge request stays `pr://...`" | Vocabulary frozen |
| ADR 0050 D3 (`:50-56`) | Declared schemes are served by the unknown-scheme fallback (`install_unknown_fallback` to `HostUriResolver`); no second resolver | The `pr`/`issue` arms are not that path |
| ADR 0050 D5 and condition 3 (`:66-76`, `:164`) | Host URIs are judged `credentials: false` today; replacing that is an S3 prerequisite | Different effects from `pr`/`issue` (`credentials: true`) |
| ADR 0050 Non-goals (`:96-103`) | "Declaring a scheme does not replace `pr://`, `issue://`, or `mcp://`" | The literal brief is a stated non-goal |
| ADR 0050 Consequences (`:141-156`) | "Forge adapters keep documenting merge requests as `pr://...`"; Prohibited: registering a built-in wire name | Same |
| ADR 0050 Decision record (`:158-169`) | Conditions 1-4 (D1, D7, D5 correction, S1 placement); Unchanged: `Scheme` enum, `url-vocab.json`, latched roster, journal schema, `mcp://` | Schema and vocabulary invariants |
| ADR 0050 slices (`:105-119`) with `TASKS.md:201-205` | S1 DONE (#593); S2 OPEN; S3-S5 BLOCKED; one PR per slice | Brief says S1/S2 merged; only S1 |
| ADR 0024, 0027, 0036 (cited by ADR 0050) | Latched roster and prompt prefix; Read is the one materializer; Python ABI additive | Adding or removing a built-in scheme edits the latched prefix |
| Mission hard constraint (memory excerpt of the 2026-11-13 prompt) | "Additive only. Do not change the behaviour, names, schemas or revisions of existing tools, skills, convars, hooks, URL schemes or CLI commands." | The gap must not change existing URL schemes |

ADR 0050 decision-record conditions mapped to this note: (1) D1 one spelling, built-in names refused: 1.2, 6.4 and S17.5 (and the S2 guard test); (2) D7 extensions use `mcp://`: untouched, forge extensions need no native MCP scheme; (3) D5 correction (replace the blanket `credentials: false`): 6.2 first row and 6.4 step 2; (4) S1 placement (refusal lives in `crates/py`): 1.2, enforced at `crates/py/python/omp/schemes.py:79`.

## 3. Current state in omp2

A read of `pr://owner/repo/7/diff/2`, layer by layer (paths at `omp2@f4186b783b`).

### 3.1 Lexical reservation and vocabulary (core; stays)

- `crates/tools/url-vocab.json` (1659 bytes): rows `ISSUE` wire `["issue"]` (`:16`) and `PR` wire `["pr"]` (`:17`), `selectors: true`. Packed by `crates/py/build.rs:133`, embedded as `URL_VOCABULARY_JSON` (`crates/tools/src/read/resolver.rs:158`), included by `crates/py/tests/schemes_contract.rs:11`.
- `Scheme::Issue` (`resolver.rs:206`) and `Scheme::Pr` (`:208`) in the closed enum (`:181-225`). `Scheme::parse` maps every other valid name to `Unknown` (`:233`). `accepts_selectors` is false only for `Mcp` and `Unknown` (`:238-240`). `ResolverTableBuilder::register` rejects `Scheme::Unknown` (`:596-599`), so the dense table has no route for it (`entry`, `:705-708`; `get`, `:746-749`).
- **Two hand-kept "internal scheme" lists exist and they are not equivalent.**
  - `crates/tools/src/path.rs:86-105` `is_internal_resource` lists 13 names including `issue` and `pr`. Its only caller is `aggregate_file_write_approval` (`:68-84`), and that function has no production caller anywhere in the repo: `git grep` finds its definition and three calls in its own unit test (`:486-496`, which use `local://`, `artifact://` and `vault://`, never `pr://` or `issue://`). So this list is a latent seam, not live behaviour (**UNVERIFIED** beyond this repo).
  - `crates/edit/src/path_policy.rs:19-30` `INTERNAL_PREFIXES` lists 10 and omits `pr://`, `issue://`, `history://` and `memory://`. It is live: the "internal scheme ... not a filesystem path" refusal in `PathPolicy::resolve` (`:69-75`; the only test of it that `git grep` finds names `agent://`, `:585`), `is_internal_url` (`:206-212`, which guards hashline tag recovery at `:178-181` and missing-path recovery at `crates/edit/src/files.rs:184-190`), and the leading-`@` strip (`:281`). The match is a case-sensitive `starts_with` on a string in which only `local:/` is normalized (`:248-255`); `is_internal_resource` lowercases (`path.rs:91`).
  - **Consequence for `pr://1` and `issue://1`.** `PathPolicy::resolve` does not refuse them. By reading, `expand_path` leaves the string unchanged (`:257-320`) and `lexical_normalize(cwd.join(..))` (`:77-87`, `:394-406`) yields the workspace-relative path `<cwd>/pr:/1` (value derived by reading, not run). The `write` tool has `reject_uri_like_target` (`crates/tools/src/write.rs:1254-1282`), which refuses any `scheme://` target no writer owns with "{scheme}:// targets are not supported yet"; that `pr://` and `issue://` reach it was not traced (**UNVERIFIED**). `crates/tools/src/edit.rs` has no scheme handling, so an `edit` of `pr://1` is not refused as an internal scheme and fails, if at all, because the file does not exist (**UNVERIFIED**: whether any edit mode can create the file).

### 3.2 Resolver registration (core, behind `cap-forge`)

- `crates/envd/src/tool_url.rs:49-52`: `type SchemeForge = omp_forge_delegated::Delegated<forge_adapter::ForgeBackend, omp_forge_delegated::PermitAll>`.
- Variants `Issue(ForgeResolver<SchemeForge>)` (`:227`) and `Pr(ForgeResolver<SchemeForge>)` (`:231`), registered at `:632-658` with `SchemeEntry::new(Scheme::Issue, true, false, "cached forge issues")` and `SchemeEntry::new(Scheme::Pr, true, false, "cached forge change requests and diffs")`. With the cap off nothing is registered (`:659-660`).
- Trait arms: `read` `:266-267`; `list` fails with "GitHub list resources are read as Markdown." (`:376-379`); `complete` returns empty (`:431-432`); `read_fetch` returns `FetchEffects { credentials: true }` for both (`:453-460`) while host URIs get `credentials: false` (`:462`); `walk_fetch` is `None` (`:487-492`); `fetch_host` names the forge host (`:512-524`).
- `cap-forge` is a Cargo feature of `omp-envd` (`crates/envd/Cargo.toml:21`, default empty at `:13`), on by default in `omp-app` (`TASKS.md:167`). It appears in 23 files (19 source files and 4 Cargo manifests); hit counts: `forge_url.rs` 67, `forge_adapter/mod.rs` 48, `crates/tools/src/render/misc.rs` 17, `tool_url.rs` 17, `crates/envd/src/tools/registry.rs` 15, `crates/tools/src/roster.rs` 13, `crates/envd/src/tools/roster_host.rs` 13, `crates/envd/src/server/session.rs` 9, the rest 4 or fewer. `just check-minimal-caps` (`justfile:232`) proves the forge crates leave the envd graph when caps are off.

### 3.3 Core front: `ForgeResolver` (`crates/envd/src/forge_url.rs`, 2193 lines, tests from `:1506`)

- Module doc (`:1-17`) calls it the "Forge-neutral resolver" and admits a private GitHub REST client remains for legacy paths.
- `ForgeScheme { Issue, PullRequest }` (`:350-355`); `ForgeResolver<H>` (`:357-365`) holds scheme, root, `ForgeCache`, `ForgeCredentialBridge`, an `omp_http` client and the forge host `H`.
- `resolve` (`:403-480`): `numbered_ref` (`:1207-1228`, wrapping the pure parser `omp_forge::parse_resource_url`, `crates/forge/src/scheme.rs:194`) leads to `resolve_numbered` (`:653-694`: cache get and put without an etag, stale body on refresh failure) and `fetch_numbered` (`:696-760`: `forge.change_request_diff`, `view_change_request`, `view_issue`, `list_comments`; 8 MiB bound; `# title` plus body). List URLs go `Target::parse` (`:869-953`), `resolve_list` (`:484-519`), `fetch_list` (`:520-530`, `forge.search`).
- The host defaults to `github.com` when the URL omits one (`:72`, `:1220-1222`); a bare number uses the checkout remote through `infer_repo` (`:1414`).

### 3.4 Delegation chain (already extension-owned)

`SchemeForge` (`Delegated`, routes by ASCII-case-insensitive host, `TASKS.md:163`) leads to `ForgeBackend` (Dyn-only; `crates/envd/src/forge_adapter/mod.rs:1-19`: "There is no in-core Github fallback arm"), then `DynForgeAdapter` and `LiveForgeDeviceClient` (`forge_adapter/dyn_forge.rs`), then `DeviceControlFactory::forge_invoker`, then `omp.devices.call forge/<op>` (`docs/adr/0049-forge-dyn-device-schema.md:58-76`, leaves `:26-41`), then `extensions/forge` (`omp.forge`, capability `env.net.fetch`, `FORGE_PARENT = omp.devices.parent("forge", ...)` at `extensions/forge/src/omp_forge_ext/devices.py:25`) host-dispatching by `repo.host` to `extensions/github` (`omp.github`) and `extensions/gitlab` (`omp.gitlab`) (`TASKS.md:174`). None of the three manifests declares `[[schemes]]`. The in-core `github.rs` is deleted (`TASKS.md:177`).

### 3.5 Vendor residue still in core

- **Legacy REST branch:** `ForgeResolver::fetch` (`:538-592`), `fetch_comments` (`:600-651`), `Target::accept` (`:974-980`), `Target::api_url` (`:982-1020`), `ForgeRepo::api_url` (`:122-124`), `api_url_for_host` (`:127-133`), `Target::comments_url` (`:1026-1032`), `http_fault` (`:1503-1505`); 173 lines in all. It is reached only from the non-numbered, non-list tail of `resolve` (`:421-479`). `numbered_ref` returns `None` only for list shapes (`is_list_resource`, `:1231-1242`) or `MissingNumber`, and `Target::parse` then yields a list view or an error, so the branch looks dead. **UNVERIFIED:** no test drives it over HTTP (the test module has no mock server); only string assertions on `api_url` exist (`:1646-1654`, `:1678-1685`).
- **Constants and strings:** `DEFAULT_FORGE_HOST` (`:72`), `https://api.github.com{path}` (`:129`), `GITHUB_COM_TOKEN_VARS` (`:167`), `GITHUB_ENTERPRISE_TOKEN_VARS` (`:170`), credential spec `"github"` (`:270`), `x-github-api-version` headers (`:541`, `:610`); `FetchHost::github` (`crates/envd/src/fetch_host.rs:66`); the list error string (`tool_url.rs:378`).
- **`ForgeCredentialBridge` is not forge-URL-only.** Telemetry upload, share, grievances and chat services share it (`crates/driver/src/{bridges,share,telemetry_upload}.rs`, `crates/app/src/{grievances_cmd.rs,chat_services/misc.rs}`, `crates/envd/src/lib.rs:153,687`). Moving its constants is an ADR 0049 D2.7 and D4 security change, not a URL-scheme change.

### 3.6 Classification of every `pr://` / `issue://` hit under `crates/` (22 files at the pin, all named below)

- **Behaviour (resolver, grammar, admission):** `crates/forge/src/scheme.rs` (23), `crates/envd/src/forge_url.rs` (9), `crates/envd/src/tool_url.rs` (6), `crates/envd/src/fetch_host.rs` (5).
- **Doc comments naming the schemes:** `crates/tools/src/read/resolver.rs:205,207`, `crates/forge/{README.md,src/lib.rs,src/error.rs}`, `crates/envd/src/forge_adapter/mod.rs:5`, `crates/envd/src/tools/registry.rs:1363`, `crates/tools/src/{read.rs:465,grep.rs:94}`, `crates/proto/proto/omp/policy/v1/policy.proto:169`, `crates/tools/src/render/fs.rs:127`.
- **Chat and TUI:** `crates/chat/src/autocomplete/github_refs.rs` (8: `#3164` inserts `pr://3164 ` or `issue://3164 `; stays by design, `TASKS.md:177`), `crates/chat/src/overlays/info.rs:513`, `crates/tui/src/editcore.rs:4332`.
- **Tests (oracle):** `crates/envd/src/server/tests/probes.rs`, `crates/tools/tests/it/{read,grep}.rs`, `crates/envd/src/admission.rs:1763,2166`, `crates/driver/src/headless/kernel.rs:4490,4501`, in-file tests of `forge_url.rs` and `scheme.rs`.

## 4. Existing-spelling matrix (must not change; existing tests are the oracle and are never edited)

| Spelling | Grammar owner | Resolution owner today | Oracle (existing tests) |
|---|---|---|---|
| `pr://<N>`, `issue://<N>` (bare number; repo from checkout remote; host defaults to `github.com`) | `numbered_ref` and `expand_shorthand` (`forge_url.rs:1207-1245`), `infer_repo` (`:1414`) | `resolve_numbered` | `shorthand_number_uses_the_checkout_and_keeps_a_diff_selector` (`forge_url.rs:1724`); `numbered_reads_render_title_and_body_and_cache_without_an_etag` (`:2003`); probes `("issue://5", credentialed)` (`probes.rs:1570`) |
| `pr://owner/repo/<N>` (no host) | `parse_resource_url` (`crates/forge/src/scheme.rs:194`) | same | `shorthand_omits_the_host_and_nested_owners_keep_their_slashes` (`scheme.rs:420`); probes `pr://owner/repo/7` (`probes.rs:1571`) |
| `pr://<host>/owner/repo/<N>` (dotted host) | `scheme.rs` | same, routed by host | `parses_a_change_request_with_host_owner_repo_and_number` (`scheme.rs:406`); `target_parses_dotted_and_single_label_enterprise_hosts` (`forge_url.rs:1640`) |
| `pr://ghe/owner/repo/7` (single-label host, numbered form only; `ghe/owner/repo` is an error) | `Target::parse` (`:869-904`) | same | `forge_url.rs:1648-1667` |
| nested owners `pr://code.example/org/team/widget/42` | `split_repo`, `join_owner` (`scheme.rs:307,327`) | same | `scheme.rs:420` |
| `/diff`, `/diff/all`, `/diff/<i>` (i from 1) | `peel_selector` (`scheme.rs:249`) | `fetch_numbered` Diff arm and `render_diff` (`forge_url.rs:705-715`) | `diff_selectors_roundtrip_and_issues_reject_them` (`scheme.rs:432`); `diff_selectors_slice_the_host_text` (`forge_url.rs:2071`); `numeric_tail_disambiguates_host_from_diff_suffix` (`:1659`); `crates/tools/tests/it/read.rs:2524` |
| `issue://.../diff` rejected | `scheme.rs:443` | none | `scheme.rs:432`; error "Only pr:// resources support /diff[/all\|<index>]." (`forge_url.rs:923`) |
| `?comments=0`, `false`, `1`, `true` (other keys ignored; `comments=maybe` is an error) | `parse_comments` (`scheme.rs:357`) | `fetch_numbered` (`forge_url.rs:703`) | `comments_query_and_percent_decoding` (`scheme.rs:453`); `issue_comments_can_be_disabled_and_an_unregistered_host_is_not_github` (`forge_url.rs:2047`) |
| percent-decoding and traversal (`my%2Dwidget` ok; `%2e%2e`, `..` rejected) | `decode_path` (`scheme.rs:235`), `Target::parse` (`forge_url.rs:879-881`) | none | `scheme.rs:453,469` |
| list URLs: `pr://`, `issue://`, `://owner/repo`, `://host/owner/repo` with `?state=&limit=&author=&label=` (state open, closed, merged or all, default open; limit 1..100, default 30) | `Target::parse` list arm (`:938-951`), `is_list_resource` (`:1231`) | `resolve_list`, `fetch_list`, `forge.search` (`:484-530`) | `list_urls_stay_outside_the_numbered_parser` (`:1700`); `list_urls_resolve_through_forge_search_and_cache_without_an_etag` (`:2101`); `gitlab_list_urls_route_through_the_registered_forge_host` (`:2139`) |
| stale or failed refresh keeps the cached body plus a stale diagnostic | `resolve*` | same | `stale_cache_is_reported_out_of_band_without_changing_document_bytes` (`:1624`); `a_failed_refresh_returns_the_cached_body_and_a_stale_diag` (`:2166`) |
| credential scoping per host family | `ForgeCredentialBridge` (`:181-345`) | same | `ambient_tokens_are_scoped_to_their_host_family` (`:1538`); `enterprise_hosts_never_receive_the_stored_dotcom_credential` (`:1575`); `remote_urls_preserve_enterprise_hosts_and_normalize_github_dot_com` (`:1673`) |
| effect and admission judgment of every spelling above (credentialed fetch; forge host named for admission; `pr://Owner/Repo/7/diff`; GHE remote) | `read_fetch`, `walk_fetch`, `fetch_host` (`tool_url.rs:453-524`), `fetch_host.rs` | core | `read_judges_each_call_by_what_its_targets_fetch` (`crates/tools/tests/it/read.rs:2497`); `egress_resolvers` (`:2413`); `grep_judges_each_call_by_what_its_roots_fetch` (`crates/tools/tests/it/grep.rs:597`); `fetch_queries_name_every_host_the_resolvers_reach` (`probes.rs:1417`); `production_read_declares_its_fetches_and_local_reads_stay_read` (`:1542`); `read_queries_name_the_hosts_its_targets_fetch` (`:1652`); `production_search_tools_declare_their_remote_roots_and_local_roots_stay_read` (`:1770`) |
| malformed: `pr://42`, `pr://acme/widget` (list), `.../0`, `issue://acme/../1`, `.../diff/0`, `comments=maybe` | `scheme.rs:469-489` | none | `rejects_incomplete_and_foreign_urls` (`scheme.rs:469`) |
| `write` or `edit` of `pr://x` | `write`: `reject_uri_like_target` (`crates/tools/src/write.rs:1254-1282`); `edit`: `PathPolicy::resolve` with `INTERNAL_PREFIXES` (`crates/edit/src/path_policy.rs:19-30,69-89`) | core | none found for `pr://` or `issue://`; the one refusal test names `agent://` (`path_policy.rs:585`) (**UNVERIFIED**) |

Coverage: every `pr://` / `issue://` literal in the `crates/forge/src/scheme.rs` tests (14, at `:407,421,434-436,443,447,454,459,463,476,477,479,483`) and in the `forge_url.rs` tests (`pr://acme/widget/7/diff/1`, `/2`), and every `Target::parse` / `numbered_ref` call-site resource there (`ghe.example.com/owner/repo/7`, `ghe/owner/repo/7`, `ghe.example.com/owner/repo/8`, `owner/repo/77/diff/1`, `ghe/owner/repo`, `acme/widget` with `state=open`, `acme/widget/7`, `7/diff/2`), falls into a row above. The matrix has no `?at=` row and no `mr://` row: omp2 ignores unknown query keys (`scheme.rs:15-17`) and has no `mr` scheme.

## 5. What upstream does (`c308970263`, `packages/coding-agent/src`)

- **Ownership: core, built-in, not replaceable by hosts.** `internal-urls/router.ts` registers `IssueProtocolHandler`, `PrProtocolHandler` and `MrProtocolHandler` in the `InternalUrlRouter` constructor (`:125-127`). The constructor's schemes form `#builtinSchemes`, "OMP-owned, never replaced by hosts" (`:104-105`, `:133`), and `isBuiltin(scheme)` exposes it (`:177-180`). Unregistered custom schemes fall to the MCP resource handler (`:102-103`, `:108-109`, `:128`). The TUI tools read scheme specs through `packages/tui/src/tools/url-scheme-host.ts:16-27`.
- **Where non-replacement is enforced (two registration doors).**
  - RPC host URIs: `modes/rpc/host-uris.ts` registers host-declared schemes served by the RPC host (`RpcHostUriProtocolHandler`, `:39`, delegating reads and, when declared writable, writes over the RPC transport). It refuses a built-in name: "Host URI scheme is reserved by OMP: <scheme>://" (`:104-108`). This is the analogue of omp2's `HostUriResolver` and ADR 0050 D3.
  - Extension link schemes: `ExtensionAPI.registerLinkScheme` (`extensibility/extensions/loader.ts:339-351`; interface `types.ts:2112-2120`) lets an extension claim a scheme such as `cc-stretch:` so Markdown links using it are drawn without the URL suffix and raise a `link_activate` event (`types.ts:1372-1384`). It is interactive-only display and activation, not content resolution. It throws for the standard schemes (`STANDARD_LINK_SCHEMES`, `loader.ts:72`) and for router built-ins (`:347-349`).
- **Handlers.** `internal-urls/issue-pr-protocol.ts` (560 lines): `IssueProtocolHandler` (`:442`) and `PrProtocolHandler` (`:500`), each with `spec: SchemeSpec = { backing: "remote", selectors: "lines", immutable: true }` and a `promptDoc()`. They call `gh` through `../tools/gh` (`getOrFetchIssue`, `getOrFetchPr`, `getOrFetchPrDiff`, `getPinnedPrDiff`) and `../utils/github`, and cache in the SQLite `../tools/github-cache` (`formatFreshnessNote`). `forge-diff.ts` (141 lines) is the diff-family renderer shared with `mr://`. `mr-protocol.ts` (124 lines): `MrProtocolHandler` (`:80`), GitLab through `glab` (`../utils/gitlab`, `../tools/glab-mr`), diff family only.
- **Grammar** (`issue-pr-protocol.ts:9-23`): `issue://` and `pr://` list the default repo; `://owner/repo` lists a repo; `://123` is one item with the repo from the session cwd; `://owner/repo/123` is fully qualified; any shape may carry a `<host>/` prefix (a dotless host only in the numbered form); `?comments=0`; `?state=&limit=` list options; `pr://owner/repo/123/diff/2?at=<fetchedAt>` pins that cached diff generation. `mr://<host>/<group...>/<project>/<iid>/diff[/all|/<i>]` and `mr://<iid>/diff[/...]` (`mr-protocol.ts:4-7`).
- **Model-facing text** (`prompts/internal-urls/issue.md`, 122 chars; `issue-reference.md`, 165 chars): `?state=&limit=&author=&label=`, `/diff` (files), `/diff/<i>`, `/diff/all`, and "?at=<generation> pins that cached snapshot".
- **Differences from omp2.**
  - `?at=<generation>`: upstream-only. omp2's pure parser ignores unknown query keys (`crates/forge/src/scheme.rs:15-17`) and `Target::parse` reads only `state`, `limit`, `author`, `label` and `comments` (`forge_url.rs:912-951`).
  - `mr://`: upstream-only. omp2 spells a GitLab MR `pr://host/group/project/N` (ADR 0049 D2.5); ADR 0050 D2 says `mr://` is not a built-in.
  - Upstream `pr://` and `issue://` are GitHub-only through `gh`; omp2 routes by host to any forge adapter.
  - Both refuse host replacement of built-ins; ADR 0050 D1 is the same rule.
  - Upstream has no extension-owned resolution of forge schemes (its extension-side scheme claim is for link activation), so there is nothing to port for "extension-owned forge schemes".

## 6. The key question: how core keeps serving the spellings while resolution moves out

### 6.1 Answer: the shim already exists; keep it and do not move the front

| Layer | Owner today | Moves under Gap 17? |
|---|---|---|
| Spelling reservation (`pr`, `issue` in `Scheme`, `url-vocab.json`) | core vocabulary | No. ADR 0050 D1, D2, Unchanged; upstream does the same |
| Grammar (`omp-forge` `parse_resource_url`; list grammar in `Target::parse`) | core, pure, vendor-neutral | No. Neutral by ADR 0049 D2.1 |
| Cache, selectors, markdown and diff rendering, stale diagnostics | core `ForgeResolver` | No. ADR 0049 D2.5 "core-side caching and selectors" |
| Effects and admission (`read_fetch`, `walk_fetch`, `fetch_host`) | core | No. ADR 0049 D2.2 "enforce security in core" |
| Host routing | `omp-forge-delegated::Delegated`, by ASCII-case-insensitive host | Already generic |
| Vendor protocol (GitHub, GitLab) | `extensions/forge`, `github`, `gitlab` behind `forge/*` devices | Already moved |
| GitHub constants and the dead REST branch | core `forge_url.rs` | Candidates only (S17.3-S17.4) |

Absent extension and cap off: cap off registers nothing (`tool_url.rs:659-660`). Cap on with no mounted device returns `ForgeError::Unavailable` honestly (`forge_adapter/mod.rs:15-18`, `TASKS.md:172`). Both stay as they are.

### 6.2 Why re-homing on the extension-declared path changes behaviour

| Today (oracle in section 4) | On the declared or unknown path |
|---|---|
| Effect: `read_fetch` is `FetchEffects { credentials: true }` (`tool_url.rs:460`) | `FetchEffects { credentials: false }` for every host URI (`tool_url.rs:462`; ADR `:76`; `TASKS.md:203`), so the approval tier drops |
| Selectors: `Scheme::Pr` and `Issue` accept the shared line selector (`resolver.rs:238`) | `Unknown` is selector-opaque until S3 (ADR `:32`) |
| Cache: `ForgeCache` fresh and stale entries with stale diagnostics (`forge_url.rs:653-694`; tests `:1624`, `:2166`) | `HostUriResolver` has no cache |
| Admission: `fetch_host` names the forge host (`tool_url.rs:512-524`; `probes.rs:1443-1453`) | the environment fetches nothing for a host URI (`tool_url.rs:450-452`), so no forge host is named |
| Search tools: a `grep` root `issue://5` or `pr://...` is judged credentialed (`tool_search.rs:199-204` reads `ResolverTable::read_fetch` for the scheme; `tool_url.rs:460`; `crates/tools/src/grep.rs:94`; `probes.rs:1836`) | a root whose scheme parses to `Scheme::Unknown` is judged `None` by that same call: the dense table has no `Unknown` route (`resolver.rs:597-599`, `:705-708`, `:756-764`); `read` instead uses the fallback for a foreign target (`crates/tools/src/read.rs:971`) and gets `credentials: false`. Whether such a root reaches `internal_root_fetch` at all was not traced (**UNVERIFIED**) |
| Vocabulary and schema: `Scheme::{Issue,Pr}`, `url-vocab.json`, Python `_SCHEME_ALIASES`, ADR 0024 prompt prefix | removal edits all four (ADR 0050 Alternatives, `:131-139`) |

### 6.3 Options

| Option | What moves | Needs | Verdict |
|---|---|---|---|
| A. Keep ADR 0049 D2.5; close as class a/b; optional residue hygiene (section 7) | nothing, or the dead REST branch | none, or owner rulings | **Recommended** |
| B. Extension-declared `pr`/`issue` (the literal brief) | spelling ownership and resolution | New ADR superseding 0049 D2.5 and 0050 D1, D2, Non-goals, Unchanged; S2-S5 first; remove `Scheme::{Issue,Pr}` and vocabulary rows; no alias table allowed (D1) | Reject: changes six behaviours and the frozen schema |
| C. Thin-forwarder arm plus a new `forge/read_resource` device; the extension renders | grammar, cache and rendering move into Python | new dyn leaf (additive) and a byte-identical port of `render_numbered`, `render_diff`, `render_search_list` | Not recommended: contradicts D2.5 "core-side caching and selectors"; high parity risk |
| D. Forge extensions declare extra, non-colliding schemes after S2 | new names only | S2-S5 | Allowed by ADR 0050; no current need |

### 6.4 If the owner nevertheless chooses Option B (order of work)

1. New ADR amending ADR 0049 D2.5 and ADR 0050 D1, D2, Non-goals and Unchanged, with an explicit exemption from the additive-only constraint.
2. Land S2-S5, including the S3 prerequisite (declaration-based effect judgment) so a declared `pr` can still be judged `credentials: true`.
3. Give the declared path a core cache and an admission host name; it has neither today.
4. Remove `Scheme::{Issue,Pr}` from `url-vocab.json`, `resolver.rs`, the `crates/py/build.rs` pack and Python `_SCHEME_ALIASES`, after an ADR 0024 prefix review.
5. The two scheme lists need no change for parity: `is_internal_resource` has no production caller and `INTERNAL_PREFIXES` does not list `pr` or `issue` (3.1).
6. Add `[[schemes]]` claims to `extensions/forge/omp.toml`; keep `cap-forge` as the compile gate.

The shim is then a core reservation plus a manifest claim of the same spellings; ADR 0050 D1 forbids an alias table.

## 7. Slice plan (optional residue; none of it changes a spelling, a vocabulary row or a schema)

Rules for every slice: one PR per slice on its own branch (never on `omp2`; check `git branch --show-current` before each commit); tests are additive only and no existing test, golden or `crates/tools/url-vocab.json` is edited; Rust compile, test and lint run on BuildBuddy only (`rustfmt --edition 2024 --config-path rustfmt.toml <files>`, then `just remote ...`; a pass counts only with a `Summary [..] N tests run: N passed` line).

**S17.0 Disposition and drift (docs only; coordinator).** Ledger row: class a for "resolution out of core" (ADR 0049 D2.5; PRs #553-#557, #559 and #583), class b for per-extension `pr`/`issue` ownership; no ADR needed. Fix stale text at `docs/adr/TASKS.md:155` and `:196` ("S1 may start") and optionally the module doc `forge_url.rs:14-17`. Verify: `git grep -n "S1 may start" docs/adr/TASKS.md` returns the lines counted in section 10 today and 0 after.

**S17.1 Pin the vocabulary reservation (tests only; optional).** First check whether the existing Python-side contract (`crates/py/tests/schemes_contract.rs:14`, which refuses `pr`) already pins `pr` and `issue` as built-in wire names. If a Rust-side pin is still wanted, add one new test fn in the `crates/tools` integration binary, `pr_and_issue_stay_builtin_wire_names`, asserting `Scheme::parse("pr") == Scheme::Pr`, `Scheme::parse("issue") == Scheme::Issue`, both in `Scheme::ALL`, and the `PR` and `ISSUE` rows of `url-vocab.json` with `selectors: true`. Do **not** pin the two scheme lists: `aggregate_file_write_approval` has no production caller, and `PathPolicy::resolve("pr://1")` is not refused (3.1), which looks like an omission rather than a contract (open question 5). Verify: `just remote remote-plain-test omp-tools` shows the new test name and a `Summary` line; `git diff --stat` shows test code only.

**S17.2 Prove the REST branch dead (test only).** New fn `legacy_rest_branch_is_unreachable` appended to the `forge_url.rs` test module. Over both `ForgeScheme`s, resources built from the segments `""`, `7`, `0`, `diff`, `all`, `ghe`, `ghe.example.com`, `owner`, `repo`, `acme`, `widget`, `..`, `%2e%2e` up to length 5, and queries `None`, `state=open`, `comments=0` (with the existing temp-checkout helper for bare numbers): assert that `numbered_ref(..) == Ok(None)` implies `Target::parse(..)` is `Err` or has a `View::List`. The tests sit behind `cap-forge` and `omp-envd` defaults to empty (`crates/envd/Cargo.toml:13`), so the remote run must enable it, for example `just remote remote-plain-test "omp-envd --features cap-forge"` (the recipe at `justfile:135` substitutes `pkg` as text; **UNVERIFIED**). A counterexample falsifies 3.5: keep the branch and record the shape.

**S17.3 Retire the dead REST branch (owner decision; blocked by the no-test-edits rule).** Delete `fetch` (`:538-592`), `fetch_comments` (`:600-651`), `Target::{accept, api_url, comments_url}`, `ForgeRepo::{api_url, api_url_for_host}`, `http_fault`, the `Fetch` enum (`:839`), the `omp_http` client field and constructor call (`:363`, `:376`), the `x-github-api-version` headers and dead imports. The assertions at `forge_url.rs:1646-1654` and `:1678-1685` call `api_url`, so deleting the code removes or rewrites tests: do not do it without the owner's explicit ruling; otherwise leave the branch. If approved, verify: `git diff --stat` shows `forge_url.rs` plus the tests the owner named; remote nextest for `omp-envd` with `cap-forge` green; `just remote check-minimal-caps` green; S17.1 and S17.2 unchanged.

**S17.4 D2.7 residue as adapter data (owner plus council; security).** Move `GITHUB_COM_TOKEN_VARS`, `GITHUB_ENTERPRISE_TOKEN_VARS`, `DEFAULT_FORGE_HOST`, the `"github"` credential spec and the `FetchHost::github` naming behind adapter claim data (`ForgeDynHosts::with_catalog_claims`, manifest `credentials.allow`). It is not a URL-scheme change and `ForgeCredentialBridge` is shared (3.5). Oracle: `forge_url.rs:1538`, `:1575`, `:1673`, probes `:1417`, `:1652`. Keep outside Gap 17 unless the owner folds it in.

**S17.5 Upstream parity options (class d; owner; each its own gap or ADR).** `?at=<generation>` for `pr://.../diff` is additive but gives meaning to a key omp2 ignores today and needs a cache-generation design in `ForgeCache`. `mr://` is class b: the only legal route is a declared scheme after S2-S5 plus an ADR amendment, because ADR 0050 D2 and Consequences say adapters document MRs as `pr://`.

Independent of Gap 17: ADR 0050 S2-S5 proceed on their own gates. The S2 test `declared_scheme_fold_refuses_builtin_and_unclaimed` is the permanent guard that `pr` and `issue` cannot be claimed.

## 8. Risks and edge cases

1. Re-homing on the unknown fallback changes six behaviours (6.2).
2. Case handling of the two scheme lists differs: `PathPolicy::resolve` normalizes only `local:/` (`path_policy.rs:248-255`) and `INTERNAL_PREFIXES` matches with a case-sensitive `starts_with` (`:70`), so `AGENT://x` skips the refusal (by reading, not run); `is_internal_resource` lowercases (`path.rs:91`). Neither changes `pr` or `issue` behaviour today.
3. Host case and routing: `Delegated` routes by ASCII-case-insensitive host (`TASKS.md:163`); `pr://Owner/Repo/7/diff` is an oracle row (`probes.rs:1446`).
4. Single-label hosts parse only in the numbered form (`forge_url.rs:1648-1667`); a second parse site would break it.
5. `pr://42` is rejected by the pure parser (`scheme.rs:476`) but the resolver expands bare numbers through `infer_repo` first (`forge_url.rs:1216`, `:1245`); a shim that forwards the raw URL or re-parses elsewhere diverges.
6. Extension reload: `Delegated` stays immutable and late catalog registration is out of scope (`forge_adapter/mod.rs:18-19`; dyn schema doc `:75-76`), so ADR 0050 S4 generation rebind does not apply to the forge path today.
7. Unavailable states: `gitlab.com` and `github.com` are always registered Dyn and answer `Unavailable` until devices mount (`TASKS.md:172`); changing registration order or activation changes the first-read error.
8. Cap off registers nothing; `just check-minimal-caps` (`justfile:232`) must stay green.
9. The forge tests sit behind `cap-forge`: a remote run without the feature can be green with zero forge tests. Require the new test names in the `Summary` output.
10. The shared main checkout moved under agents during this research (`b8032a94ad` to `f4186b783b`) and `/private/tmp` was wiped on a reboot; verify anchors at a pinned SHA and keep notes in the repo worktree.
11. Doc drift: `TASKS.md:155`, `:196`; `forge_url.rs:14-17`.

## 9. Open questions for the owner or coordinator

1. Accept class a (goal) and class b (literal reading)? Otherwise name the amendment path (Option B or C) and the additive-only exemption.
2. S17.3: delete the dead REST branch with its three `api_url` assertions, or keep it?
3. S17.4: schedule the D2.7 residue as an ADR 0049 follow-up?
4. Is `?at=<generation>` parity wanted (separate gap)? Is `mr://` confirmed not needed?
5. Is the omission of `pr://`, `issue://`, `history://` and `memory://` from `INTERNAL_PREFIXES` intentional? Consequence (3.1): an `edit` of `pr://1` is not refused as an internal scheme. Is `aggregate_file_write_approval` meant to be wired in? It has no production caller. A single source of truth for the two lists would remove the drift risk.

## 10. Tracks, verification, anchors

- **Tracks that produced nothing.**
  - No ledger row for Gap 17 exists: `git grep -i -E "forge-url-schemes|gap[ -]17"` over tracked files finds nothing at this HEAD, and the mission ledger `docs/research/upstream-gap-closure.md` is not present in this worktree (in the earlier worktree its table was empty). The definition came from the brief only.
  - `skill://` and `memory://` are not readable through `omp.read` in fabric (skills were read from `~/.omp/agent/managed-skills/`), and the mission prompt was visible only as a truncated memory excerpt, so the hard-constraint quote stops at "stop work on ...".
  - `sift-light` and `ctx_*` were withdrawn mid-run and results above about 4 KB were cut, so large files were read in line windows.
  - The first copy of this note was lost when `/private/tmp` was wiped on a reboot. This file is a rewrite from the same evidence with the later corrections folded in (3.1, 5, 6.2, 7, 8 item 2, 9 item 5, and the S2 check against origin).
  - No subagents were used: the question was answerable by direct reading, as `adw-research-dispatch` advises.
- **Done.** Static reads; one regex call graph for 3.5 (approximate); a `git diff --name-only` drift check; `gh`, `git ls-remote` and PR searches for S2; the anchor checks below. **Not done:** compile, test, lint or any run (BuildBuddy unused; nothing changed).
- **Anchor checks.**
  - First pass, at `b8032a94ad`: 242 `(path, line, needle)` citations, 0 failures.
  - Second pass, at the worktree HEAD and the upstream pin (this rewrite): 97 citations (those added by the later reads, a smoke sample of the older ones, and every upstream one), 0 failures. A needle must sit within 2 lines of the cited line.
  - Drift: `git diff --name-only b8032a94ad HEAD` lists 14 files (ADRs 0054, 0055, 0059, 0060 and 0061, two knowledge-base files, seven research notes); none is cited here. `git diff --name-only f4186b783b HEAD` over `crates`, `extensions`, ADR 0049, ADR 0050, `docs/adr/TASKS.md` and `justfile` is empty, so the first-pass line numbers hold at HEAD. ADR 0055 line 112 records ADR 0050 as "unaffected".
  - 33 distinct full repo paths cited here exist at HEAD (the one regex miss is the branch name `docs/adr0050-accept`). All 22 files with `pr://` or `issue://` hits under `crates/` are named in 3.6.
  - Quoted sizes re-measured: `url-vocab.json` 1659 bytes, `forge_url.rs` 2193 lines, upstream `issue-pr-protocol.ts` 560 lines, `forge-diff.ts` 141, `mr-protocol.ts` 124 (`wc -l` semantics), prompt files 122 and 165 characters.
  - To re-check a line: `git -C <repo> show <sha>:<path> | sed -n 'N,Mp'`.
- **Corrected during verification.** `mr-protocol.ts` is 124 lines, not 125; `cap-forge` appears in 23 files, not 22 (an earlier `head -22` truncated the listing); `is_internal_resource` is not live behaviour (no production caller), which changed 1.6, 3.1, 4, 6.4 step 5 and S17.1; grep's judgment of a declared-scheme root was read out of the dense-table route (6.2) instead of left open.
- **S2 absence, four ways.** `git grep declared_scheme_fold -- crates` is empty at HEAD; `gh api` commits and `gh pr list --state merged` show `origin/omp2` at `f4186b783b` with no scheme PR after #593; `gh pr list --state all --search "ADR 0050"` returns only #558, #588, #593 and #559; `git ls-remote --heads origin '*adr0050*' '*scheme*'` returns only `docs/adr0050-accept` and `feat/adr0050-s1-schemes`. `docs/adr/TASKS.md` still carries "S1 may start" on 2 lines (155 and 196) while 201-205 show S1 DONE, S2 OPEN, S3-S5 BLOCKED.
- **UNVERIFIED (carried).** (1) The legacy REST branch is unreachable (3.5). (2) Whether `aggregate_file_write_approval` has callers outside this repo (3.1). (3) Whether a declared-scheme root reaches `internal_root_fetch` in `grep` (6.2). (4) Whether `write` reaches `reject_uri_like_target` for `pr://`, and whether any edit mode can create `<cwd>/pr:/1` (3.1). (5) The exact remote invocation with `cap-forge` (S17.2).
- **Nothing was changed.** No tracked file was edited in either checkout. This note is the only new file; no ADR was written, because the disposition is class a/b and ADR 0049 D2.5 with ADR 0050 D1 already decide the question.

## 11. Review (EGM, 2026-11-13)

A fresh-context reviewer (zoom-out and design critic) settled the open items:

1. **Class.** Accepted: class a for the goal (ADR 0049 D2.5 already routes `pr://` and `issue://` through `ForgeResolver<Delegated<ForgeBackend>>`, `crates/envd/src/tool_url.rs:637-649`), class b for the literal move (`docs/adr/0049-tiered-integration-architecture.md:168` and `docs/adr/0050-extension-declared-url-schemes.md:48,136` reject new built-in forge schemes).
2. **Legacy GitHub REST branch.** Delete it, in two steps. Static tracing shows it is unreachable: `peel_selector` (`crates/forge/src/scheme.rs:249-266`) consumes the `/diff` forms and `Target::parse` rejects other trailing segments, so `numbered_ref` never yields the case that reaches `fetch` (`crates/envd/src/forge_url.rs:431`). List reads already go through `forge.search` (`forge_url.rs:520-525`). Step 1 replaces `fetch` and `fetch_comments` with an invalid-input fault and runs the forge tests on BuildBuddy with `cap-forge`. Step 2 deletes the branch, `Target::api_url`, `ForgeRepo::api_url`, `api_url_for_host`, the bridge's GitHub spec and the five `api_url` assertions (`forge_url.rs:1646, 1651, 1654, 1678, 1685`), keeping the identity assertions and `numeric_tail_disambiguates_host_from_diff_suffix`. Fix the module doc (`forge_url.rs:1-16`) and `TASKS.md:166`, which still describe the REST client as live. This removes dead code only. It answers UNVERIFIED item (1) above by static tracing; step 1's test proves it.
3. **ADR 0049 D2.7 residue.** A gated follow-up, not part of this gap: `GITHUB_COM_TOKEN_VARS`, `DEFAULT_FORGE_HOST`, `FetchHost::github` (`crates/envd/src/fetch_host.rs:67,122`, live egress routing with a protobuf variant at `:294,313`, so a rename needs a wire-compatibility plan), and `crates/app/src/update_cmd.rs:487-490` (GitHub tokens for release metadata, which needs its own class decision).
4. **`?at=` snapshot pin and `mr://`.** Not adopted. ADR 0049 and ADR 0050 reject `mr://`; `?at=` needs a cache-generation design with no current user need.
5. **`INTERNAL_PREFIXES`.** A real but latent defect in the `omp-edit` library only. `crates/edit/src/path_policy.rs:19` lists ten prefixes and omits `pr`, `issue`, `history`, `memory`, `omp`, `job`, `attachment`, `conflict`, `http` and `https`, so `resolve()` (`:69`) joins `pr://1` onto the working directory. The shipped tools are not affected: `write` rejects it at `crates/tools/src/write.rs:848` via `reject_uri_like_target` (`:1257`), and the envd edit path rejects non-file schemes (`crates/envd/src/tool_document.rs:881`). This answers UNVERIFIED item (4). `omp_edit::Session` has no production constructor. Fix: treat any valid `scheme://` other than `local:`, `file:` or a Windows drive as internal, with one test per previously missing scheme in `crates/edit`. The behaviour change is confined to the library.

Approved follow-up slices: S17.2 (unreachability test), S17.3 (two-step deletion), and a separate small slice for the `INTERNAL_PREFIXES` fix. The D2.7 residue waits for its own ADR 0049 follow-up.

