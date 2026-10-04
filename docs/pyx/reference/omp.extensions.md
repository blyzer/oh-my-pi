# `omp.extensions`

`omp.extensions` declares the two engine-owned seams that keep behavior across turns: a Director that judges candidate yields, and a Component that reduces journal entries into session-tree operations. Both are declared at import time and run in the killable extension-host process. Reach for them when a one-shot hook is not enough, because the behavior must keep control of the loop or retain durable state.

`director` and `component` are re-exported from top-level `omp`.

```python
import omp


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

See [Policy](../guides/policy.md) for how these relate to hooks and sandbox policy, and [`omp.journal`](omp.journal.md) for the DOM operation builders a Component returns. The full design is in `docs/py/15-directors.md`.

## Directors

### `omp.director`

```python
def director(
    director_id: str,
    *,
    claims: Sequence[str] = (),
    binds: Mapping[str, bool | int | float | str] | None = None,
) -> Callable[[_T], _T]
```

Registers a lifecycle callback on the engine Director stack. The decorated object is a callable or a class; a class is instantiated with no arguments for each dispatch, so it must not keep state in `self`. A callable provides `before_inference` and `on_yield` behavior, either as methods of a class or, for a bare callable, as both.

**Parameters**

- `director_id`: Non-empty id without surrounding whitespace. A repeated id raises `DuplicateRegistration`.
- `claims`: Unique members of `"mode"`, `"loop"`, `"tool_choice"`, and `"worktree"`. A Director that holds a slot blocks another that claims the same slot; the newcomer is queued instead of nested.
- `binds`: Scalar convar values (`bool | int | float | str`) installed on the engagement layer while the Director is active.

**Raises**: `TypeError` for a non-callable target or a non-scalar bind; `ValueError` for an empty id, a repeated claim, or an unknown claim; `DeclarationSealed` after the registry freezes.

### `before_inference(event)`

May be `async`. `event` is a dict with `state` (the Director's durable properties) plus `message_count`, `tool_count`, and `max_output_tokens`. Return `None` or a mapping:

| Key | Meaning |
|---|---|
| `prepared` | `"unchanged"` (default) or `"rebuild"` to re-project the request from the updated tree. |
| `ops` | DOM operations (see [`omp.journal`](omp.journal.md)), committed as one `patch@1`. |

### `on_yield(event)`

Must be synchronous; an awaitable raises `TypeError`. `event` is a dict with `state`, `had_tool_calls`, `assistant_text`, and `stop_reason`. Return a verdict string or a mapping:

| Key | Meaning |
|---|---|
| `verdict` | `pass`, `continue`, `yield`, `done`, `push`, or `fail`. Any other value becomes a `fail`. |
| `reminder` | With `continue`: a developer aside appended before the next turn. |
| `reason` | With `fail`: the error text. |
| `child` | With `push`: `{"id", "callable", "claims", "binds"}` of the child Director. |
| `updates` | Scalar `{key: value}` state committed on the Director's element in the same tick. Non-scalar values are dropped. |

Nothing mutates live agent state while a callback runs: effects are the verdict, `updates`, and `ops`. A callback that raises or times out in `on_yield` becomes a `fail`, so the Director is popped and the candidate goes to its parent.

## Components

### `omp.component`

```python
def component(
    component_id: str,
    *,
    interested: Sequence[str] = ("patch@1",),
) -> Callable[[_T], _T]
```

Registers a pure journal-to-`<meta>` reducer. The callback is called as `callback(entry)` and must be synchronous.

**Parameters**

- `component_id`: Non-empty id without surrounding whitespace.
- `interested`: A non-empty, duplicate-free subset of the engine's closed journal vocabulary: `journal@1`, `turn.start@1`, `msg.user@1`, `msg.assistant.start@1`, `stream@1`, `msg.assistant.end@1`, `tool.call@1`, `tool.update@1`, `tool.result@1`, `turn.receipt@1`, `patch@1`, `compaction@1`. Extension-defined kinds are not supported.

**Returns**: The decorated callable.

`entry` is a dict with `id`, `kind`, `rev`, `by`, `prior`, `label`, and `data` (the raw JSON payload as `str`). Return `None`, a mapping `{"ops": [...]}`, or an iterable of operations built with `omp.journal.insert`, `set_prop`, `remove`, `move`, and `patch`.

The reducer runs only for newly appended live entries. The host commits the returned operations as one `patch@1` labeled `ext:<component_id>`; replay applies the durable patch and never calls Python again. A failing callback leaves the tree unchanged.

**Raises**: `TypeError` for a non-callable target; `ValueError` for an empty id, an empty or duplicated `interested`, or an unknown kind; `DeclarationSealed` after the registry freezes.
