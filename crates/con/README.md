# omp-con

`omp-con` is omp's typed command-stream control plane. Variables, commands,
actions, cfg profiles, aliases, key bindings, session replay, and replicated
administration all use one parser and one registry.

## Structural philosophy

- Variables are declared once with `var!`; their type, default, validation,
  completion, and policy flags stay together.
- Effective values are derived from ordered layers: defaults, archived
  `config.cfg` values, the project overlay's values, values a child inherits
  from its parent at spawn, its agent class values (`subagent.cfg`,
  `<agent>.cfg`, the spawner's route), journal-backed session writes, then
  engagement binds from outermost to innermost.
- `reset <var>` removes the variable from the layer its statement commits to
  instead of writing the inherited value back: on the console a child returns
  to its class value, else its parent's, and the main session to the user
  cfg, else the default; in a class cfg it returns to the parent's value; in
  `config.cfg` to the default. A reset `SESSION` override leaves the journal,
  and persistence (`writecfg`) records only a scope's own values, so the line
  drops out.
- A project cfg overlay (`<project>/.omp/<name>.cfg`) is repository content, so
  it runs with project authority instead of the user's: `CfgLoader::load` is
  the user's text, `CfgLoader::load_project` the overlay's, and they are never
  merged. The overlay may only `set` and `reset` convars declared with the
  `project` flag; aliases, binds, `exec`, every other command, and every other
  convar are refused with `ConError::ProjectVarDenied`/`ProjectCommandDenied`
  (reported as warnings, counted in `ExecOutcome::denied`, never fatal).
  Overlay values for `config.cfg` live in the `Origin::Project` layer, which
  `writecfg` never persists; `subagent.cfg`/`<agent>.cfg` overlays commit to
  the class layer under the same restriction.
- `SESSION` writes are projected into `<meta><con><var name value origin>` by
  `omp-session`. Replaying or rewinding the journal therefore reconstructs
  control state without a second settings database.
- Every declared value is copied from the parent's effective view at spawn, then
  `subagent.cfg` and `<agent>.cfg` execute in that order; inheritance is not a flag.
- A context can present a child scope in place of its own (`adopt_scope`, the
  main chat resuming a child session): the child's inherited and class layers
  replace its own, and its session layer is parked, so the main scope's
  writes neither outrank the child's class nor become the child's own; they
  reach the child only through `scope_seed`, its inherited picture.
  `drop_scope` restores the parked layer.
- `REPLICATED` values are authority-owned and locally immutable on replicas.
- Persistence is a replayable command script, not a parallel serialization
  format. `dumpcfg` (`Ctx::dump`) includes only `ARCHIVE` diffs plus aliases and binds.

The built-in names use subsystem prefixes (`ai_*`, `cl_*`, `sv_*`), including
`sv_cheats`, `ai_model`, `ai_fastmode`, and `cl_resize_policy`.

## Layout

| Module | Responsibility |
| --- | --- |
| `value` | Typed values, durations, enums, lists, and kv blocks |
| `spec` | Variable/command/action declarations and flags |
| `ctx` | Registry, command execution, cfg loading, binds, and aliases |
| `layers` | Archive/inherited/class/session/engagement precedence, parked scopes, and child seeds |
| `script` | Quotes, comments, separators, lists, and kv parsing |
| `dump` | Deterministic diff-from-default command script |
| `repl` | Authority-to-replica patches |
| `complete` | Names, enum values, and custom providers |
| `builtins` | Core commands and starter convars |
| `macros` | `var!`, `cmd!`, and `action!` declarations |
