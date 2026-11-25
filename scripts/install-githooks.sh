#!/usr/bin/env bash
# Install the repository's guard hooks from .githooks (commit-msg, pre-commit, pre-push,
# reference-transaction).
#
# The hooks are COPIED into the directory git reads hooks from (`git rev-parse --git-path hooks`:
# the shared .git/hooks, or core.hooksPath when set), so every worktree gets them. Setting
# core.hooksPath to .githooks would switch off the other hooks there, which is why this does not.
#
# A hook of the same name that is not ours (for example the git-lfs pre-push) is NOT overwritten:
# for commit-msg, pre-commit and pre-push it is renamed to <name>.chained, and our hook runs it
# after its own check with the same arguments and input. reference-transaction has no chaining, so
# a foreign one stops the install for that hook. Re-run after pulling a change to .githooks to
# update the copies.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
dest=$(git -C "$root" rev-parse --git-path hooks)
case "$dest" in /*) ;; *) dest="$root/$dest" ;; esac
mkdir -p "$dest"

status=0
for name in commit-msg pre-commit pre-push reference-transaction; do
	src="$root/.githooks/$name"
	target="$dest/$name"
	if [ -e "$target" ] || [ -L "$target" ]; then
		if cmp -s "$src" "$target"; then
			echo "up to date: $target"
			continue
		fi
		if ! grep -q '^# omp-githooks:' "$target" 2>/dev/null; then
			if [ "$name" = reference-transaction ]; then
				echo "refusing to overwrite $target: it is not an omp-githooks hook and this one cannot chain." >&2
				status=1
				continue
			fi
			if [ -e "$target.chained" ]; then
				echo "refusing to overwrite $target: $target.chained already exists." >&2
				status=1
				continue
			fi
			mv "$target" "$target.chained"
			echo "kept your existing hook as $target.chained (it runs after ours)"
		fi
	fi
	cp "$src" "$target"
	chmod +x "$target"
	echo "installed: $target"
done
exit "$status"
