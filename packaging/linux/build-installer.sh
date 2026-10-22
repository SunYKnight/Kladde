#!/bin/sh
# Builds the self-extracting Linux installer: header script followed by a tar.gz payload.
#   packaging/linux/build-installer.sh <kladde binary> <version> <output .sh>
set -eu

binary=$1
version=$2
output=$3
here=$(dirname "$0")
icon="$here/../../crates/kladde/assets/kladde.png"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$binary" "$stage/kladde"
cp "$icon" "$stage/kladde.png"

sed "s/@VERSION@/$version/g" "$here/installer-header.sh" > "$output"
tar -czf - -C "$stage" kladde kladde.png >> "$output"
chmod 755 "$output"
