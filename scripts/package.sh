#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

target=""
archive=false
while (($#)); do
  case "$1" in
    --target)
      if (($# < 2)); then printf 'Missing value for --target\n' >&2; exit 2; fi
      case "$2" in
        aarch64-apple-darwin|x86_64-apple-darwin) target="$2" ;;
        *) printf 'Unsupported macOS target: %s\n' "$2" >&2; exit 2 ;;
      esac
      shift 2 ;;
    --archive) archive=true; shift ;;
    *) printf 'Usage: bash scripts/package.sh [--target TARGET] [--archive]\n' >&2; exit 2 ;;
  esac
done

minimum_os="$(plutil -extract LSMinimumSystemVersion raw assets/Info.plist)"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-$minimum_os}"
metadata="$(cargo metadata --locked --no-deps --format-version 1)"
version="$(printf '%s' "$metadata" | plutil -extract packages.0.version raw -)"
target_dir="$(printf '%s' "$metadata" | plutil -extract target_directory raw -)"
version_core="${version%%[-+]*}"
build_number="${IRRESPONSIBLE_BUILD_NUMBER:-$version_core}"
if [[ ! "$build_number" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  printf 'Invalid bundle build number: %s\n' "$build_number" >&2
  exit 2
fi
build_args=(--release --locked)
if [[ -n "$target" ]]; then build_args+=(--target "$target"); fi
cargo build "${build_args[@]}"
# CARGO_BUILD_TARGET also selects Cargo's target subdirectory.
if [[ -z "$target" ]]; then target="${CARGO_BUILD_TARGET:-}"; fi
binary="$target_dir${target:+/$target}/release/irresponsible"
case "$(lipo -archs "$binary")" in
  arm64) arch=arm64 ;;
  x86_64) arch=x86_64 ;;
  *) printf 'Expected one supported macOS architecture\n' >&2; exit 1 ;;
esac

bundle="dist/IRresponsible.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
# Rename a new executable into place so an already running image stays intact.
binary_stage="$(mktemp "$bundle/Contents/MacOS/.irresponsible.XXXXXX")"
cp "$binary" "$binary_stage"
chmod 755 "$binary_stage"
mv -f "$binary_stage" "$bundle/Contents/MacOS/irresponsible"
cp assets/Info.plist "$bundle/Contents/Info.plist"
plutil -insert CFBundleShortVersionString -string "$version_core" "$bundle/Contents/Info.plist"
plutil -insert CFBundleVersion -string "$build_number" "$bundle/Contents/Info.plist"
plutil -insert CFBundleGetInfoString -string "IRresponsible $version" "$bundle/Contents/Info.plist"
cp assets/app-icon/AppIcon.icns "$bundle/Contents/Resources/AppIcon.icns"
mkdir -p "$bundle/Contents/Resources/licenses"
cp assets/fonts/Inter-LICENSE.txt assets/fonts/Iosevka-LICENSE.md assets/Phosphor-LICENSE "$bundle/Contents/Resources/licenses/"
cp assets/SciPy-LICENSE "$bundle/Contents/Resources/licenses/"
cp assets/Gogh-LICENSE "$bundle/Contents/Resources/licenses/"
cp assets/AutoEq-LICENSE "$bundle/Contents/Resources/AutoEq-LICENSE"
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
touch "$bundle"
printf 'Built %s (%s, %s)\n' "$PWD/$bundle" "$version" "$arch"
if "$archive"; then
  output="dist/IRresponsible-${version}-macos-${arch}.zip"
  ditto -c -k --sequesterRsrc --keepParent "$bundle" "$output"
  printf 'Archived %s\n' "$PWD/$output"
fi
