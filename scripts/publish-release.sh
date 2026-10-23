#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../dist"
: "${GH_REPO:?}" "${RELEASE_TAG:?}" "${VERSION:?}" "${PRERELEASE:?}"
if [[ "$RELEASE_TAG" != "v$VERSION" ]]; then
  printf 'Release tag does not match the built version.\n' >&2
  exit 1
fi
assets=("IRresponsible-${VERSION}-macos-arm64.zip" "IRresponsible-${VERSION}-macos-x86_64.zip")
for file in "${assets[@]}"; do test -s "$file"; done
sha256sum "${assets[@]}" > SHA256SUMS

state="$(gh api --paginate --slurp "repos/$GH_REPO/releases" |
  jq -r --arg tag "$RELEASE_TAG" 'flatten | map(select(.tag_name == $tag)) |
    if length == 0 then "missing" elif .[0].draft then "draft" else "published" end')"
case "$state" in
  published) printf 'Release %s is already published.\n' "$RELEASE_TAG"; exit 0 ;;
  missing)
    cat > release-notes.md <<'NOTES'
Download the arm64 archive for Apple Silicon or x86_64 for Intel. BlackHole 2ch is required.

These builds are ad-hoc signed and are not notarized. The repository's Bash installer verifies the release checksum, installs to ~/Applications, and removes quarantine from the app:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/zezic/irresponsible/master/scripts/install.sh | bash
```
NOTES
    args=(--draft --verify-tag --generate-notes --title "IRresponsible $RELEASE_TAG" --notes-file release-notes.md)
    if [[ "$PRERELEASE" == true ]]; then args+=(--prerelease); fi
    gh release create "$RELEASE_TAG" "${args[@]}"
    ;;
  draft) ;;
  *) printf 'Unexpected release state: %s\n' "$state" >&2; exit 1 ;;
esac
# Resuming a failed upload replaces assets only while the release is a draft.
gh release upload "$RELEASE_TAG" "${assets[@]}" SHA256SUMS --clobber
gh release edit "$RELEASE_TAG" --draft=false --prerelease="$PRERELEASE"
