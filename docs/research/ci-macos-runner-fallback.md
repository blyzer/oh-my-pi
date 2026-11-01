# Which CI jobs use the self-hosted Mac

The self-hosted Apple silicon runner is registered on **this** repository
(`blyzer/oh-my-pi`), via `vars.MACOS_RUNNER`. It is not the upstream
repository's runner. Same-repo branches and pull requests use it directly.

## Split

| Job | Runner | Why |
|---|---|---|
| `rust` (workspace tests, P1–P8, P12) | `vars.MACOS_RUNNER`, else `macos-15` | Persistent target dir outside the checkout: incremental Rust compile and the embedded CPython link survive between runs |
| `lint_macos` | GitHub-hosted `macos-15` | No reuse of that target dir; a warm clippy was no faster than a cold one |
| Linux jobs (format, lint, P7, P9–P11, `tool_sources`, BuildBuddy) | GitHub-hosted `ubuntu-24.04` | Do not need the Mac |

That keeps the Mac for the one job that benefits from its disk cache, and
runs everything else on GitHub so the Mac stays free.

## What we do not do

There is no availability probe. GitHub's default token cannot list
self-hosted runners, and sending `rust` to hosted when the probe fails throws
away the cache the Mac exists for. If the Mac is offline, that one job waits;
the admin bypass in the ruleset is the escape hatch. Unset `MACOS_RUNNER` to
move `rust` to hosted `macos-15`.

A pull request opened **from another repository** (head repo full name ≠
`github.repository`) is forced to hosted `macos-15` in the workflow file, so
that code does not run on the Mac. `head.repo.fork` is the wrong signal: it
is true when this repository itself was forked from upstream, including for
our own branches.
