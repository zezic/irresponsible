#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
archive="$1"
version="$2"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/irresponsible-installer.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT
mkdir "$fixture/bin"
# Repack a quarantined copy so removal is exercised without changing the build.
ditto -x -k "$archive" "$fixture/payload"
xattr -w com.apple.quarantine '0083;00000000;IRresponsible installer test;' "$fixture/payload/IRresponsible.app"
name="$(basename "$archive")"
ditto -c -k --sequesterRsrc --keepParent "$fixture/payload/IRresponsible.app" "$fixture/$name"
(cd "$fixture" && shasum -a 256 "$name" > SHA256SUMS)
printf '{"tag_name":"v%s"}\n' "$version" > "$fixture/release.json"
cat > "$fixture/bin/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
output=""
while (($#)); do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  https://api.github.com/repos/zezic/irresponsible/releases/latest) file=release.json ;;
  "https://github.com/zezic/irresponsible/releases/download/v${TEST_VERSION}/${TEST_ARCHIVE}") file="$TEST_ARCHIVE" ;;
  "https://github.com/zezic/irresponsible/releases/download/v${TEST_VERSION}/SHA256SUMS") file=SHA256SUMS ;;
  *) printf 'Unexpected installer request: %s\n' "$url" >&2; exit 1 ;;
esac
printf '%s\n' "$url" >> "$TEST_FIXTURE/requests"
cp "$TEST_FIXTURE/$file" "$output"
MOCK
# The fixture models a stopped app without touching any running local instance.
printf '#!/usr/bin/env bash\nexit 1\n' > "$fixture/bin/pgrep"
chmod +x "$fixture/bin/curl" "$fixture/bin/pgrep"
export TEST_VERSION="$version" TEST_ARCHIVE="$name" TEST_FIXTURE="$fixture"
export PATH="$fixture/bin:$PATH"
/bin/bash scripts/install.sh --directory "$fixture/Applications"
installed="$fixture/Applications/IRresponsible.app"
codesign --verify --deep --strict "$installed"
if xattr -p com.apple.quarantine "$installed" >/dev/null 2>&1; then
  printf 'Installer left quarantine on the app.\n' >&2
  exit 1
fi
before="$(shasum -a 256 "$installed/Contents/MacOS/irresponsible")"
printf '%064d  %s\n' 0 "$name" > "$fixture/SHA256SUMS"
if /bin/bash scripts/install.sh --directory "$fixture/Applications"; then
  printf 'Installer accepted an invalid checksum.\n' >&2
  exit 1
fi
test "$(shasum -a 256 "$installed/Contents/MacOS/irresponsible")" = "$before"
# Checksum success does not excuse a mismatched app version.
original_version="$TEST_VERSION"
original_archive="$TEST_ARCHIVE"
export TEST_VERSION="${version}-installer-test"
export TEST_ARCHIVE="${name/$version/$TEST_VERSION}"
cp "$fixture/$name" "$fixture/$TEST_ARCHIVE"
printf '{"tag_name":"v%s"}\n' "$TEST_VERSION" > "$fixture/release.json"
(cd "$fixture" && shasum -a 256 "$TEST_ARCHIVE" > SHA256SUMS)
if /bin/bash scripts/install.sh --directory "$fixture/Applications"; then
  printf 'Installer accepted a mismatched app version.\n' >&2
  exit 1
fi
test "$(shasum -a 256 "$installed/Contents/MacOS/irresponsible")" = "$before"
export TEST_VERSION="$original_version" TEST_ARCHIVE="$original_archive"
printf '{"tag_name":"v%s"}\n' "$version" > "$fixture/release.json"
# A failed final rename restores the existing app from the staged backup.
(cd "$fixture" && shasum -a 256 "$name" > SHA256SUMS)
cat > "$fixture/bin/mv" <<'MOCK'
#!/usr/bin/env bash
case "$1" in
  */unpacked/IRresponsible.app) printf 'Simulated replacement failure\n' >&2; exit 1 ;;
esac
exec /bin/mv "$@"
MOCK
chmod +x "$fixture/bin/mv"
if /bin/bash scripts/install.sh --directory "$fixture/Applications"; then
  printf 'Installer ignored a failed replacement.\n' >&2
  exit 1
fi
test "$(shasum -a 256 "$installed/Contents/MacOS/irresponsible")" = "$before"
# An app reported as running must stop the installer before any downloads.
printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/bin/pgrep"
requests="$(wc -l < "$fixture/requests")"
if /bin/bash scripts/install.sh --directory "$fixture/Applications"; then
  printf 'Installer continued while the app was running.\n' >&2
  exit 1
fi
test "$(wc -l < "$fixture/requests")" = "$requests"
printf 'Installer verified quarantine removal, checksum and version rejection, rollback, and running-app protection.\n'
