#!/bin/sh
# Installs the latest Orbit Disk release:
#
#   curl -fsSL https://raw.githubusercontent.com/JohnDeved/orbit-disk/main/install.sh | sh
#
# Downloads the release's ZIP (the archive the in-app updater uses), checks it against the
# release's SHA256SUMS and the app's code signature, and puts Orbit Disk.app in /Applications
# (or ~/Applications when /Applications isn't writable). A download made with curl isn't
# quarantined, so macOS doesn't ask to approve the first launch. Later updates arrive in the app.
#
# Environment:
#   ORBIT_DISK_VERSION=0.11.4   install this release instead of the latest
#   ORBIT_DISK_DIR=PATH         install into this folder
#   ORBIT_DISK_NO_OPEN=1        don't open the app afterwards
set -eu

repo=JohnDeved/orbit-disk
app_name='Orbit Disk.app'
bundle_id=local.orbitdisk.app

say() { printf '%s\n' "$*"; }
fail() { printf 'Orbit Disk install: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || fail 'Orbit Disk runs on macOS only.'
major=$(sw_vers -productVersion | cut -d. -f1)
[ "$major" -ge 14 ] 2>/dev/null || fail "macOS 14 or later is required (this Mac has $(sw_vers -productVersion))."

if [ -n "${ORBIT_DISK_VERSION:-}" ]; then
  tag="v${ORBIT_DISK_VERSION#v}"
else
  # The latest release page redirects to its tag; no API token or rate limit involved.
  latest=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") ||
    fail 'could not reach GitHub.'
  tag=${latest##*/}
fi
case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *) fail "could not find the latest release (got '$tag')." ;;
esac
version=${tag#v}
archive="Orbit-Disk-$version-universal.zip"
base="https://github.com/$repo/releases/download/$tag"

work=$(mktemp -d "${TMPDIR:-/tmp}/orbit-disk-install.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

say "Downloading Orbit Disk ${version}…"
curl -fsSL "$base/SHA256SUMS" -o "$work/SHA256SUMS" || fail "release $tag has no SHA256SUMS."
curl -fL --progress-bar "$base/$archive" -o "$work/$archive" || fail "could not download $archive."

expected=$(awk -v f="$archive" '$2 == f { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "SHA256SUMS does not list $archive."
actual=$(shasum -a 256 "$work/$archive" | cut -d' ' -f1)
[ "$actual" = "$expected" ] || fail "checksum mismatch for $archive; the download is damaged. Nothing was installed."

ditto -x -k "$work/$archive" "$work/unpacked"
new="$work/unpacked/$app_name"
[ -d "$new" ] || fail "$archive does not contain $app_name."
[ "$(defaults read "$new/Contents/Info" CFBundleIdentifier 2>/dev/null)" = "$bundle_id" ] ||
  fail "$archive contains an unexpected app."
codesign --verify --deep --strict "$new" 2>/dev/null || fail "the app's code signature is invalid. Nothing was installed."

if [ -n "${ORBIT_DISK_DIR:-}" ]; then
  dir=$ORBIT_DISK_DIR
  mkdir -p "$dir"
elif [ -w /Applications ]; then
  dir=/Applications
else
  dir="$HOME/Applications"
  mkdir -p "$dir"
fi
target="$dir/$app_name"

if [ -e "$target" ]; then
  [ "$(defaults read "$target/Contents/Info" CFBundleIdentifier 2>/dev/null)" = "$bundle_id" ] ||
    fail "$target exists and is not Orbit Disk; move it away and try again."
  if pgrep -qf "$target/Contents/MacOS/"; then
    fail 'Orbit Disk is running. Quit it (Orbit Disk → Quit) and run the installer again.'
  fi
fi

# Copy next to the target first, so a failed copy never leaves a half-installed app.
staged="$dir/.orbit-disk-install.$$.app"
ditto "$new" "$staged" || { rm -rf "$staged"; fail "could not write to $dir."; }
rm -rf "$target"
mv "$staged" "$target"

say "Installed Orbit Disk $version in $dir."
if [ -z "${ORBIT_DISK_NO_OPEN:-}" ]; then
  open "$target"
fi
