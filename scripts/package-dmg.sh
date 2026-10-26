#!/bin/zsh
set -euo pipefail

app="${1:?Usage: scripts/package-dmg.sh PATH.app OUTPUT.dmg}"
output="${2:?Usage: scripts/package-dmg.sh PATH.app OUTPUT.dmg}"
(( $# == 2 )) || { print -u2 'Usage: scripts/package-dmg.sh PATH.app OUTPUT.dmg'; exit 2; }
app="${app:A}"
output="${output:A}"
[[ -d "$app/Contents" && "$output" == *.dmg && ! -e "$output" ]] || {
  print -u2 'An existing app and a new .dmg output path are required'; exit 1
}

staging=$(mktemp -d "${TMPDIR:-/tmp}/orbit-dmg.XXXXXX")
mounted=false
cleanup() {
  if $mounted && ! hdiutil detach "$staging/mount" -quiet; then
    print -u2 "Could not detach disk image at $staging/mount; leaving it in place"
    return 1
  fi
  rm -rf "$staging"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
mkdir "$staging/content" "$staging/mount"
ditto "$app" "$staging/content/Orbit Disk.app"
ln -s /Applications "$staging/content/Applications"
hdiutil create -quiet -volname 'Orbit Disk' -srcfolder "$staging/content" \
  -format UDZO "$staging/Orbit-Disk.dmg"
hdiutil verify -quiet "$staging/Orbit-Disk.dmg"
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$staging/mount" "$staging/Orbit-Disk.dmg"
mounted=true
[[ "$(readlink "$staging/mount/Applications")" == /Applications ]]
codesign --verify --deep --strict "$staging/mount/Orbit Disk.app"
differences=$(rsync -rnclip --delete --out-format='%i %n' "$app/" "$staging/mount/Orbit Disk.app/")
[[ -z "$differences" ]] || { print -u2 "Disk image differs from the original app: $differences"; exit 1; }
hdiutil detach -quiet "$staging/mount"
mounted=false
mv "$staging/Orbit-Disk.dmg" "$output"
