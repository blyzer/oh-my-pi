# Upstream gap closure (OMPv1 c308970263 to omp2)

This ledger tracks each functional gap between omp2 and upstream OMPv1 at
can1357/oh-my-pi `c308970263` (v18.20.0, 2026-11-13). Each gap has a research
note under `docs/research/`. Gaps classified as a port or a proposal also have
an ADR.

Classes: **a** already covered, **b** not needed, **c** port, **d** propose.

| # | Gap | Class | Research | ADR | Status | PRs |
|---|---|---|---|---|---|---|
| 1 | Subagent isolation (CoW backends, ovnfs) | pending | | | Research in progress | |
| 2 | Mnemopi model-free fact extraction | c | | 0062 | Research and ADR in progress; prototype commit daab333d13 local, ungated | |
| 3 | Semantic find | d | [semantic-find](semantic-find.md) | [0060](../adr/0060-semantic-find.md) | Proposed; EGM review running | |
| 4 | Desktop intervention guard and XKB | c | [desktop-intervention-guard](desktop-intervention-guard.md) | | Research done; ADR pending | |
| 5 | Automatic reasoning effort | c | [auto-reasoning-effort](auto-reasoning-effort.md) | | Research done; ADR pending | |
| 6 | In-process shell builtins | c (rsync b) | [shell-builtins](shell-builtins.md) | [0054](../adr/0054-shell-builtins.md) | Proposed; EGM review running | |
| 7 | Config discovery | see note | [config-discovery](config-discovery.md) | | Research done; ADR pending | |
| 8 | Web readers and search backends | a (readers) + c | [web-readers](web-readers.md) | [0055](../adr/0055-web-search-backends-and-reader-fidelity.md) | Proposed; EGM review running | |
| 9 | Subagent tools | c (/seance), d (probe) | [subagent-tools](subagent-tools.md) | 0056 | ADR in progress | |
| 10 | Session tools | c, d, b | [session-tools](session-tools.md) | 0057 | ADR in progress | |
| 11 | Context tools | a, b, d | [context-tools](context-tools.md) | 0058 | ADR in progress | |
| 12 | TUI job widgets and charts | see note | [tui-job-widgets](tui-job-widgets.md) | | Research done; ADR pending | |
| 13 | Browser inspection, ABKit | pending | | | Research in progress | |
| 14 | Memory backends | pending | | | Research in progress | |
| 15 | Providers and sign-in | c, d | [providers-signin](providers-signin.md) | [0059](../adr/0059-provider-catalog-additions.md) | Proposed; EGM review running | |
| 16 | Ratchet and skillshare | a (skillshare) + d (ratchet) | [ratchet-skillshare](ratchet-skillshare.md) | [0061](../adr/0061-ratchet-policy.md) | Proposed; EGM review running | |
| 17 | Forge URL schemes out of core | a (goal) + b (literal move) | [forge-url-schemes](forge-url-schemes.md) | none | Closed: ADR 0049 D2.5 already routes pr:// and issue:// to forge extensions; moving the spellings is forbidden by ADR 0050 D1 and the additive rule | |
| 18 | Generated RPC clients | pending | | | Research in progress | |
| 19 | VM sandbox fallback, Windows CI | pending | | | Research in progress | |
