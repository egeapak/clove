#!/usr/bin/env bash
# Publish the workspace crates to crates.io, one at a time, in dependency order
# (docs/RELEASE.md §3-§4). Stops at the first failure: published crates are
# permanent, so fix the cause and resume with `--from <crate>`.
#
#   publish.sh --dry-run          rehearse before tagging (needs a built web UI)
#   publish.sh [--from <crate>]   the real publish
#
# A real publish refuses to run unless:
#   * HEAD is the commit tagged v<version>, and the tracked tree is clean;
#   * none of the clove-web build overrides is set (CLOVE_WEB_DIST_DIR,
#     CLOVE_WEB_DIST_BASE_URL, DOCS_RS): `cargo publish -p clove-web` builds the
#     crate to verify it, and an override would skip the very download that every
#     user's build performs;
#   * the web UI assets for <version> download from the GitHub Release, the
#     checksum matches, and the archive holds index.html and _app/. clove-web
#     downloads them at build time, so a bad or missing asset breaks the
#     published crate for everyone, permanently.
set -euo pipefail

dry_run=0
from=""
while [ $# -gt 0 ]; do
	case "$1" in
	--dry-run) dry_run=1 ;;
	--from) from=${2:?--from needs a crate name}; shift ;;
	*) echo "usage: $0 [--dry-run | --from <crate>]" >&2; exit 2 ;;
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

fail() { echo "error: $*" >&2; exit 1; }

[ "$dry_run" = 1 ] && [ -n "$from" ] && fail "--from does not apply to --dry-run"

cd "$(git rev-parse --show-toplevel)"
version=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' Cargo.toml | head -1)
[ -n "$version" ] || fail "cannot read the workspace version"
tag="v$version"

[ -z "$(git status --porcelain --untracked-files=no)" ] || fail "tracked files have uncommitted changes"

if [ "$dry_run" = 1 ]; then
	# Rehearsal, before the tag and the GitHub Release exist: verify every crate
	# against the others, and let clove-web take the built SPA from this checkout
	# instead of downloading it. (Dependents cannot be dry-run one by one before
	# their dependencies are on the registry.)
	dist="$PWD/crates/clove-web/dist"
	[ -f "$dist/index.html" ] && [ -d "$dist/_app" ] \
		|| fail "build the web UI first: (cd crates/clove-web/web && npm ci && npm run build)"
	CLOVE_WEB_DIST_DIR="$dist" exec cargo publish --workspace --dry-run \
		--exclude xtask --exclude clove-plugin-echo
fi

for var in CLOVE_WEB_DIST_DIR CLOVE_WEB_DIST_BASE_URL DOCS_RS; do
	[ -z "${!var:-}" ] || fail "$var is set; unset it so clove-web's verify build downloads the real release assets"
done

git rev-parse -q --verify "refs/tags/$tag" >/dev/null || fail "tag $tag does not exist"
[ "$(git rev-parse HEAD)" = "$(git rev-parse "$tag^{commit}")" ] || fail "HEAD is not the commit tagged $tag"

if [ -n "$from" ]; then
	known=0
	for crate in "${crates[@]}"; do [ "$crate" = "$from" ] && known=1; done
	[ "$known" = 1 ] || fail "--from $from is not one of: ${crates[*]}"
fi

base="https://github.com/egeapak/clove/releases/download/$tag"
asset="clove-web-dist-$tag.tar.gz"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
for name in "$asset" "$asset.sha256"; do
	curl -fsSL -o "$scratch/$name" "$base/$name" \
		|| fail "$base/$name does not download; publish the GitHub Release (with its web UI assets) first"
done
expected=$(awk '{print tolower($1); exit}' "$scratch/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
	actual=$(sha256sum "$scratch/$asset" | awk '{print $1}')
else
	actual=$(shasum -a 256 "$scratch/$asset" | awk '{print $1}')
fi
[ -n "$expected" ] && [ "$expected" = "$actual" ] \
	|| fail "the web UI asset hashes to $actual but its .sha256 says ${expected:-nothing}"
listing=$(tar -tzf "$scratch/$asset")
echo "$listing" | grep -Eq '^(\./)?index\.html$' || fail "the web UI asset has no index.html at its root"
echo "$listing" | grep -Eq '^(\./)?_app/' || fail "the web UI asset has no _app/ (it is the placeholder, not the built SPA)"
echo "web UI assets for $tag download, match their checksum, and hold the built SPA"

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
