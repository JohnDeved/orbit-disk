#!/bin/zsh
# Builds Orbit Disk.app from the Rust workspace. A thin wrapper over `cargo xtask bundle`, which
# does the work: version stamping, Metal shader mode, universal lipo, Sparkle and ad-hoc signing.
set -euo pipefail
cd "${0:A:h}"
usage='Usage: ./build-app.sh [--universal] [--output PATH.app] [--release-mode]'
app_path="${PWD:h}/Orbit Disk.app"
flags=()
while (( $# )); do
  case "$1" in
    --universal) flags+=(--universal); shift ;;
    # Precompiled Metal shaders are mandatory (fails without Xcode's Metal toolchain).
    --release-mode) flags+=(--release-mode); shift ;;
    --output) app_path="${2:?--output needs an app path}"; shift 2 ;;
    *) print -u2 "$usage"; exit 2 ;;
  esac
done
app_path="${app_path:A}"
[[ "$app_path" == *.app ]] || { print -u2 'Output must end in .app'; exit 2; }
command -v cargo >/dev/null || path=("$HOME/.cargo/bin" $path)
exec cargo xtask bundle "${flags[@]}" --output "$app_path"
