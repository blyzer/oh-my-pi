# omp-rpc

`omp-rpc` provides transport and protocol-negotiation plumbing for omp's gRPC services and the framed `omp rpc` embedding protocol. It supports owner-only local Unix-domain sockets, TCP connections secured with mutual TLS, Content-Length framed stdio, and a typed child-process client.

## Structure

- `client` drives the stdio embedding protocol, including host tools, generation-fenced host URI resources, typed authentication exchanges, cancellation, and fail-closed shutdown.
- `framing` bounds, encodes, and incrementally reassembles v1/v2 Content-Length frames.
- `protocol` owns typed stdio requests, events, host-resource frames, and terminal outcomes.
- `health` wraps gRPC liveness and per-service readiness reporting.
- `hello` implements the initial peer handshake and schema-revision compatibility checks.
- `tls` builds client and server TLS configuration.
- `uds` listens for and connects to Unix-domain socket transports.
- The crate-level `Error` type unifies I/O, transport, RPC, TLS, schema-negotiation, and unsupported-transport failures.

## Host tools

`set_host_tools` replaces the embedding host's tool roster between turns. Each
`HostToolDefinition` names the tool (`name`, optional `label`), describes it for the model
(`description`, JSON Schema `parameters`), may hide it from normal rosters (`hidden`), and may
declare its maximum effects (`effects`, a `HostToolEffects`):

```json
{
  "name": "list_tickets",
  "description": "List open tickets",
  "parameters": {"type": "object"},
  "effects": {
    "documents": {"read": true, "writeGlobs": []},
    "exec": {"commands": ["git"], "network": false},
    "inference": {"maxRequests": 1, "maxUsd": "0.25"},
    "desktop": {"capture": false, "accessibility": false, "input": false},
    "fetch": {"credentials": false},
    "subagents": 0
  }
}
```

Every domain is optional and an absent one is denied; unknown fields are refused. The envelope
sets the tool's approval tier: reads are `read`, read-only egress (`fetch`) is `fetch`, document
writes are `write`, and commands, network, inference, desktop input and subagents are `exec`.
A tool without `effects` is undeclared and is registered with the unknown ceiling (any command,
with the network), so it is `exec` tier: every approval mode except an explicit `yolo` prompts
before each call. Declare `"effects": {}` for a tool with no effects.
Host tools always run with the host's authority; an active sandbox never confines them.

## History, session and event-filter commands

These follow the v1 (`pi`) RPC commands of the same names. Where omp's journal differs from
v1's session file, the payload says so instead of imitating v1.

- `get_entries` (`since?`) returns `{ entries, leafId }`: every journal entry in append order,
  across all branches, as `{ type, rev, id, parentId, causeId?, label?, data }`. `type` is the
  journal kind (`msg.user`, `stream`, `tool.call`, ...), `parentId` the branch parent, `data` the
  entry's JSON payload, and `leafId` the selected head. With `since`, only the entries strictly
  after that id; an id the journal does not hold fails with `code: "unknown_since"`. While a turn
  runs it fails with `session_busy`.
- `get_tree` returns `{ tree, leafId }`: the message tree (user messages, assistant message
  starts and compactions) over every branch, as a **flat** list in append order of
  `{ entry, children }`. `entry.parentId` is the nearest message ancestor (null for a root) and
  `children` lists the ids of the message children. v1 nests the nodes instead; a nested tree is
  as deep as the conversation, and serializing nested JSON recurses per level (a 1 000-level tree
  overflowed a 2 MiB stack when measured), so hosts rebuild nesting from `children`.
- `get_available_thinking_levels` returns `{ levels }`: `off`, then the efforts the active model's
  catalog reasoning policy supports, least to most intensive. A model without a reasoning
  policy offers only `off`.
- `open_session` (`sessionDir`, optional `provider` + `modelId`) binds the process to a
  host-keyed directory, relative to the project when not absolute: it resumes the newest journal
  there, or starts a fresh one when there is none, and returns
  `{ cancelled, resumed, sessionId, sessionFile }`. When the newest journal is already active
  nothing switches. `provider` and `modelId` go together, are checked against the catalog before
  anything switches (`model_not_found`), and select the model as `set_model` does. Unlike v1, a
  resumed journal does not restore a saved model.
- `set_event_filter` (`events: string[] | null`, `messageUpdates?: "full" | "delta"`) replaces
  the whole filter and echoes `{ events, messageUpdates }`. `events` lists the session event
  types to forward (`null` forwards all); `"delta"` narrows each `message_update` to
  `message: { role }` and drops `assistantMessageEvent.partial`. It applies to the session event
  stream (message, tool and kernel events, `turn_end`, `agent_end`); responses, requests,
  `available_commands_update`, `session_start` and subagent frames are never filtered. An
  invalid request fails with `invalid_params` and changes neither setting.

## Philosophy

Transport concerns stay separate from service behavior while local and network clients share the same protocol. Connections negotiate compatibility before exchanging application data so protobuf unknown-field behavior cannot silently discard data from a newer client. Health reporting uses the standard `grpc.health.v1` protocol rather than a project-specific alternative.
