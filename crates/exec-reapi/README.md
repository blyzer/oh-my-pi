# omp-exec-reapi

A Remote Execution API v2 client that implements the `omp-exec` backend
contract (ADR 0040, Slice 1 PR 1b).

## Vendored protocols

The `.proto` sources under `proto/` are the upstream Remote Execution API v2
(`bazelbuild/remote-apis`, Apache-2.0) and the googleapis packages it imports
(`google.api`, `google.rpc`, `google.longrunning`, `google.bytestream`,
Apache-2.0). `proto-buildbuddy/` holds BuildBuddy's `BuildBuddyService` and the
messages it needs, compiled only with the `buildbuddy-cancel` feature. They are
compiled by `build.rs` with `protox` and `tonic-prost-build`, so no system
`protoc` is required.

## Structural philosophy

Generic REAPI is the portable, cold-capable backend. Nothing that names one
service lives in the client: the key header, platform property names and
isolation properties are profile data given by the composing crate. The one
vendor wire behaviour, invocation cancel, is a feature-gated module.
