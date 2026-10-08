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

## Philosophy

Transport concerns stay separate from service behavior while local and network clients share the same protocol. Connections negotiate compatibility before exchanging application data so protobuf unknown-field behavior cannot silently discard data from a newer client. Health reporting uses the standard `grpc.health.v1` protocol rather than a project-specific alternative.
