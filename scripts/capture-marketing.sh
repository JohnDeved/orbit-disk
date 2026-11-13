#!/bin/zsh
# Renders the README hero and feature cards from the Rust app.
#
#   scripts/capture-marketing.sh [--app PATH.app] [--raw DIR] [--default-state]
#
# Launches `OrbitDisk --fixture` (deterministic generated tree, isolated HOME so no settings or
# saved maps leak in) once per state, finds its window through CGWindowList (JavaScript for
# Automation, scripts/marketing/window-id.js; no Swift, no Python modules), captures it with
# `screencapture -l`, then composes docs/screenshots/orbit-hero-*.png and docs/art/card-*.png with
# the standalone scripts/marketing/{Hero,Cards}.swift compositors (AppKit image drawing only; they
# do not compile the app). docs/art/perf-*.svg and download-mac.svg are hand-written, not rendered.
#
# The app does not yet expose deterministic marketing states. Until it does, this script stops
# with exit status 3 (the screenshots workflow then skips the commit). `--default-state`
# captures the plain fixture window to raw/orbit-default.png only, to check the capture path.
#
# Required app additions (apps/orbit-disk args + orbit-ui), all fixture-only:
#   --appearance dark|light     force the window appearance (screenshots are dark).
#   --window-size 1280x810      fixed content size in points; not restored from settings.
#   --marketing-state STATE     implies --fixture; STATE is one of
#       overview    root map, "Projects" folder selected, empty collection
#       collection  "Downloads" selected and collected (tray visible)
#       projects    inside "Projects", its first child selected
#       review      removal review sheet for the collected "Downloads"
#       permanent-delete  the review with its permanent-deletion confirmation open
#       hero        "Downloads" collected, then "Projects" selected (the README hero)
#       downloads   "Downloads" selected, nothing collected (start of the collect drag)
#       projects-open  inside "Projects" with nothing selected (the map center describes it)
#     (the app also has projects and trash-confirm; the README does not use them)
#     Mapping is already finished (no progress UI), animations are off (reduce motion), the
#     window is key, and after the first frame showing the state is presented the app prints
#     `marketing-state: ready` on stderr and stays open until terminated.
#   The fixture tree needs stable, recognizable "Projects" and "Downloads" folders under the
#   demo home (the Swift fixture is scripts/marketing/FixtureExtension.swift at v0.9.15).
# Screen Recording permission is needed for screencapture to see the window's contents.
set -euo pipefail
cd "${0:A:h:h}"
root="$PWD"
usage='Usage: scripts/capture-marketing.sh [--app PATH.app] [--raw DIR] [--default-state]'
app=""
raw=""
default_state=false
while (( $# )); do
  case "$1" in
    --app) app="${2:?--app needs a path}"; shift 2 ;;
    --raw) raw="${2:?--raw needs a directory}"; shift 2 ;;
    --default-state) default_state=true; shift ;;
    *) print -u2 "$usage"; exit 2 ;;
  esac
done
tmp=$(mktemp -d "${TMPDIR:-/tmp}/OrbitMarketing.XXXXXX")
pid=""
cleanup() {
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  rm -rf "$tmp"
}
# zsh skips EXIT traps when errexit fires inside a function, so functions fail through `fail`.
fail() { print -u2 -- "$*"; exit 1; }
trap cleanup EXIT
if [[ -z "$app" ]]; then
  print "Building the app for capture…"
  app="$tmp/Orbit Disk.app"
  ./build-app.sh --output "$app"
fi
exe="${app:A}/Contents/MacOS/OrbitDisk"
[[ -x "$exe" ]] || { print -u2 "No OrbitDisk executable in $app"; exit 1; }
raw="${raw:-$tmp/raw}"
mkdir -p "$raw" "$tmp/home"

help_text=$("$exe" --help 2>&1 || true)
if ! $default_state && [[ "$help_text" != *--marketing-state* ]]; then
  print -u2 'OrbitDisk has no --marketing-state mode yet, so deterministic screenshots cannot be captured.'
  print -u2 'See the header of scripts/capture-marketing.sh for the app flags this needs.'
  print -u2 'Use --default-state to check the capture path with the plain fixture window.'
  exit 3
fi

# Launches the app, waits for its window (and the ready line), captures it, terminates it.
capture() {
  local name="$1"; shift
  local log="$tmp/$name.log" window="" ready=false waited=0 width height
  HOME="$tmp/home" "$exe" "$@" 2> "$log" &
  pid=$!
  while (( waited < 120 )) && ! $ready; do
    kill -0 "$pid" 2>/dev/null || fail "OrbitDisk exited early: $(cat "$log")"
    window=$(osascript -l JavaScript scripts/marketing/window-id.js "$pid" 2>/dev/null || true)
    if [[ -n "$window" ]]; then
      if $default_state; then
        sleep 4 # the fixture's simulated mapping takes 2.5 s
        ready=true
      elif /usr/bin/grep -q 'marketing-state: ready' "$log"; then
        sleep 0.3
        ready=true
      fi
    fi
    $ready || sleep 0.5
    (( waited += 1 ))
  done
  $ready || fail "OrbitDisk did not show $name within 60 s: $(cat "$log")"
  window=$(osascript -l JavaScript scripts/marketing/window-id.js "$pid") || fail 'CGWindowList lookup failed'
  [[ -n "$window" ]] || fail "The window for $name disappeared"
  screencapture -x -o -l "${window%% *}" "$raw/$name" ||
    fail "screencapture failed for $name (does this terminal have Screen Recording permission?)"
  kill "$pid"; wait "$pid" 2>/dev/null || true; pid=""
  width=$(sips -g pixelWidth "$raw/$name" | awk '/pixelWidth/ {print $2}') || fail "sips failed"
  height=$(sips -g pixelHeight "$raw/$name" | awk '/pixelHeight/ {print $2}') || fail "sips failed"
  print "Captured $name: ${width} × ${height} px (window ${window#* } pt)"
  (( width >= 1200 && height >= 700 )) || fail "Capture of $name is too small"
}

if $default_state; then
  capture orbit-default.png --fixture
  print "Default-state capture in $raw (README images unchanged)."
  exit 0
fi
common=(--appearance dark --window-size 1280x810)
capture orbit-overview.png "${common[@]}" --marketing-state overview
capture orbit-collection.png "${common[@]}" --marketing-state collection
capture orbit-hero-state.png "${common[@]}" --marketing-state hero
capture orbit-downloads.png "${common[@]}" --marketing-state downloads
capture orbit-projects-open.png "${common[@]}" --marketing-state projects-open
capture orbit-review.png "${common[@]}" --marketing-state review
capture orbit-permanent-delete.png "${common[@]}" --marketing-state permanent-delete

mkdir -p "$root/docs/screenshots" "$root/docs/art"
target="$(uname -m)-apple-macosx14.0"
swiftc -swift-version 5 -target "$target" -module-cache-path "$tmp/module-cache" \
  scripts/marketing/Hero.swift -o "$tmp/Hero"
"$tmp/Hero" "$raw" "$root/docs/screenshots"
swiftc -swift-version 5 -target "$target" -module-cache-path "$tmp/module-cache" \
  scripts/marketing/Cards.swift -o "$tmp/Cards"
"$tmp/Cards" "$raw" "$root/docs/art"
print "Rendered the README hero and feature cards."
