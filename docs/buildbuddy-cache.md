# Remote Bazel cache via BuildBuddy

The `.bazelrc` shipped here points at `bazel-remote.bazel-cache.svc.cluster.local`
— an address that only resolves inside upstream's cluster. A fork inherits the
configuration and none of the reachability, so every build starts cold: on
GitHub-hosted runners the native addon job compiles for roughly an hour.

BuildBuddy replaces that with an endpoint any machine can reach. Nothing here
is required: without a key, CI and local builds keep the disk cache and behave
exactly as before.

## CI

Set one repository secret:

```
BB_API_KEY = <org API key>
```

Optionally set the `BB_REMOTE_HOST` repository *variable* to point at a
self-hosted or regional endpoint; it defaults to `remote.buildbuddy.io`.

`bazel-cache` picks the backend on its own:

| condition | backend |
|---|---|
| `BB_API_KEY` set | BuildBuddy — remote cache + build event stream |
| `BAZEL_REMOTE_USER` **and** `BAZEL_REMOTE_PASSWORD` set | upstream's in-cluster `bazel-remote` |
| neither | disk cache, pruned per platform |

BuildBuddy wins when both are present: an API key is an explicit choice, while
the user/password pair may be inherited from upstream's configuration and
points somewhere a fork cannot reach.

The key is masked before the rc file is written. `--config=ci` enables
`--announce_rc`, which prints every flag — including headers — into the log.

## Local

`.bazelrc.user` is already `try-import`ed and gitignored. Create it with:

```
common --config=cache-rw
common --remote_cache=grpcs://remote.buildbuddy.io
common --remote_header=x-buildbuddy-api-key=<your key>
common --remote_download_toplevel
```

`--remote_download_toplevel` matters on a laptop: without it Bazel downloads
every intermediate output of every cached action, and on a large Rust graph
that is slower than rebuilding.

Do not commit this file. It holds a credential, and `.gitignore` already
covers it — but a `git add -f` would sail past that.

## Verifying it works

A cache hit shows in the build summary:

```
INFO: 3285 processes: 3100 remote cache hit, 185 linux-sandbox.
```

`remote cache hit` is the number that matters. If every process still says
`linux-sandbox`, the cache is reachable but empty — expected on the first
build, since something has to populate it.

With `--bes_backend` configured, CI runs also appear at
`app.buildbuddy.io/invocation/<id>` with per-target timings. That is the
fastest way to find which target dominates a build, which the raw log does not
answer.

## What this does not fix

The GitHub Actions cache quota is a separate limit: 10 GB per repository,
storage rather than runner minutes. BuildBuddy takes the *action* cache off
that budget, but the disk-cache archives saved by `actions/cache` still count
against it. Those are pruned per platform on save; see
`.github/actions/bazel-natives/action.yml`.
