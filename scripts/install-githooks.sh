#!/usr/bin/env bash
# Install the repository's guard hooks (.githooks/commit-msg, .githooks/reference-transaction).
#
# The hooks are COPIED into the directory git reads hooks from (`git rev-parse --git-path hooks`:
# the shared .git/hooks, or core.hooksPath when set), so every worktree gets them and other hooks
# there (for example the git-lfs ones) keep working. Setting core.hooksPath to .githooks would
# switch those off, which is why this does not do it.
#
# Re-run it after pulling a change to .githooks to update the copies. It refuses to overwrite a
# hook that is not one of ours.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
dest=$(git -C "$root" rev-parse --git-path hooks)
case "$dest" in /*) ;; *) dest="$root/$dest" ;; esac
mkdir -p "$dest"

status=0
for name in commit-msg reference-transaction; do
	src="$root/.githooks/$name"
	target="$dest/$name"
	if [ -e "$target" ] || [ -L "$target" ]; then
		if cmp -s "$src" "$target"; then
			echo "up to date: $target"
			continue
		fi
		if ! grep -q '^# omp-githooks:' "$target" 2>/dev/null; then
			echo "refusing to overwrite $target: it is not an omp-githooks hook." >&2
			echo "  Chain .githooks/$name from it, or move it away, then run this again." >&2
			status=1
			continue
		fi
	fi
	cp "$src" "$target"
	chmod +x "$target"
	echo "installed: $target"
done
exit "$status"
