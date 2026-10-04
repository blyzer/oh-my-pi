# `omp.journal`

Use `omp.journal` to build the DOM operations a Component returns and to read the engine's immutable journal projections. Only the engine writes the session journal: an extension does not append entries or define entry kinds. It contributes durable changes by returning ordinary DOM operations from an `@omp.component` callback (or `ops` from an `@omp.director` callback), and the host commits them as one `patch@1` entry.

```python
import omp

@omp.component("review-marks", interested=("tool.result@1",))
def mark(entry: dict) -> dict:
    return omp.journal.patch(
        omp.journal.set_prop(7, "reviewed", True),
    )
```

This module performs no I/O. Every builder is pure, and every node handle must be a positive `int`; a `bool`, `0`, or a negative number raises `TypeError`.

`EntryAccessDenied`, `EntryId`, `EntryTooLarge`, `EntryUndecodable`, `JournalEntry`, `JournalError`, and `JournalIndeterminate` are also re-exported from top-level `omp`. Their canonical reference entries remain on this page.

See [Agents and sessions](../guides/agents-and-sessions.md), [`omp.sessions`](omp.sessions.md), and [`omp.context`](omp.context.md).

## Identifiers and records

### `omp.journal.EntryId`

```python
@dataclass(frozen=True, slots=True, order=True)
class EntryId:
    session: str
    index: int

    @classmethod
    def parse(cls, value: str) -> EntryId

    def __str__(self) -> str
```

Opaque, totally ordered physical index within one session journal. String form is `<session_id>:<index>`.

**Parameters**: `parse()` accepts only the canonical string form; the index must contain canonical ASCII decimal digits without redundant leading zeroes.

**Returns**: A parsed `EntryId`.

**Raises**: `TypeError` for a non-string and `ValueError` for a non-canonical id.

```python
entry_id = omp.EntryId.parse("01JSESSION:42")
assert str(entry_id) == "01JSESSION:42"
```

### `omp.journal.JournalEntry`

```python
@dataclass(frozen=True, slots=True)
class JournalEntry(Generic[_T]):
    id: EntryId
    kind: str
    rev: str
    ts: int
    principal: Principal
    provenance: Provenance
    value: _T | None
    raw: bytes
    display: bool
    in_context: bool
    artifact: ArtifactRef | None = None
```

Immutable read-only projection of one engine journal entry.

| Field | Meaning |
|---|---|
| `id` | Physical session-journal position. |
| `kind` / `rev` | Engine entry identity and revision. |
| `ts` | Host timestamp. |
| `principal` / `provenance` | Core-authenticated writer and package provenance. |
| `value` | Decoded value supplied by the host, otherwise `None`. |
| `raw` | Canonical JSON bytes. |
| `display` / `in_context` | Host projection flags. |
| `artifact` | Spill reference when the payload is stored outside the inline record. |

## Errors

### `omp.journal.JournalError`

```python
class JournalError(OmpError):
    def __init__(
        self,
        message: str,
        *,
        appended: Iterable[EntryId] = (),
    ) -> None
```

Base error for journal-derived projections and host-owned journal operations. `appended` records entries already accepted when a multi-entry host operation failed part-way.

### `omp.journal.EntryTooLarge`

```python
class EntryTooLarge(JournalError):
    def __init__(self, actual: int, limit: int) -> None
```

Raised when a projected entry exceeds an engine-owned size bound.

### `omp.journal.EntryAccessDenied`

```python
class EntryAccessDenied(JournalError):
    def __init__(self, kind: str) -> None
```

Raised when the caller may not read the requested journal projection.

### `omp.journal.JournalIndeterminate`

```python
class JournalIndeterminate(JournalError):
    def __init__(
        self,
        operation: str = "journal mutation",
        *,
        appended: Iterable[EntryId] = (),
    ) -> None
```

Raised when Core cannot prove the durability outcome of a host-owned journal operation.

### `omp.journal.EntryUndecodable`

```python
class EntryUndecodable(JournalError):
    def __init__(self, raw: bytes, reason: str) -> None
```

Raised when bytes are not exactly the canonical JSON encoding accepted by `decode()`.

## DOM operation builders

A patch is a list of operations against integer node handles. On the wire and in the journal each operation is a JSON array. The Rust decoder accepts exactly these four opcodes (`crates/dom/src/op.rs`).

### `omp.journal.insert`

```python
def insert(
    parent: int,
    after: int | None,
    tag: str,
    *,
    props: Mapping[str, object] | None = None,
    content: str | None = None,
) -> list[object]
```

Builds one `ins` operation: `["ins", parent, after, {"tag", "props", "kids", "content"?}]`.

**Raises**: `TypeError` for a non-positive handle, an empty or non-string `tag`, or non-string `content`.

### `omp.journal.remove`

```python
def remove(handle: int) -> list[object]
```

Builds one `rm` operation: `["rm", handle]`.

### `omp.journal.set_prop`

```python
def set_prop(handle: int, prop: str, value: object) -> list[object]
```

Builds one `set` operation: `["set", handle, prop, value]`.

**Raises**: `TypeError` for a non-positive handle or an empty `prop`.

### `omp.journal.move`

```python
def move(handle: int, parent: int, after: int | None = None) -> list[object]
```

Builds one `mv` operation: `["mv", handle, parent, after]`.

### `omp.journal.patch`

```python
def patch(*ops: Sequence[object]) -> dict[str, list[list[object]]]
```

Returns the canonical Component callback result, `{"ops": [...]}`, for the given operations.

```python
result = omp.journal.patch(
    omp.journal.insert(3, None, "note", content="checked"),
    omp.journal.set_prop(9, "status", "done"),
)
```

## Reading projections

### `omp.journal.decode`

```python
def decode(raw: bytes) -> Any
```

Decodes bytes only when they are exactly the canonical JSON encoding written by the host. Canonical form uses UTF-8, sorted keys, compact separators, and finite JSON numbers.

**Returns**: The decoded JSON value.

**Raises**: `TypeError` for non-bytes and `EntryUndecodable` for invalid UTF-8, invalid JSON, non-finite values, or a non-canonical encoding.

### `omp.journal.raw_bytes`

```python
def raw_bytes(row: Mapping[str, object]) -> bytes
```

Reads canonical bytes from an engine projection row, from its `raw` (`bytes` or `str`) or `raw_base64` field.

**Raises**: `TypeError` when the row carries neither field or `raw_base64` is invalid.

## Data model field index

| Dataclass | Fields |
|---|---|
| `EntryId` | `session`, `index` |
| `JournalEntry` | `id`, `kind`, `rev`, `ts`, `principal`, `provenance`, `value`, `raw`, `display`, `in_context`, `artifact=None` |
