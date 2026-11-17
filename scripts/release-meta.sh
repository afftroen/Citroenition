#!/usr/bin/env bash
# What the release workflow needs to know before it builds anything:
#   - the version (Cargo.toml), which must match the tag when the run is for a tag;
#   - the release notes: the CHANGELOG.md section of that version, written to notes.md in the repository root.
# Appends VERSION=<x> to $GITHUB_ENV when that is set (the workflow's later steps read it).
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
if [ -n "${GITHUB_ENV:-}" ]; then
    echo "VERSION=$version" >> "$GITHUB_ENV"
fi
if [ "${GITHUB_REF_TYPE:-}" = tag ] && [ "${GITHUB_REF_NAME:-}" != "v$version" ]; then
    echo "the tag is $GITHUB_REF_NAME but Cargo.toml says v$version" >&2
    exit 1
fi

awk -v v="$version" '/^## /{ on = (index($0, "## " v) == 1) } on' CHANGELOG.md > notes.md
test -s notes.md || { echo "CHANGELOG.md has no section for $version" >&2; exit 1; }
cat notes.md
