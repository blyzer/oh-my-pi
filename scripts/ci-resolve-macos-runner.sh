#!/usr/bin/env bash
# Resolve which macOS runner a CI job should use (trusted / same-repo runs).
#
# GitHub Actions has no native "prefer self-hosted, else hosted" OR for
# runs-on (a label array is AND). When MACOS_RUNNER names a self-hosted Mac,
# an offline or busy machine leaves required checks pending for up to 24h.
# This script keeps the fast path when that Mac is online and idle, and falls
# back to GitHub-hosted macos-15 otherwise so shipping is not blocked.
#
# Fork pull requests must NOT rely on this script for the hosted-only gate: a
# fork can rewrite this file in the PR. ci.yml forces hosted macos-15 for forks
# in the workflow YAML (macos_runner fork step + rust runs-on expression)
# before this script is consulted.
#
# Prints a JSON value suitable for `runs-on: ${{ fromJSON(...) }}` to stdout.
# Notices go to stderr (and ::notice:: when running in Actions).
#
# Env:
#   PREFERRED   — contents of vars.MACOS_RUNNER (JSON string or array), or empty
#   FALLBACK    — JSON for hosted fallback (default: "macos-15")
#   REPO        — owner/name (default: from gh)
#   GH_TOKEN    — token with actions:read (optional; missing → FALLBACK)
set -euo pipefail

FALLBACK="${FALLBACK:-\"macos-15\"}"
PREFERRED="${PREFERRED:-}"
REPO="${REPO:-}"

notice() {
	echo "::notice::$1" >&2
	echo "$1" >&2
}

emit() {
	printf '%s\n' "$1"
}

if [ -z "$PREFERRED" ] || [ "$PREFERRED" = '""' ]; then
	emit "$FALLBACK"
	exit 0
fi

# Normalize preferred to a JSON array of label strings.
if ! preferred_json=$(printf '%s' "$PREFERRED" | jq -c '
	if type == "string" then [.]
	elif type == "array" then .
	else error("PREFERRED must be a JSON string or array")
	end
' 2>/dev/null); then
	notice "MACOS_RUNNER is not valid JSON ($PREFERRED); using hosted fallback $FALLBACK"
	emit "$FALLBACK"
	exit 0
fi

# Hosted-only preference: nothing to probe.
if [ "$preferred_json" = '["macos-15"]' ] || [ "$preferred_json" = '["macos-14"]' ] || [ "$preferred_json" = '["macos-13"]' ]; then
	emit "$preferred_json" | jq -c 'if length == 1 then .[0] else . end'
	exit 0
fi

if [ -z "${GH_TOKEN:-}" ]; then
	notice "No GH_TOKEN to probe runners; using hosted fallback $FALLBACK"
	emit "$FALLBACK"
	exit 0
fi

if [ -z "$REPO" ]; then
	REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)
fi

# List repo self-hosted runners. Fail open to FALLBACK on any API error so a
# permissions miss never queues required checks forever. --paginate emits one
# page object per chunk; merge .runners across pages before matching labels.
if ! runners_json=$(gh api --paginate "repos/$REPO/actions/runners" 2>/dev/null \
	| jq -c -s '{runners: [.[].runners // [] | .[]]}'); then
	notice "Could not list runners for $REPO; using hosted fallback $FALLBACK"
	emit "$FALLBACK"
	exit 0
fi

# Prefer an online, idle runner that carries every preferred label.
match=$(PREFERRED_JSON="$preferred_json" RUNNERS_JSON="$runners_json" jq -n -c '
	($ENV.PREFERRED_JSON | fromjson) as $want
	| ($ENV.RUNNERS_JSON | fromjson).runners as $runners
	| [
		$runners[]
		| select(.status == "online" and (.busy | not))
		| . as $r
		| ($r.labels | map(.name)) as $have
		| select(all($want[]; . as $l | $have | index($l)))
		| $r.name
	]
	| .[0] // empty
')

if [ -n "$match" ]; then
	notice "Using preferred macOS runner labels $preferred_json (idle runner: $match)"
	emit "$preferred_json"
	exit 0
fi

notice "No online idle runner matches MACOS_RUNNER=$preferred_json; falling back to $FALLBACK"
emit "$FALLBACK"
