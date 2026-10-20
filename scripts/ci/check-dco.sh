#!/usr/bin/env bash
# Developer Certificate of Origin check (CONTRIBUTING.md): every commit in the
# range must carry a `Signed-off-by:` trailer whose email matches the commit's
# author email (what `git commit -s` writes). Merge commits are skipped.
#   check-dco.sh <base-ref> <head-ref>
set -euo pipefail

base=${1:?base ref (the PR base commit)}
head=${2:?head ref (the PR head commit)}

missing=0
count=0
for sha in $(git rev-list --no-merges "$base..$head"); do
	count=$((count + 1))
	author=$(git show -s --format='%ae' "$sha")
	signoffs=$(git show -s --format='%(trailers:key=Signed-off-by,valueonly)' "$sha")
	if ! printf '%s\n' "$signoffs" | grep -qiF "<$author>"; then
		echo "::error::commit $sha ($(git show -s --format='%s' "$sha")) has no Signed-off-by for its author <$author>"
		missing=$((missing + 1))
	fi
done

if [ "$missing" -ne 0 ]; then
	echo "$missing of $count commit(s) lack a DCO sign-off. Add one with:" >&2
	echo "  git rebase --signoff $base" >&2
	echo "then force-push your branch. See CONTRIBUTING.md." >&2
	exit 1
fi
echo "ok: all $count commit(s) are signed off"
