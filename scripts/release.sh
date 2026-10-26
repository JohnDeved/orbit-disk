#!/bin/zsh
set +x
set -euo pipefail
cd "${0:A:h:h}"
tag="${1:?Usage: scripts/release.sh vX.Y.Z [--prepare-only]}"
prepare_only=false
if (( $# == 2 )) && [[ "$2" == --prepare-only ]]; then
  prepare_only=true
elif (( $# != 1 )); then
  print -u2 'Usage: scripts/release.sh vX.Y.Z [--prepare-only]'; exit 2
fi
signing_mode=$(python3 scripts/validate-release.py "$tag" --check-tags --signing-mode)
[[ -n "${SPARKLE_PRIVATE_KEY:-}" ]] || { print -u2 'SPARKLE_PRIVATE_KEY must contain the exported Sparkle signing key'; exit 1; }
[[ "$(git rev-parse "$tag^{commit}")" == "$(git rev-parse HEAD)" ]] || { print -u2 'Release tag must point to HEAD'; exit 1; }
[[ -z "$(git status --porcelain --untracked-files=normal)" ]] || { print -u2 'Release requires a clean working tree'; exit 1; }
repository=JohnDeved/orbit-disk
if ! $prepare_only; then
  remote_commit=$(gh api "repos/$repository/commits/$tag" --jq .sha)
  [[ "$remote_commit" == "$(git rev-parse HEAD)" ]] || { print -u2 'Remote release tag does not point to HEAD'; exit 1; }
  releases=$(gh api --paginate "repos/$repository/releases" --jq '.[].tag_name')
  if print -r -- "$releases" | /usr/bin/grep -Fxq -- "$tag"; then
    print -u2 "Release $tag already exists; published releases are never overwritten"
    exit 1
  fi
fi
mkdir -p .build/releases
staging=$(mktemp -d "$PWD/.build/releases/staging.XXXXXX")
trap 'rm -rf "$staging"' EXIT
(umask 077; printf '%s' "$SPARKLE_PRIVATE_KEY" > "$staging/signing-key")
unset SPARKLE_PRIVATE_KEY
./build-app.sh --universal --output "$staging/Orbit Disk.app"
./scripts/sign-release.sh "$staging/Orbit Disk.app"
unset APPLE_CERTIFICATE_P12_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
  APPLE_API_KEY_P8 APPLE_API_KEY_ID APPLE_API_ISSUER
archive_name="Orbit-Disk-${tag#v}-universal.zip"
mkdir "$staging/assets"
archive="$staging/assets/$archive_name"
ditto -c -k --sequesterRsrc --keepParent "$staging/Orbit Disk.app" "$archive"
tools="$PWD/.build/artifacts/sparkle/Sparkle/bin"
"$tools/generate_appcast" --ed-key-file "$staging/signing-key" \
  --download-url-prefix "https://github.com/$repository/releases/download/$tag/" \
  --link "https://github.com/$repository" --maximum-deltas 0 "$staging/assets"
signature=$(python3 scripts/validate-release.py "$tag" --appcast "$staging/assets/appcast.xml" --archive "$archive")
"$tools/sign_update" --verify --ed-key-file "$staging/signing-key" "$archive" "$signature"
"$tools/sign_update" --verify --ed-key-file "$staging/signing-key" "$staging/assets/appcast.xml"
rm "$staging/signing-key"
(cd "$staging/assets"; shasum -a 256 "$archive_name" appcast.xml > SHA256SUMS)
output="$PWD/.build/releases/$tag"
[[ ! -e "$output" ]] || { print -u2 "Release output already exists: $output"; exit 1; }
mv "$staging/assets" "$output"
if $prepare_only; then
  printf 'Verified release assets: %s\n' "$output"
  exit 0
fi
if [[ "$signing_mode" == adhoc ]]; then
  cat > "$staging/release-notes.md" <<'NOTES'
For macOS 14 or later, on Apple silicon and Intel Macs.

This build is not signed with an Apple Developer ID certificate or notarized by Apple. macOS may block the first launch; initial approval in System Settings > Privacy & Security is required. Future in-app updates are verified with Orbit Disk's Sparkle signing key.
NOTES
else
  cat > "$staging/release-notes.md" <<'NOTES'
For macOS 14 or later, on Apple silicon and Intel Macs. Signed with Developer ID and notarized by Apple. In-app updates are verified with Orbit Disk's Sparkle signing key.
NOTES
fi
# Assets stay private in a draft until the complete signed set has been uploaded.
gh release create "$tag" --repo "$repository" --verify-tag --draft \
  --title "Orbit Disk ${tag#v}" --generate-notes --notes-file "$staging/release-notes.md" \
  "$output/$archive_name" "$output/appcast.xml" "$output/SHA256SUMS"
gh release edit "$tag" --repo "$repository" --draft=false --latest
printf 'Published https://github.com/%s/releases/tag/%s\n' "$repository" "$tag"
