#!/usr/bin/env bash
# Pack the built web UI into the release assets a published clove-web downloads
# at build time (crates/clove-web/build.rs):
#   clove-web-dist-v<version>.tar.gz          the contents of dist/, at the archive root
#   clove-web-dist-v<version>.tar.gz.sha256   `<hex digest>  <file name>`
# Part of docs/RELEASE.md §6; release.yml runs it and uploads both files.
#   pack-web-dist.sh <built dist dir> <version> <output dir>
set -euo pipefail

dist=${1:?built dist dir (crates/clove-web/dist)}
version=${2:?version, without the leading v}
out=${3:?output dir}

[ -f "$dist/index.html" ] || { echo "error: $dist has no index.html (build the SPA first)" >&2; exit 1; }
[ -d "$dist/_app" ] || { echo "error: $dist has no _app/ (that is the placeholder, not the built SPA)" >&2; exit 1; }

mkdir -p "$out"
name="clove-web-dist-v${version}.tar.gz"
# No AppleDouble (._*) entries from macOS tar: the extractor accepts plain files,
# so they would be unpacked and embedded in the binary.
COPYFILE_DISABLE=1 tar -czf "$out/$name" -C "$dist" .

cd "$out"
if command -v sha256sum >/dev/null 2>&1; then
	sha256sum "$name" > "$name.sha256"
else
	shasum -a 256 "$name" > "$name.sha256"
fi
echo "packed $out/$name ($(wc -c < "$name" | tr -d ' ') bytes)"
cat "$name.sha256"
