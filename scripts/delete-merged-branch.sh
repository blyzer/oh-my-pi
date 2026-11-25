#!/usr/bin/env bash
# Delete local branches that are finished: each branch's pull request is merged AND the branch still
# points at the very commit that was merged. A branch that moved after its PR, has no merged PR, or
# whose merged commit is not on the remote base is refused and left alone.
#
#   scripts/delete-merged-branch.sh [--dry-run] [--no-fetch] <branch>...
#
# Why a script: the guard hooks refuse `git branch -D` (see .githooks/reference-transaction), so an
# unmerged branch cannot be dropped by accident. This is the one sanctioned way to delete the merged
# ones: it sets ALLOW_BRANCH_DELETE=1 itself, only after the checks above pass.
#
# Checks, per branch:
#   1. not a protected branch (git config omp.protectedBranches, else "main master omp2");
#   2. exists locally and is not checked out in any worktree;
#   3. a MERGED pull request with this head branch recorded exactly the commit the branch points at;
#   4. that commit is an ancestor of origin/<base> (the repository merges with merge commits; a
#      squash or rebase merge leaves the commit off the base, so it is refused: delete it by hand).
#
# Repository for `gh`: $OMP_GH_REPO, else derived from the `origin` URL (a fork checkout can have an
# `upstream` remote that `gh` would otherwise pick). `gh` must be authenticated.
set -euo pipefail

dry=0
fetch=1
branches=()
for arg in "$@"; do
	case "$arg" in
	--dry-run) dry=1 ;;
	--no-fetch) fetch=0 ;;
	-h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
	-*) echo "unknown option: $arg" >&2; exit 2 ;;
	*) branches+=("$arg") ;;
	esac
done
[ "${#branches[@]}" -gt 0 ] || { echo "usage: $0 [--dry-run] [--no-fetch] <branch>..." >&2; exit 2; }

repo=${OMP_GH_REPO:-}
if [ -z "$repo" ]; then
	url=$(git remote get-url origin)
	repo=$(printf '%s' "$url" | sed -E 's#^(git@github\.com:|ssh://git@github\.com/|https://github\.com/)##; s#\.git$##')
fi
protected=${GIT_PROTECTED_BRANCHES:-$(git config --get omp.protectedBranches || true)}
protected=${protected:-main master omp2}

status=0
refuse() {
	echo "refused: $1: $2" >&2
	status=1
}

checked_out=$(git worktree list --porcelain | sed -n 's#^branch refs/heads/##p')

for b in "${branches[@]}"; do
	for p in $protected; do
		if [ "$b" = "$p" ]; then
			refuse "$b" "protected branch"
			continue 2
		fi
	done
	if ! tip=$(git rev-parse --verify --quiet "refs/heads/$b"); then
		refuse "$b" "no such local branch"
		continue
	fi
	if printf '%s\n' "$checked_out" | grep -Fxq -- "$b"; then
		refuse "$b" "checked out in a worktree"
		continue
	fi
	prs=$(gh pr list --repo "$repo" --head "$b" --state merged --limit 20 \
		--json number,headRefOid,baseRefName --jq '.[] | "\(.number) \(.headRefOid) \(.baseRefName)"')
	match=$(printf '%s\n' "$prs" | awk -v tip="$tip" '$2 == tip { print; exit }')
	if [ -z "$match" ]; then
		if [ -n "$prs" ]; then
			refuse "$b" "a merged PR exists but the branch moved after it (branch $tip; merged: $(printf '%s' "$prs" | tr '\n' ';'))"
		else
			refuse "$b" "no merged pull request has this head branch"
		fi
		continue
	fi
	number=${match%% *}
	base=${match##* }
	if [ "$fetch" = 1 ]; then
		git fetch -q origin "$base" || { refuse "$b" "could not fetch origin/$base"; continue; }
	fi
	if ! git merge-base --is-ancestor "$tip" "refs/remotes/origin/$base" 2>/dev/null; then
		refuse "$b" "PR #$number is merged but $tip is not on origin/$base (squash or rebase merge, or a stale remote ref)"
		continue
	fi
	if [ "$dry" = 1 ]; then
		echo "would delete: $b ($tip, merged in #$number into $base)"
	else
		ALLOW_BRANCH_DELETE=1 git branch -D -- "$b" >/dev/null
		echo "deleted: $b ($tip, merged in #$number into $base)"
	fi
done
exit "$status"
