# omp-ext

Extension configuration, dependency resolution, lockfiles, index metadata, and
local trust state for OMP.

## Structural philosophy

The crate is a pure domain: deterministic data transformations over declared
configuration and durable on-disk state. It owns no process, no connection, and
no CLI. Everything that needs a running Environment — Git materialization,
site publication — lives in the host or the CLI driver above it, so this crate
sits below both and can be reasoned about as data in, data out.

- `config`: declared extension configuration, overlays, scopes, and CLI
  contributions.
- `lock`: reproducible lockfiles and local installed/enabled records.
- `resolver`: the `uv` resolution driver and R1-R12 policy checks.
- `trust`: signature verification, trust tiers, and the local grant file,
  including operator approvals of plugin-launched commands.
- `plugin_command`: the approval key (`Hash32` digest of plugin version,
  command, arguments, and environment) and typed refusal for every process an
  installed plugin's MCP, LSP, or DAP declaration would start; plugin
  resolution attaches approvals and each launching seam gates through them.
- `index`, `upgrade`, `doctor`: index metadata, generation commits, and
  integrity diagnostics.
- `marketplace`, `claude_plugin`: Claude-compatible marketplace catalogs, the
  `installed_plugins.json` registry `omp ext install` writes, and the
  resolution of enabled installs into contained plugin roots whose skills,
  commands, rules, MCP, LSP/DAP servers, and hooks the driver and Environment
  discovery load. Claude Code's own registry merges in read-only
  (`ClaudeCodeHome`); omp's registries win for the same id.
- `claude_hooks`: Claude Code hook declarations (`hooks/hooks.json`, manifest
  `hooks`) parsed into typed hooks, plus the data tables mapping Claude events
  onto omp hook seams and Claude tool names onto omp tool families.
