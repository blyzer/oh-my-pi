# Reference repositories: extension management and ABI-independent mechanisms

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverable 7. Reference repositories are untrusted input: no code was run, no dependency installed, no instruction in a README or comment was followed, and nothing was copied. Licences come from [`ref-inventory.md`](ref-inventory.md): `Ve-ria/quickx` is GPL-3.0 and `Ve-ria/CLIProxyAPIPlus` is AGPL-3.0 (ideas studied only). `AmethystDev-Labs/QuickCLI` is unavailable (404 on 2026-11-28); it was not read and no fallback documentation was found in this pass.

Matrix rows use the X dimension IDs of [`01-method-and-dimension-ids.md`](01-method-and-dimension-ids.md): X01 plugin mechanism, X02 capability manifest, X03 registry and distribution, X04 integrity and signatures, X05 permissions and isolation, X06 lazy startup and independent termination, X07 health reporting, X08 upgrade and rollback, X09 platform-specific executable distribution, X10 dependency declarations, X11 compatibility declarations. TSV files: `matrix-<owner>_<repo>.tsv` in this directory.

Citation form: `owner/repo@sha7:path:symbol`.

## 1. router-for-me/CLIProxyAPI @ dbe641f (MIT)

Read: code (Go) for the plugin host, store and ABI types, plus the plugin examples README and `config.example.yaml`.

**Mechanism (X01).** A plugin is a native dynamic library (`c-shared`) loaded in the host process. The host calls `dlopen(path, RTLD_NOW | RTLD_LOCAL)`, resolves the single symbol `cliproxy_plugin_init`, and exchanges two C structs: `cliproxy_host_api` (host callbacks) and `cliproxy_plugin_api` (`call(method, request_bytes, len, out_buffer)`, `free_buffer`, `shutdown`), each carrying `abi_version` (`router-for-me/CLIProxyAPI@dbe641f:internal/pluginhost/loader_unix.go:22-39, 44-49, 194-214`). The calls carry serialized JSON, so the ABI is a thin C shell around a message protocol: "native dynamic library plugins communicate across the C ABI via serialized JSON buffers" (`examples/plugin/README.md:131`). Methods are named strings such as `plugin.register`, `plugin.quiesce`, `plugin.reconfigure`, `plugin.shutdown`, `model.register`, `auth.login.start`, `scheduler.pick`, `model.route`, `executor.execute`, `executor.execute_stream`, `request.translate`, `response.intercept_stream_chunk`, `thinking.apply`, `usage.handle`, `quota.fetch`, `management.register`, `host.http.do` (`sdk/pluginabi/types.go:79-144`). `ABIVersion uint32 = 1` (`sdk/pluginabi/types.go:10-11`). Examples exist in Go, C and Rust (`examples/plugin/README.md:30`).

**Costs of the in-process C ABI, as stated by the code.**
- A Go `c-shared` runtime cannot be unloaded: after `dlopen`, "the image must never be unloaded", failure paths "deliberately leak the mapped image" (`internal/pluginhost/loader_unix.go:188-192`). Independent termination is therefore not available.
- The host re-executes its own binary with `CLIPROXY_PLUGIN_DLOPEN_PROBE` to probe whether a library loads, because a bad library can crash the process (`loader_unix.go:133-160`; `internal/pluginhost/shadow.go`, 616 lines, copies the library to a shadow path before loading).
- Prebuilt artifacts must pin the CPU baseline so instructions compiled on a CI runner do not fault on other machines (`examples/plugin/README.md:175-178`).
- Error semantics must be encoded inside the JSON envelope (`http_status`) because only bytes cross (`README.md:118-131`).

**Capability surface (X02).** Capabilities are declared at runtime by the plugin (`plugin.register`), not in a static signed manifest. A store manifest carries metadata only (id, name, author, version, release tag, repository, licence, tags, locales, install plan; `internal/pluginstore/manifest.go:9-27`). This is a runtime-asserted surface, in contrast to OMP2's static declaration table.

**Registry and distribution (X03, X09).** JSON registry `{schema_version, plugins[]}`; each plugin has `versions[]` and an `install` plan of per-`goos`/`goarch` artifacts `{url, sha256, size}` (`internal/pluginstore/registry.go:36-83`). Default registry is a raw GitHub URL for `router-for-me/CLIProxyAPI-Plugins-Store` (`registry.go:17`); extra `store-sources` and per-URL store authentication by environment variable are configurable (`config.example.yaml:1415-1445`). Install types: direct artifacts and GitHub release (`manifest.go:46-61`).

**Integrity (X04).** SHA-256 only: `VerifyChecksum` and `VerifyArtifactChecksum` (`internal/pluginstore/checksum.go:34`, `direct.go:45`, `install.go:133, 169`). A case-insensitive search of `internal/pluginstore` and `internal/pluginhost` for `ed25519|cosign|minisign|gpg|signature|verify` found only checksum helpers. No publisher signature, key pinning or revocation was found.

**Permissions and isolation (X05).** None beyond an enable flag: `plugins.enabled: false` by default and per-plugin `enabled` and `priority` (`config.example.yaml:1415-1416, 1450-1455`). The plugin runs with the host's privileges in the host address space.

**Upgrade (X08).** Atomic replace of the library file (`install.go:299, 519-566`); the registry may list several versions. No rollback or generation retention was found (`rg` for rollback, backup, previous in `pluginstore`).

**Dependencies and compatibility (X10, X11).** No dependency field in `Plugin` or `Manifest` (structs read). Compatibility is the integer `abi_version` checked by the host and `schema_version` of the registry.

**Health (X07).** Not examined in this pass.

**Lesson for OMP2.** The useful parts are the registry shape (per-platform artifact table), store authentication by environment-variable reference (the secret never enters a manifest), the stable method-name vocabulary over a message payload, and the `quiesce`/`reconfigure` lifecycle verbs. The costs (no unload, probe-by-reexec, CPU-baseline pinning, no signatures, in-process privileges) are the reasons the owner excluded the C ABI route. OMP2 already has the safer variants of each useful part (section 5 of `omp2-extensions.md`).

## 2. Ve-ria/CLIProxyAPIPlus @ 60bb379 (AGPL-3.0; ideas only, nothing copied)

Read: directory structure, a name search for `plugin`, `examples/custom-provider/main.go`, and `sdk/access/registry.go`. Head date 2026-05-09 (older than the `router-for-me/CLIProxyAPI` head, which has the dynamic-library host).

- **No dynamic plugin host.** `internal/` has no `pluginhost` or `pluginstore` directory (`ls internal`); a case-insensitive search for `plugin` in `*.go`, `*.md`, `*.yaml` finds an in-process usage-observer interface (`Ve-ria/CLIProxyAPIPlus@60bb379:internal/usage/logger_plugin.go:22 coreusage.RegisterPlugin`), a MIME file and provider-specific authentication code, not a loader. X01 is therefore compile-time Go registration.
- **Static extension through the Go SDK.** A custom provider is a Go type implementing an `Executor` interface (`Identifier`, `PrepareRequest`, `Execute`, `ExecuteStream`, `CountTokens`, `Refresh`, `HttpRequest`) linked into the embedding program (`Ve-ria/CLIProxyAPIPlus@60bb379:examples/custom-provider/main.go:65-171`); registries such as `sdk/access/registry.go:30 RegisterProvider` are process-global maps filled at start-up. This is the "statically compiled provider crate" option expressed in Go.
- **Declarative providers.** Configuration-declared OpenAI-compatible endpoints (`config.example.yaml:269 openai-compatibility`, commented example), the same data-only route OMP2 uses for providers-as-data.
- **Everything else (registry, signatures, permissions, health, rollback, dependencies, compatibility):** not present in the code read; marked `N` where a search covered it and `U` where it did not.
- **Lesson.** The "Plus" fork stayed on static registration plus config; the dynamic-library host appears only in the upstream repository at a later head. This supports the position that a gateway can grow a large provider set without a plugin ABI.

