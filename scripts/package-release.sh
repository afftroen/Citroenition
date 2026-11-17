#!/usr/bin/env bash
# Package the release: one tar.gz with the four programs, the licences and the docs, plus SHA256SUMS.
#
#   scripts/package-release.sh <version> <directory with the built programs> <output directory>
#
# The release workflow runs exactly this; run it by hand to see what a release will contain.
set -euo pipefail

version=${1:?usage: package-release.sh <version, e.g. 0.1.0> <bin dir> <out dir>}
bins=${2:?usage: package-release.sh <version> <bin dir> <out dir>}
out=${3:?usage: package-release.sh <version> <bin dir> <out dir>}

root=$(cd "$(dirname "$0")/.." && pwd)
name="citroenition-v${version}-x86_64-linux"
programs=(citro citro-gui citro-hub citro-install)

# the archive's timestamps come from the commit, so packaging the same commit twice gives the same bytes
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct 2>/dev/null || echo 0)}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$out"

for p in "${programs[@]}"; do
    [ -x "$bins/$p" ] || { echo "missing program: $bins/$p" >&2; exit 1; }
    install -m 0755 "$bins/$p" "$stage/$name/$p"
done
cp "$root/LICENSE" "$root/README.md" "$root/CHANGELOG.md" "$root/docs.md" "$stage/$name/"
# citro-gui embeds the Inter font; its licence (SIL OFL 1.1) travels with every copy
cp "$root/crates/citro-gui/assets/fonts/Inter-LICENSE.txt" "$stage/$name/LICENSE-Inter.txt"
cp -r "$root/docs" "$stage/$name/docs"

tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
    -C "$stage" -cf - "$name" | gzip -n -9 > "$out/$name.tar.gz"
(cd "$out" && sha256sum "$name.tar.gz" > SHA256SUMS)

echo "$out/$name.tar.gz"
cat "$out/SHA256SUMS"
