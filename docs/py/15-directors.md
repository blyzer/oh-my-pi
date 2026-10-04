# Directors and Components

> Owner document for `@omp.director` and `@omp.component`: the two engine-owned extension seams
> that replaced regimes. `omp.policy` remains the security and sandboxing namespace described in
> [`06-policy.md`](06-policy.md).
> Siblings: [`05-hooks.md`](05-hooks.md) for one-shot extension hooks,
> [`08-context.md`](08-context.md) for context projection,
> [`09-journal.md`](09-journal.md) for the DOM patch builders a Component returns, and
> [`12-agents.md`](12-agents.md) for agent lifecycle.

> **Supersedes `15-regimes.md`.** `@omp.regime`, `omp.regimes`, `omp.Decision`, the `ctx` /
> `next_` handler pair, the fixed `Point` events (`CONTEXT`, `SETTLE`, ...), regime activation
> (`omp.regimes.start`), and the Rust `Regime` / `RegimeSet` / `Arbiter` no longer exist.
> `crates/py/python/omp/regimes.py` was deleted by commit `47e02d12a6`, and
> `crates/agent/src/{regime,arbiter,control,tree}.rs` by `d98ed242f5`. There are no compatibility
> aliases (`AGENTS.md`: clean cutovers). The design is in
> [ADR 0015](../adr/0015-directors.md) (Directors) and [ADR 0003](../adr/0003-one-authoritative-session-tree.md)
> (the session tree and its patch stream).
> Statements below are checked against the code named beside them. Where the code does not settle a
> question, the text says **Unknown** rather than guessing.

## Purpose

An extension can keep behavior in exactly two engine-owned places, and both are declared at import
time and run in the killable extension-host process (`crates/py/python/omp/extensions.py`):

| Seam | Job | Runs |
|---|---|---|
| **Director** | Keeps control across turns: refines the next inference and judges each candidate yield. | At every inference and every candidate yield while engaged. |
| **Component** | Reduces a journal entry into ordinary DOM operations under the session tree. | Once per newly appended live entry; never on replay. |

There is deliberately no third place. The registrar admits no custom journal kind, restore
callback, mutable session map, or extension-owned state store (`crates/agent/src/extensions.rs`,
`ExtensionRegistrar`). Durable state is a property on a Director element or a node produced by a
Component; module globals are never authoritative (ADR 0003 rule 3).

## Where each piece lives

| Piece | Location |
|---|---|
| `Director` trait, `Verdict`, `Slot`, `BindValue`, `DirectorStack`, `DirectorRegistry` | `crates/agent/src/director.rs` |
| Built-in Directors (`advisor`, `autoresearch`, `compaction`, `force_tool`, `goal`, `loop_mode`, `plan`, `prewalk`, `todo_reminder`, `vibe`) | `crates/agent/src/directors/` |
| `ExtensionRegistrar`, `LiveComponent` | `crates/agent/src/extensions.rs` |
| `Component`, `ComponentRegistry` (replay fold) | `crates/session` |
| `PyDirector`, `PyComponent` (Python adapters) | `crates/envd/src/exthost/extensions.rs` |
| Python decorators and dispatch | `crates/py/python/omp/extensions.py`, `crates/py/python/omp/_registry.py` |
| DOM patch builders | `crates/py/python/omp/journal.py` ([`09-journal.md`](09-journal.md)) |

## The Director model

A Director stack is a live subtree of the session DOM (`<meta><directors>`), never a runtime array
promised to serialize later. The loop only walks it (ADR 0015):

- `prepare_inference` walks outermost to innermost; the innermost Director refines the request its
  parent was about to make.
- `on_yield` walks innermost to outermost and each Director returns one `Verdict`.
- A Director that needs an exclusive resource declares a **claim** on a `Slot`. Claims held by an
  active Director block another Director that claims the same slot; the newcomer is queued
  (FIFO) instead of nested and is promoted when the slot frees.
- Pause keeps the subtree materialized, so resuming restores exact Director and child state, and
  rewind or resume re-derive the stack from the journal.

`Slot` values (`Slot` in `director.rs`): `mode` (the user-visible operating mode), `loop`
(ownership of automatic continuation), `tool_choice` (ownership of native tool selection), and
`worktree` (ownership of the worktree view).

| Verdict | Meaning |
|---|---|
| `pass` | Let the next outer Director inspect the candidate yield. |
| `continue` | Consume the yield and run another turn; may carry a `reminder` developer aside. |
| `yield` | Consume the candidate and return it to the user. |
| `push` | Engage a child Director on top of this one and continue the loop. |
| `done` | Pop this Director, then offer the same candidate to its parent. |
| `fail` | Pop this Director, journal an error notice, and re-offer the candidate to its parent. |

A **hook** observes or edits one inference or turn and cannot own a yield. Anything that needs the
second MUST be a Director; anything that needs only the first MUST NOT be
([`05-hooks.md`](05-hooks.md)).

The agent loop is a generic hook surface. It does not carry per-feature outcome tracking. Rules
that watch model output while it streams (the TTSR family) are intended to be built as a Director;
`crates/driver/src/discovery/rules.rs` parses their `condition` and `scope` frontmatter, and the
built-in `stream-rules` Director watches the response through the generic stream hook. The
extension-facing `ttsr_triggered` event (`toolhost.proto`, `omp.events.TtsrTriggeredEvent`) remains
in the vocabulary, but no emitter for it has been implemented under `crates/agent` yet.

## `@omp.director`

```python
@omp.director(
    director_id,
    *,
    claims=(),       # subset of {"mode", "loop", "tool_choice", "worktree"}
    binds=None,      # {convar_name: bool | int | float | str}
)
```

Declaration rules (all enforced in `extensions.py`):

- `director_id` is a non-empty string without surrounding whitespace; a repeated id raises
  `omp.DuplicateRegistration`.
- `claims` entries must be unique members of the four slots above; anything else raises
  `ValueError`.
- `binds` values must be scalar `bool | int | float | str`; a `dict`, `list`, `tuple`, `set`, or
  `None` raises `TypeError`. The binds are convar values installed on the engagement layer while
  the Director is active (`Director::binds`, ADR 0012), for example `binds={"ai_fastmode": True}`.
- The decorated object is a callable or a class. A class is instantiated with no arguments for
  each dispatch, so it must not keep state in `self`.
- Registration after FREEZE raises `omp.DeclarationSealed`.

### Callbacks

The handler is looked up as `before_inference` / `on_yield` on the object; a bare callable serves as
both.

```python
@omp.director("verify-before-yield", claims=("loop",))
class VerifyBeforeYield:
    def on_yield(self, event):
        if event["had_tool_calls"] or "verified" in event["assistant_text"]:
            return "pass"
        return {
            "verdict": "continue",
            "reminder": "State how you verified this change.",
            "updates": {"asked": True},
        }
```

**`before_inference(event)`**, may be `async`. `event` is a dict with `state` (this Director's
durable `state/*` properties, keys without the prefix) and the request summary
`message_count`, `tool_count`, `max_output_tokens`. Return `None` (unchanged) or a mapping:

| Key | Meaning |
|---|---|
| `prepared` | `"unchanged"` (default) or `"rebuild"` to re-project the request from the updated tree. |
| `ops` | DOM operations (see [`09-journal.md`](09-journal.md)), journaled as one `patch@1` labeled `extension.director.before_inference`. |

**`on_yield(event)`**, must be synchronous; an awaitable raises `TypeError`. `event` is a dict with
`state`, `had_tool_calls`, `assistant_text`, and `stop_reason`. Return a verdict string or a mapping:

| Key | Meaning |
|---|---|
| `verdict` | One of `pass`, `continue`, `yield`, `done`, `push`, `fail`. Any other value becomes a `fail`. |
| `reminder` | With `continue`: developer aside appended before the next turn. |
| `reason` | With `fail`: the error text. |
| `child` | With `push`: `{"id", "callable", "claims", "binds"}` of the child Director. A malformed child becomes a `fail`. |
| `updates` | `{key: bool | int | float | str}` durable state committed on this Director's element in the same tick. Non-scalar values are dropped. |

Nothing mutates live agent state while the callback runs: effects are the returned verdict,
`updates`, and `ops`.

### Failure

`PyDirector` calls the callback with a per-call timeout (default
`DEFAULT_EXTENSION_HOOK_TIMEOUT`, 5 s, `crates/envd/src/exthost/extensions.rs`). A raised
exception, timeout, or transport failure in `on_yield` becomes
`Verdict::Fail("Python extension Director callback failed")`, so the Director is popped and the
candidate goes to its parent. In `before_inference` it surfaces as
`DirectorError::ExtensionCallback`; what the loop does with that error is **Unknown** (not read
here).

### Engagement

Declaring a Director installs its constructor in the `DirectorRegistry`
(`ExtensionRegistrar::install`; production wiring at `crates/driver/src/headless/kernel.rs`). A
registered extension Director takes effect only when something engages it with
`DirectorStack::engage_registered`, or when another Director's `push` verdict returns it as a
child. **Unknown:** the only callers of `engage_registered` in the tree are tests
(`crates/e2e/tests/p9_extension_control.rs`, `crates/agent/tests/empty_output.rs`), and
`crates/app/src/chat_control.rs` engages only the built-in ids. No production path that engages a
Python-declared Director was found. Treat an extension Director as registered but not
user-reachable until that changes.

## `@omp.component`

```python
@omp.component(
    component_id,
    *,
    interested=("patch@1",),   # non-empty, unique, drawn from the closed journal vocabulary
)
```

A Component is a pure journal-to-`<meta>` reducer. `interested` names journal kinds by
`name@rev` and must be a non-empty, duplicate-free subset of the engine's closed vocabulary:
`journal@1`, `turn.start@1`, `msg.user@1`, `msg.assistant.start@1`, `stream@1`,
`msg.assistant.end@1`, `tool.call@1`, `tool.update@1`, `tool.result@1`, `turn.receipt@1`,
`patch@1`, `compaction@1`. Extension-defined kinds are unsupported by design; `@omp.entry_kind`
does not exist.

The callback is called as `callback(entry)` and must be synchronous (an awaitable raises
`TypeError`). `entry` is a dict:

| Key | Meaning |
|---|---|
| `id` | Entry id. |
| `kind`, `rev` | Journal kind name and revision. |
| `by` | Id of the causing entry, or `None`. |
| `prior` | Id of the previous entry it supersedes, or `None`. |
| `label` | Optional patch label. |
| `data` | The entry's raw JSON payload as `str`. `omp.journal.decode` takes canonical `bytes`, so `json.loads` is the safe reader; whether journaled payloads are always canonical is **Unknown**. |

Return `None` (no change), a mapping `{"ops": [...]}`, or an iterable of operations. Build
operations with `omp.journal.insert`, `set_prop`, `remove`, `move`, and `patch`
([`09-journal.md`](09-journal.md)).

Semantics (`PyComponent`, `crates/agent/src/loop.rs`):

- The reducer runs live only, right after each committed entry whose kind it is interested in.
  The kernel commits the returned operations as one `patch@1` labeled `ext:<component_id>` whose
  cause is that entry.
- Replay applies the durable `patch@1` directly and never calls Python again, so replay is
  deterministic and does not depend on the extension being installed.
- A callback failure or malformed operations leaves the DOM unchanged and appends the notice
  "Python extension Component callback failed" to the current turn.

**Unknown:** how a Python Component obtains the integer handles its operations need. The callback
receives only `entry` (no DOM, no handles), and `omp.journal.insert(parent, after, tag, ...)`
requires a positive-integer parent handle. The Rust `LiveComponent::reduce` receives the DOM but
`PyComponent` ignores it. Also, `crates/py/tests/frozen_surface.rs` declares a Component as
`ext_state(entry, dom)` returning a string-handle tuple; that test only proves declaration
projection, and it does not match the one-argument call in `_dispatch_component_apply`.

## Declaration and lifecycle

- Decorators record import-time metadata only; no filesystem, network, or process operation
  happens at import (`omp/__init__.py`).
- FREEZE seals the registry. A late `omp.director(...)` / `omp.component(...)` raises
  `omp.DeclarationSealed`. The frozen projection lists `directors` (id, callable, claims, binds,
  trigger) and `components` (id, callable, interested, trigger)
  (`crates/py/tests/extension_registrar_contract.rs`).
- A callable is identified by `module.qualname`; the host rejects a dispatch whose identity does
  not match the frozen registry.
- Python callbacks run in the killable extension host; the engine, not the extension, owns
  ordering, durability, and rewind.

## Removed regime vocabulary

The wire and manifest vocabulary that once carried regimes is gone too, so no part of the tree
can declare, start, or dispatch one (`crates/envd/tests/control_authority.rs` and
`crates/envd/tests/domain_control_router.rs` keep asserting that `omp.regimes.start` is not
handled):

- `crates/proto/proto/omp/toolhost/v1/toolhost.proto` no longer defines the `Regime*` messages
  and enums. `HostFrame` tag 16 and `WorkerFrame` tag 21 (both named `regimes`) are `reserved`,
  so the numbers and names cannot be reused.
- `crates/ext/src/config.rs` no longer has a `regimes` static-declaration class
  (`StaticDeclarationClass::Regime`); a manifest row with `kind = "regime"` is rejected as an
  unknown declaration kind.
- The staged-proposal limit rejection is `ProposalRejection::ProposalLimitReached` (wire name
  `proposal_limit_reached`, formerly `regime_limit_reached`), and the goal and staging doc
  comments describe goal control and proposals rather than regimes.

## Design boundary

Keep:

- one authoritative session tree; Directors and Components only read and patch it;
- Directors for anything that needs a candidate yield, Components for journal-derived state;
- closed verdict vocabulary and closed journal kind vocabulary;
- claims on `Slot`s instead of per-feature `*_mode_enabled` flags or private mutexes between
  extensions.

Do not add:

- extension-defined journal kinds, restore callbacks, or extension-owned state stores;
- a second control surface parallel to `Verdict` (a decision object, a `ctx` / `next_` pair, a
  campaign arbiter);
- compatibility aliases for `regime`, `Decision`, `campaign`, `Ladder`, or `omp.regimes`
  (`frozen_surface.rs` asserts `omp.campaign`, `omp.CampaignScope`, and `omp.Ladder` are absent).
