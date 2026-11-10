# remote-approve — remote approval ticket relay (ADR 0049 D5 rescue)

Offers Core-owned `ApprovalTicket`s to a configured HTTP webhook and resolves
pending tickets from remote `allow`/`deny` answers.

Historical source: `git show 00611047c1^:examples/remote-approve/`.

## Surface

- Approver `remote-approve`
- Hooks: `extension_activate`, `turn_start` (poll)
- Settings: `webhook`, `poll_interval`, `unreachable`, `allow_fail_open`
- Pure helpers: `omp_remote_approve.decisions`

## Gaps vs frozen API

- Transport is still GET-shaped via `omp.env.http_get` only (no POST verb on the
  frozen Environment HTTP surface). The webhook contract stays query-parameter
  based so the missing verb is exact rather than silent.
- `Inject` requires `prompt` (filled with `[remote-approve:poll]`).

## How to test

```sh
PYTHONPATH=extensions/remote-approve/src python3 -m unittest discover -s extensions/remote-approve/tests -v
```
