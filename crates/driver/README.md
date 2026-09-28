# omp-driver

`omp-driver` is OMP's journal-first coding-agent composition boundary. It
assembles `omp-agent`, `omp-session`, inference, project-environment tools,
convars, subagent spawning, and live-session routing without depending on CLI,
TUI, or desktop presentation crates.

The crate sits above `omp-envd`, `omp-env`, and `omp-serve` and below
`omp-app`. Driver code composes the session; app code selects a command or
presentation adapter.

## Structure

- `headless::kernel` constructs the production kernel, `.oms` session,
  inference route, environment authority, and session-owned `task`/`hub`
  tools reused by chat, print, RPC, and ACP.
- `plugin_hooks` runs installed plugins' Claude-format hooks as one
  in-process host on the kernel's generic hook gate, each command in the
  environment's in-process shell.
- `sessions` is the disposable process-local routing index for live kernel
  mailboxes and detached DOM snapshots.
- `subagent` seeds child convars and composes child kernels through the same
  headless path.
- `registry` assembles catalog, credential, inference, and service
  authorities.
- `discovery` retains credential-blind model configuration and role selection,
  plus the prompt material the kernel journals as `prompt-facts`: `skills`
  (`SKILL.md`, `skill://`), `rules` (context files such as `AGENTS.md` /
  `CLAUDE.md` walked up from the project, and `.omp/rules` / `RULES.md` /
  `.cursor/rules` / `.clinerules` rule documents served as `rule://`), and
  `prompts` (Markdown prompt templates that become `/name` slash commands with
  `$1` / `$ARGUMENTS` substitution). `--no-context-files`, `--no-rules`,
  `--no-prompt-templates`, and `--prompt-template <path>` are their seams.
- A rule's `agents:` frontmatter scopes it to agent classes (`main` for the
  top-level session, the spawned class such as `task` or `scout` for a
  subagent): case-insensitive globs, a list or a comma-separated string. A
  `!` prefix negates an entry — positives define the admitted set (every
  agent when there are none) and negations subtract from it, so
  `agents: [!reviewer]` reaches every agent but `reviewer` and
  `agents: [review-*, !review-bot]` every `review-*` class but `review-bot`.
  An excluded rule is kept out of the prompt, out of `rule://` listings and
  completion, and `rule://<name>` refuses it. A child session journals the
  class it runs as, so resuming it (from the main chat's `/resume` or with
  `--resume`) keeps the child's rules.
- `v1_import` is the one-shot v1 (TypeScript `omp`) migrator behind
  `omp config import-v1` and the automatic first run. Its `sessions` step and
  the `/resume @v1` picker convert v1 transcripts into `.oms` journals. The
  imported journal is the only record of an import: its `<meta>` provenance
  (`import-format omp1`, `import-source-id`, `import-source`) is written before
  any transcript entry, and `ImportedIndex` derives from it which v1 sessions
  already have a journal, so the picker marks those rows (picking one reopens
  the journal) and a bulk re-run reports them skipped instead of converting
  again; deleting the journal makes the session importable again. v1's
  `artifact://<N>` URIs stay verbatim in journaled text: the importer copies
  each file of the session's artifact directory into the bucket's project blob
  store and journals its v1 id and `sha256` digest as
  `<meta><foreign-artifact>`, and `omp-envd`'s `artifact://` resolver maps a
  numeric id through that mapping for the session reading it. A referenced id
  with no v1 file is reported, not fatal.

`omp-driver` may construct `omp_envd::ProjectEnvironment` and supply the
higher-layer bridges it needs, but the filesystem/process/document/tool host
and Python extension-host/worker implementation remain in `omp-envd`.
Environment requests use `omp-env` clients. Neither boundary is reimplemented
in the driver.

## Philosophy

There is one headless production composition that every presentation reuses.
CLI parsing, terminal interaction, display policy, and presentation-protocol
adaptation stay in `omp-app`; reusable session state and authority wiring stay
here. This keeps print, RPC, ACP, and TUI modes from growing separate agent
stacks and
prevents presentation code from acquiring environment-host internals.

## Development

Run `just setup-python` once before commands that link embedded Python. Use
`just check-pkg omp-driver` and `just test-pkg omp-driver`. For joined session
behavior, use `just e2e` or an exact narrower E2E recipe from `just --list`.
Local model engines are opt-in through `local-all` or the individual
`local-*` features.
