# ADR 0050 S4 — reload / quarantine rebind

Base: current tree with S1–S3 landed (`SchemeGeneration`, fold at FREEZE,
live-only peel, D5 judgment). ADR:
[0050](../adr/0050-extension-declared-url-schemes.md). TASKS: ADR 0050 S4
**BLOCKED**.

## Exit (normative)

- After `/reload-plugins` → `Mutation::ReloadExtensions` →
  `ExtensionReloadHandle::reload`, a previously folded declared scheme still
  reads (new host generation identity rebound into the same envd-owned map).
- After `quarantine_extensions`, that extension’s schemes are gone
  (`is_live` false; `resolve_read` / `judge` undeclared) and a built-in route
  (`pr://` / dense arm) still reads.
- ADR D6: scheme is live only while its registering generation is live; a
  quarantined spelling does not fall through to another extension’s handler.

## Current call chains (path:line on this tip)

### `/reload-plugins` → reload

| Step | Location |
| --- | --- |
| Chat command posts mutation | `crates/chat/src/commands/tools.rs:544-547` |
| Mutation enum | `crates/chat/src/overlays/services.rs:1097-1098` |
| App applies mutation | `crates/app/src/chat_services/mod.rs:550` |
| Spawns `reload.reload()` | `crates/app/src/chat_services/extensions.rs:72-83` |
| Handle | `crates/envd/src/lib.rs:701-708` (`ExtensionReloadHandle::reload`) |
| Server | `crates/envd/src/server/session.rs:1145-1146` |
| Supervisor | `crates/envd/src/worker.rs:2734-2739` (`ExtHostSupervisor::reload`) |
| Per-host command | `crates/envd/src/worker.rs:3058-3067` → `ControlHostCommand::Reload` |
| Supervisor arm | `crates/envd/src/worker.rs:4079-4117` (`restart_with_authority` + `refresh_control_generation`) |
| Re-FREEZE | `crates/envd/src/worker.rs:4142-4172` |

### Quarantine

| Step | Location |
| --- | --- |
| Handle | `crates/envd/src/lib.rs:716-719` |
| Server | `crates/envd/src/server/session.rs:1156-1157` |
| Workspace-update caller | `crates/envd/src/server/session.rs:2197-2204` |
| Supervisor stop + await | `crates/envd/src/worker.rs:2768-2790` |
| Availability “static routes” | `publish_host_down` / availability sink — **tools/MCP stubs**, not URL schemes (`worker.rs:1593-1607`) |

### SchemeGeneration bind / fold / clear today

| Concern | Location | Behavior |
| --- | --- | --- |
| Install once per env open | `crates/envd/src/server/session.rs:39-49`, `:240`, `:505`, `:804` | One `Arc<SchemeGeneration>`; inject as `host_resources` if empty; `ExtHostConfig::bind_scheme_generation` |
| Bind callbacks | `session.rs:333`, `:641`, `:886` | `scheme_generation.bind_callbacks(...)` |
| Config field | `worker.rs:422-423`, `:459-463` | Held on `ExtHostConfig` only through `spawn` |
| Initial FREEZE fold | `worker.rs:1478-1486`, `:1514-1556`, call site `:1905` | `fold_declared_schemes` → `replace_owner(extension, Some(identity), claims)` |
| Hot-reload / crash FREEZE | `worker.rs:4163-4171` | **`scheme_generation: None`** — FREEZE runs, **does not re-fold** |
| `PendingControlActivation` | `worker.rs:1165-1194` | **No** `scheme_generation` field |
| Drop / clear API | `scheme_generation.rs` | **Missing** named drop; empty `replace_owner(owner, None, vec![])` already clears that owner’s rows (`:228-250`) |
| Quarantine vs schemes | `worker.rs:2768-2790` | Stops hosts only; **does not** touch `SchemeGeneration` |
| Production CONTROL read allowlist | `worker.rs:2397-2405` | `omp.schemes.read` **not** listed → `InvalidOperation` via `CallbackDispatcher` (S2 tests used `SchemeTestReader`) |

## Gaps (answers)

1. **Does reload re-fold schemes?** No. `refresh_control_generation` passes
   `None` into `freeze_control_registry` (`worker.rs:4163-4171`). Python
   re-registers and FREEZE payload still carries `schemes`, but envd never
   calls `replace_owner` again. Stale `LiveScheme.identity` keeps the old
   `host_generation`; live dispatcher fences reject it
   (`worker.rs:2420-2424` → `StaleHostGeneration`).
2. **Does quarantine drop that extension’s schemes?** No. Host shutdown only.
   Stale `LiveScheme` rows remain; `is_live` stays true; reads fail at
   dispatch / unavailable host, not as undeclared. Built-in dense routes are
   unrelated and already stay up.
3. **Crash restart** shares the same `refresh_control_generation` hole
   (`worker.rs:3820-3830`).

## Locked decisions (no owner questions)

1. **Same envd-owned map, owner-scoped replace (Option A continues).** Do not
   allocate a new `SchemeGeneration` per reload. Do not enable RPC
   `set_host_uri_schemes`. Rebind = `replace_owner` with the advanced
   `ControlConnectionIdentity` after FREEZE on the hot-reload / crash path.

2. **Thread `scheme_generation` onto `PendingControlActivation`.** At spawn
   (`worker.rs` ~1947), copy `config.scheme_generation.clone()` onto the
   activation. `refresh_control_generation` passes
   `activation.scheme_generation.as_ref()` into `freeze_control_registry`
   instead of `None`. Initial spawn path unchanged.

3. **Clear-before-down; re-fold on successful FREEZE (D6).** Add
   `SchemeGeneration::clear_owner(&self, owner: impl Into<Str>)` as a named
   API (implementation may delegate to `replace_owner(owner, None, vec![])`).
   Call `clear_owner(extension_id)` from `publish_host_down` (covers reload
   start, crash, and supervisor shutdown). Quarantine additionally clears
   every matching owner **before** sending `Shutdown` (idempotent with
   `publish_host_down`). While the host is down, the spelling is not live;
   after FREEZE succeeds, `replace_owner` restores it with the new identity.
   A FREEZE failure leaves the owner cleared (correct: generation did not
   come back).

4. **Quarantine does not free a spelling to a refused co-claimant.** First
   live claim still wins at fold. Clearing the quarantined owner removes its
   rows; another extension that was refused earlier does **not** auto-inherit
   the spelling (no fall-through). A later successful fold by a live
   extension may claim it — same as a fresh install.

5. **Allowlist `omp.schemes.read` on the callback dispatcher in this PR.**
   Add `\|\| operation.starts_with("omp.schemes.")` beside the existing
   prefixes in `dispatch_extension_callback` (`worker.rs:2397-2405`). Required
   for production `HostResources::resolve_read` after rebind; S2/S3 exits
   used `SchemeTestReader` and never hit this gate. Scope: allowlist only —
   no new CONTROL surface.

6. **Built-in route proof stays on the dense arm.** Assert
   `SchemeGeneration::is_builtin_wire_name("pr")` and that folding / clearing
   extension schemes never registers `pr` / `https` into the generation (S2
   already covers fold refuse). Do not route `pr://` through
   `HostUriResolver`.

7. **Test locus = envd unit / focused integration, not chat UI.** Prove the
   envd seams the ADR names (`ExtensionReloadHandle` / supervisor reload +
   quarantine + generation). Do not drive TUI `/reload-plugins` in S4. Chat
   → handle wiring is already landed and unchanged.

8. **S5 share seams, do not implement S5.** Keep `snapshot()` / `LiveScheme`
   / `is_live` / `clear_owner` / `replace_owner` stable. S5 listing reads
   `snapshot()` (model-facing, separate from `omp.urls.schemes`). S5
   conformance reuses S4 exit tests plus S3 peel/judgment. No listing API in
   S4.

## Non-goals

- S5 listing / joined conformance suite beyond S4 exits.
- Writable declared schemes; RPC host partition; vocab / `Scheme` enum growth.
- Changing chat command or `Mutation::ReloadExtensions` wiring.
- Merging S3 selector/judgment work.

## File allowlist

**Modify**

- `crates/envd/src/tool_url/scheme_generation.rs` — `clear_owner`; unit tests
- `crates/envd/src/worker.rs` — activation field; refresh FREEZE arg;
  `publish_host_down` clear; quarantine clear; `omp.schemes.` allowlist
- `crates/envd/tests/declared_scheme_s4.rs` (**create**) — exit proofs
- `docs/adr/0050-extension-declared-url-schemes.md` — Status in omp
- `docs/adr/TASKS.md` — S4 → DONE
- `docs/adr/STATUS.md` — 0050 row

**Do not modify**

- `crates/tools/url-vocab.json`, `Scheme` enum, latched roster
- `crates/chat/**` reload command (already correct)
- `crates/py/python/omp/schemes.py` (re-register on new process is enough)
- S5 listing surface

## Tests (S4 exit)

New file `crates/envd/tests/declared_scheme_s4.rs`:

| Test | Proof |
| --- | --- |
| `declared_scheme_reload_rebinds_identity_and_still_reads` | Fold with identity gen N + test reader → body; `clear_owner` + `replace_owner` with gen N+1 (reload rebind shape) → still reads; `LiveScheme.identity.host_generation` is N+1 |
| `declared_scheme_quarantine_drops_scheme_builtin_intact` | Fold `widget`; `clear_owner`; `!is_live("widget")`; `judge` → `Undeclared`; `resolve_read` fails as undeclared/no-handler; `is_builtin_wire_name("pr")` still true; generation snapshot has no `pr` |
| `declared_scheme_quarantine_does_not_fall_through` | Owner A folded; owner B collision refused; clear A; B still absent until B folds; no silent inherit |
| Optional colocated unit | `clear_owner` is idempotent; other owners preserved |

Keep S2 (`declared_scheme_fold.rs`) and S3 (`declared_scheme_s3.rs`) green.

**Verification commands**

```sh
just test-pkg omp-envd
# or focused:
cargo nextest run -p omp-envd declared_scheme_
```

## Implementation order

1. `clear_owner` + unit tests on `SchemeGeneration`.
2. Thread `scheme_generation` onto `PendingControlActivation`; pass into
   `refresh_control_generation` → `freeze_control_registry`.
3. `clear_owner` from `publish_host_down` and `ExtHostSupervisor::quarantine`.
4. Allowlist `omp.schemes.` on `dispatch_extension_callback`.
5. Exit tests in `declared_scheme_s4.rs`.
6. ADR Status / TASKS / STATUS.md.

## S5 shared seams (preview only)

| Seam | S4 provides | S5 uses |
| --- | --- | --- |
| `SchemeGeneration::snapshot` | already public | model-facing declared listing (not `omp.urls.schemes`) |
| `is_live` / `judge` | S3 | conformance selectors + effects |
| `replace_owner` / `clear_owner` | S4 completes lifetime | conformance reload + quarantine steps |
| `declared_scheme_{fold,s3,s4}.rs` | slice exits | join into one conformance test |

## Risks

- Mid-reload reads see a temporary undeclared spelling (accepted under D6).
- Allowlist widening is intentional and narrow (`omp.schemes.` only).
- Full multi-process reload harness is out of S4; identity rebind shape is
  the load-bearing contract the production `refresh_control_generation` path
  must call.
