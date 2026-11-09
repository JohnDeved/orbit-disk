#!/bin/zsh
# Fails when an OS-specific API or crate is used outside its platform layer.
# Layer crates are named orbit-platform-<os> / orbit-shell-<os>; everything else must stay portable.
set -euo pipefail
cd "${0:A:h:h:h}"
apple='objc2|core_foundation|core-foundation|io_kit|io-kit|fsevent|FSEvent|getattrlistbulk|fgetattrlist|clonefile|setiopolicy_np|getiopolicy_np|NSFileManager|NSWorkspace|IOPlatform|Sparkle|SPU[A-Z]'
windows='windows-sys|windows::|winapi|ReadDirectoryChangesW|FindFirstFile'
linux='inotify|fanotify|statx|getdents'
failed=0
for crate in crates/*(/) apps/*(/); do
  name=${crate:t}
  case "$name" in
    orbit-platform-macos|orbit-shell-macos) continue ;;
    orbit-platform-windows|orbit-shell-windows) continue ;;
    orbit-platform-linux|orbit-shell-linux) continue ;;
    orbit-ui) pattern="$apple|$windows|$linux" ;; # gpui itself is allowed; raw OS APIs are not
    *) pattern="$apple|$windows|$linux" ;;
  esac
  if hits=$(grep -rnE "$pattern" "$crate" --include='*.rs' --include='Cargo.toml' 2>/dev/null | grep -vE "^[^:]+:[0-9]+:[[:space:]]*(//|#)"); then
    print -u2 "Platform API outside a platform layer in $name:"
    print -u2 -- "$hits"
    failed=1
  fi
done
exit $failed
