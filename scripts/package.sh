#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo build --release --locked
bundle="dist/IRresponsible.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
# Rename a new executable into place so an already running image stays intact.
binary_stage="$(mktemp "$bundle/Contents/MacOS/.irresponsible.XXXXXX")"
cp target/release/irresponsible "$binary_stage"
chmod 755 "$binary_stage"
mv -f "$binary_stage" "$bundle/Contents/MacOS/irresponsible"
cp assets/Info.plist "$bundle/Contents/Info.plist"
cp assets/app-icon/AppIcon.icns "$bundle/Contents/Resources/AppIcon.icns"
mkdir -p "$bundle/Contents/Resources/licenses"
cp assets/fonts/Inter-LICENSE.txt assets/fonts/Iosevka-LICENSE.md assets/Phosphor-LICENSE "$bundle/Contents/Resources/licenses/"
cp assets/Gogh-LICENSE "$bundle/Contents/Resources/licenses/"
cp tests/fixtures/AutoEq-LICENSE "$bundle/Contents/Resources/AutoEq-LICENSE"
codesign --force --sign - "$bundle"
touch "$bundle"
printf 'Built %s\n' "$PWD/$bundle"
