#!/usr/bin/env bash
# Publish the workspace crates to crates.io, one at a time, in dependency order
# (docs/RELEASE.md §3-§4). Stops at the first failure: published crates are
# permanent, so fix the cause and resume with `--from <crate>`.
#
#   publish.sh [--dry-run] [--from <crate>]
#
# Refuses to run unless:
#   * HEAD is the commit tagged v<version>, and the tracked tree is clean;
#   * the web UI assets for <version> are downloadable from the GitHub Release
#     (clove-web downloads them at build time, so a missing asset breaks the
#     published crate for everyone, permanently).
set -euo pipefail

dry_run=0
from=""
while [ $# -gt 0 ]; do
	case "$1" in
	--dry-run) dry_run=1 ;;
	--from) from=${2:?--from needs a crate name}; shift ;;
	*) echo "usage: $0 [--dry-run] [--from <crate>]" >&2; exit 2 ;;
	esac
	shift
done

# The order of docs/RELEASE.md §3. clove-cli goes last, after the plugin crates
# that `clove plugin install` resolves by name.
crates=(
	clove-types clove-core clove-plugin clove-index clove-import clove-ipc clove-tui
	clove-engine clove-mcp clove-web cloved
	clove-sync-github clove-import-tk clove-import-beads
	clove-cli
)

cd "$(git rev-parse --show-toplevel)"
version=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' Cargo.toml | head -1)
[ -n "$version" ] || { echo "error: cannot read the workspace version" >&2; exit 1; }
tag="v$version"

fail() { echo "error: $*" >&2; exit 1; }

git rev-parse -q --verify "refs/tags/$tag" >/dev/null || fail "tag $tag does not exist"
[ "$(git rev-parse HEAD)" = "$(git rev-parse "$tag^{commit}")" ] || fail "HEAD is not the commit tagged $tag"
[ -z "$(git status --porcelain --untracked-files=no)" ] || fail "tracked files have uncommitted changes"

base=${CLOVE_WEB_DIST_BASE_URL:-https://github.com/egeapak/clove/releases/download/$tag}
asset="clove-web-dist-$tag.tar.gz"
for name in "$asset" "$asset.sha256"; do
	code=$(curl -s -L -o /dev/null -w '%{http_code}' --head "$base/$name" || true)
	[ "$code" = 200 ] || fail "$base/$name answers HTTP $code; publish the GitHub Release (with its web UI assets) first"
done
echo "web UI assets for $tag are downloadable"

if [ -n "$from" ]; then
	known=0
	for crate in "${crates[@]}"; do [ "$crate" = "$from" ] && known=1; done
	[ "$known" = 1 ] || fail "--from $from is not one of: ${crates[*]}"
fi

if [ "$dry_run" = 1 ]; then
	# Dependents cannot be dry-run one by one before their dependencies are on the
	# registry; the workspace form verifies them against each other instead.
	exec cargo publish --workspace --dry-run --exclude xtask --exclude clove-plugin-echo
fi

started=0
[ -z "$from" ] && started=1
for crate in "${crates[@]}"; do
	if [ "$started" = 0 ]; then
		if [ "$crate" != "$from" ]; then
			continue
		fi
		started=1
	fi
	echo "== cargo publish -p $crate ($version)"
	cargo publish -p "$crate"
done
echo "done: $version"
