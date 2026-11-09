#!/bin/zsh
# Builds, signs and verifies the release assets, then (by default) publishes them.
#
#   scripts/release.sh vX.Y.Z                 publish (CI: .github/workflows/release.yml)
#   scripts/release.sh vX.Y.Z --prepare-only  verified assets in target/releases/vX.Y.Z, no publishing
#   scripts/release.sh vX.Y.Z --dry-run       local rehearsal, never publishes (see below)
#
# --dry-run accepts the current workspace version even when it is a prerelease (v1.0.0-alpha.0),
# needs no tag, clean tree, network write or secret: it uses a throwaway Sparkle key, ignores
# Apple credentials and rehearses Developer ID signing ad hoc (scripts/sign-release.sh --dry-run).
# Without Xcode's Metal toolchain it falls back to runtime shaders (with a warning). Assets go to
# target/releases/dry-run/<tag>.<time> and the script stops before any publishing step.
set +x
set -euo pipefail
cd "${0:A:h:h}"
usage='Usage: scripts/release.sh vX.Y.Z [--prepare-only | --dry-run]'
tag="${1:?$usage}"
mode=publish
if (( $# == 2 )); then
  case "$2" in
    --prepare-only) mode=prepare ;;
    --dry-run) mode=dry-run ;;
    *) print -u2 "$usage"; exit 2 ;;
  esac
elif (( $# != 1 )); then
  print -u2 "$usage"; exit 2
fi
command -v cargo >/dev/null || path=("$HOME/.cargo/bin" $path)
repository=JohnDeved/orbit-disk
if [[ "$mode" == dry-run ]]; then
  python3 scripts/validate-release.py "$tag" --allow-prerelease --check-tags >/dev/null
  signing_mode=adhoc
  unset SPARKLE_PRIVATE_KEY APPLE_CERTIFICATE_P12_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
    APPLE_API_KEY_P8 APPLE_API_KEY_ID APPLE_API_ISSUER
  [[ -z "$(git status --porcelain --untracked-files=normal)" ]] || print -u2 'Dry run: working tree is not clean (a release would refuse).'
  git rev-parse -q --verify "$tag^{commit}" >/dev/null || print -u2 "Dry run: tag $tag does not exist (a release would refuse)."
else
  signing_mode=$(python3 scripts/validate-release.py "$tag" --check-tags --signing-mode)
  [[ -n "${SPARKLE_PRIVATE_KEY:-}" ]] || { print -u2 'SPARKLE_PRIVATE_KEY must contain the exported Sparkle signing key'; exit 1; }
  [[ "$(git rev-parse "$tag^{commit}")" == "$(git rev-parse HEAD)" ]] || { print -u2 'Release tag must point to HEAD'; exit 1; }
  [[ -z "$(git status --porcelain --untracked-files=normal)" ]] || { print -u2 'Release requires a clean working tree'; exit 1; }
fi
if [[ "$mode" == publish ]]; then
  remote_commit=$(gh api "repos/$repository/commits/$tag" --jq .sha)
  [[ "$remote_commit" == "$(git rev-parse HEAD)" ]] || { print -u2 'Remote release tag does not point to HEAD'; exit 1; }
  releases=$(gh api --paginate "repos/$repository/releases" --jq '.[].tag_name')
  if print -r -- "$releases" | /usr/bin/grep -Fxq -- "$tag"; then
    print -u2 "Release $tag already exists; published releases are never overwritten"
    exit 1
  fi
fi
mkdir -p target/releases
staging=$(mktemp -d "$PWD/target/releases/staging.XXXXXX")
trap 'rm -rf "$staging"' EXIT
if [[ "$mode" == dry-run ]]; then
  # A throwaway EdDSA key; its public half replaces SUPublicEDKey in the dry-run bundle below.
  dry_run_public_key=$(python3 scripts/dry-run-sparkle-key.py "$staging/signing-key")
  build_flags=()
  validate_flags=(--allow-prerelease)
else
  (umask 077; printf '%s' "$SPARKLE_PRIVATE_KEY" > "$staging/signing-key")
  unset SPARKLE_PRIVATE_KEY
  build_flags=(--release-mode)
  validate_flags=()
fi
app="$staging/Orbit Disk.app"
./build-app.sh --universal "${build_flags[@]}" --output "$app"
# The stamped bundle must equal Config/Info.plist with the workspace version filled in.
python3 scripts/validate-release.py "$tag" --bundle "$app" "${validate_flags[@]}"
if [[ "$mode" == dry-run ]]; then
  # generate_appcast only signs archives whose app trusts the signing key. The rehearsal
  # signature below re-seals the changed Info.plist.
  plutil -replace SUPublicEDKey -string "$dry_run_public_key" "$app/Contents/Info.plist"
  ./scripts/sign-release.sh "$app" --dry-run
else
  ./scripts/sign-release.sh "$app"
fi
unset APPLE_CERTIFICATE_P12_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
  APPLE_API_KEY_P8 APPLE_API_KEY_ID APPLE_API_ISSUER
archive_name="Orbit-Disk-${tag#v}-universal.zip"
mkdir "$staging/assets"
archive="$staging/assets/$archive_name"
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
cargo xtask sparkle "$staging/sparkle" >/dev/null
tools="$staging/sparkle/bin"
"$tools/generate_appcast" --ed-key-file "$staging/signing-key" \
  --download-url-prefix "https://github.com/$repository/releases/download/$tag/" \
  --link "https://github.com/$repository" --maximum-deltas 0 "$staging/assets"
signature=$(python3 scripts/validate-release.py "$tag" --appcast "$staging/assets/appcast.xml" --archive "$archive" \
  "${validate_flags[@]}")
"$tools/sign_update" --verify --ed-key-file "$staging/signing-key" "$archive" "$signature"
"$tools/sign_update" --verify --ed-key-file "$staging/signing-key" "$staging/assets/appcast.xml"
rm "$staging/signing-key"
dmg_name="Orbit-Disk.dmg"
./scripts/package-dmg.sh "$app" "$staging/assets/$dmg_name"
(cd "$staging/assets"; shasum -a 256 "$archive_name" "$dmg_name" appcast.xml > SHA256SUMS)
: > "$staging/release-notes.md"
if [[ -f "docs/releases/$tag.md" ]]; then
  cat "docs/releases/$tag.md" >> "$staging/release-notes.md"
  printf '\n\n' >> "$staging/release-notes.md"
fi
if [[ "$signing_mode" == adhoc ]]; then
  cat >> "$staging/release-notes.md" <<'NOTES'
For macOS 14 or later, on Apple silicon and Intel Macs.

This build is not signed with an Apple Developer ID certificate or notarized by Apple. macOS may block the first launch; initial approval in System Settings > Privacy & Security is required. Future in-app updates are verified with Orbit Disk's Sparkle signing key.
NOTES
else
  cat >> "$staging/release-notes.md" <<'NOTES'
For macOS 14 or later, on Apple silicon and Intel Macs. The app is signed with Developer ID and notarized by Apple; the disk image itself is not signed or notarized. In-app updates are verified with Orbit Disk's Sparkle signing key.
NOTES
fi
if [[ "$mode" == dry-run ]]; then
  output="$PWD/target/releases/dry-run/$tag.$(date +%Y%m%d-%H%M%S)"
  mkdir -p "${output:h}"
else
  output="$PWD/target/releases/$tag"
fi
[[ ! -e "$output" ]] || { print -u2 "Release output already exists: $output"; exit 1; }
mv "$staging/assets" "$output"
if [[ "$mode" != publish ]]; then
  cp "$staging/release-notes.md" "$output.release-notes.md"
  printf 'Verified release assets: %s\n' "$output"
  if [[ "$mode" == dry-run ]]; then
    print 'Dry run: signed with a throwaway Sparkle key and an ad-hoc hardened-runtime signature; not publishable.'
    print "Dry run: stopping before publishing. A release would run: gh release create $tag --draft … && gh release edit $tag --draft=false --latest"
  fi
  exit 0
fi
# Assets stay private in a draft until the complete signed set has been uploaded.
gh release create "$tag" --repo "$repository" --verify-tag --draft \
  --title "Orbit Disk ${tag#v}" --generate-notes --notes-file "$staging/release-notes.md" \
  "$output/$archive_name" "$output/$dmg_name" "$output/appcast.xml" "$output/SHA256SUMS"
gh release edit "$tag" --repo "$repository" --draft=false --latest
printf 'Published https://github.com/%s/releases/tag/%s\n' "$repository" "$tag"
