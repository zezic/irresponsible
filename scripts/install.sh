#!/usr/bin/env bash
set -euo pipefail

install_dir="$HOME/Applications"
while (($#)); do
  case "$1" in
    --directory)
      if (($# < 2)); then printf 'Missing installation directory\n' >&2; exit 2; fi
      install_dir="$2"; shift 2 ;;
    --help)
      printf 'Usage: install.sh [--directory PATH]\nDefault: ~/Applications\n'
      exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
[[ "$(uname -s)" == Darwin ]] || { printf 'IRresponsible requires macOS.\n' >&2; exit 1; }
case "$(uname -m)" in
  arm64) arch=arm64 ;;
  x86_64) arch=x86_64 ;;
  *) printf 'Unsupported Mac architecture.\n' >&2; exit 1 ;;
esac
if pgrep -x irresponsible >/dev/null; then
  printf 'Quit IRresponsible before installing an update.\n' >&2
  exit 1
fi
mkdir -p "$install_dir"
install_dir="$(cd "$install_dir" && pwd -P)"
destination="$install_dir/IRresponsible.app"
stage="$(mktemp -d "$install_dir/.irresponsible.XXXXXX")"
cleanup() {
  if [[ ! -e "$destination" && -d "$stage/previous.app" ]]; then
    mv "$stage/previous.app" "$destination"
  fi
  rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
fetch() {
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 --fail --silent --show-error --location --retry 3 --output "$2" "$1"
}
repo="zezic/irresponsible"
fetch "https://api.github.com/repos/$repo/releases/latest" "$stage/release.json"
tag="$(plutil -extract tag_name raw "$stage/release.json")"
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+[A-Za-z0-9.+-]*$ ]]; then
  printf 'Unexpected release tag: %s\n' "$tag" >&2
  exit 1
fi
version="${tag#v}"
archive="IRresponsible-${version}-macos-${arch}.zip"
url="https://github.com/$repo/releases/download/$tag"
fetch "$url/$archive" "$stage/$archive"
fetch "$url/SHA256SUMS" "$stage/SHA256SUMS"
(
  cd "$stage"
  awk -v file="$archive" '$2 == file { print; found++ } END { if (found != 1) exit 1 }' SHA256SUMS > selected.sha256
  shasum -a 256 -c selected.sha256
)
ditto -x -k "$stage/$archive" "$stage/unpacked"
app="$stage/unpacked/IRresponsible.app"
info="$app/Contents/Info.plist"
if [[ "$(plutil -extract CFBundleIdentifier raw "$info")" != com.zezic.irresponsible ||
      "$(plutil -extract CFBundleGetInfoString raw "$info")" != "IRresponsible $version" ]]; then
  printf 'Downloaded app identity or version does not match this release.\n' >&2
  exit 1
fi
codesign --verify --deep --strict "$app"
# The release is ad-hoc signed; clear quarantine only on this verified app.
xattr -dr com.apple.quarantine "$app"
if [[ -e "$destination" ]]; then mv "$destination" "$stage/previous.app"; fi
mv "$app" "$destination"
printf 'Installed %s to %s\n' "$tag" "$destination"
printf 'Open with: open "%s"\n' "$destination"
