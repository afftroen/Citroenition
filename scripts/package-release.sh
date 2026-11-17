#!/usr/bin/env bash
# Package the release: one tar.gz per platform with the four programs, the licences and the docs, plus SHA256SUMS.
#
#   scripts/package-release.sh <version> <directory with the built programs> <output directory> [platform]
#
# platform: x86_64-linux (the default on Linux x86-64) or aarch64-macos (the default on an Apple-silicon Mac).
# The macOS archive carries the desktop app as Citroenition.app (ad-hoc signed, so Apple silicon runs it) and a
# `citro-gui` link to the program inside it, next to the three command-line programs.
# The release workflow runs exactly this; run it by hand to see what a release will contain.
set -euo pipefail

usage="usage: package-release.sh <version, e.g. 0.1.1> <bin dir> <out dir> [x86_64-linux|aarch64-macos]"
version=${1:?$usage}
bins=${2:?$usage}
out=${3:?$usage}
platform=${4:-}

if [ -z "$platform" ]; then
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64) platform=x86_64-linux ;;
        Darwin-arm64) platform=aarch64-macos ;;
        *) echo "no release is built for $(uname -s) $(uname -m); give the platform: $usage" >&2; exit 1 ;;
    esac
fi
case "$platform" in x86_64-linux | aarch64-macos) ;; *) echo "unknown platform $platform: $usage" >&2; exit 1 ;; esac

root=$(cd "$(dirname "$0")/.." && pwd)
name="citroenition-v${version}-${platform}"
programs=(citro citro-gui citro-hub citro-install)

# the archive is reproducible: GNU tar with fixed owners, sorted names and the commit's time (macOS ships bsdtar:
# `brew install gnu-tar` gives gtar)
tar=tar
if ! tar --version 2>/dev/null | grep -q 'GNU tar'; then
    command -v gtar > /dev/null 2>&1 || { echo "GNU tar is needed (macOS: brew install gnu-tar)" >&2; exit 1; }
    tar=gtar
fi
sha256() {
    if command -v sha256sum > /dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# the archive's timestamps come from the commit, so packaging the same commit twice gives the same bytes
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct 2>/dev/null || echo 0)}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$out"

for p in "${programs[@]}"; do
    [ -x "$bins/$p" ] || { echo "missing program: $bins/$p" >&2; exit 1; }
    # on a Mac the desktop app is bundled below
    if [ "$platform" = aarch64-macos ] && [ "$p" = citro-gui ]; then continue; fi
    install -m 0755 "$bins/$p" "$stage/$name/$p"
done
cp "$root/LICENSE" "$root/README.md" "$root/CHANGELOG.md" "$root/docs.md" "$stage/$name/"
# citro-gui embeds the Inter font; its licence (SIL OFL 1.1) travels with every copy
cp "$root/crates/citro-gui/assets/fonts/Inter-LICENSE.txt" "$stage/$name/LICENSE-Inter.txt"
cp -r "$root/docs" "$stage/$name/docs"

if [ "$platform" = aarch64-macos ]; then
    app="$stage/$name/Citroenition.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    install -m 0755 "$bins/citro-gui" "$app/Contents/MacOS/citro-gui"
    cp "$root/crates/citro-gui/assets/fonts/Inter-LICENSE.txt" "$app/Contents/Resources/LICENSE-Inter.txt"
    cat > "$app/Contents/Info.plist" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>Citroenition</string>
    <key>CFBundleDisplayName</key><string>Citroenition</string>
    <key>CFBundleIdentifier</key><string>io.github.afftroen.citroenition</string>
    <key>CFBundleExecutable</key><string>citro-gui</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>${version}</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
    ln -s Citroenition.app/Contents/MacOS/citro-gui "$stage/$name/citro-gui"
    # Apple silicon only runs signed code; an ad-hoc signature (no developer identity) is enough to start it
    if command -v codesign > /dev/null 2>&1; then
        codesign --force --deep --sign - "$app"
    else
        echo "warning: no codesign here, the app is not signed" >&2
    fi
fi

"$tar" --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
    -C "$stage" -cf - "$name" | gzip -n -9 > "$out/$name.tar.gz"
(cd "$out" && sha256 "$name.tar.gz" > SHA256SUMS)

echo "$out/$name.tar.gz"
cat "$out/SHA256SUMS"
