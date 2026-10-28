#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
if (( $# != 1 )); then
  print -u2 'Usage: scripts/checks/check-ci.sh core|native|search|sessions|mapping|staging|removal|rendering'
  exit 2
fi
case "$1" in
  core)
    swift package resolve
    python3 Tests/test_update_copy.py
    swift build -c release --product OrbitControl
    python3 Tests/test_mcp.py "$(swift build -c release --show-bin-path)/OrbitControl"
    swift run -c release OrbitCoreChecks
    ./scripts/checks/check-updater.sh
    ;;
  native)
    ./scripts/checks/check-app.sh --native-only
    ./scripts/checks/check-automation.sh
    ./scripts/checks/check-automation-transport.sh
    ;;
  search)
    ./scripts/checks/check-prepared-search.sh
    ./scripts/checks/check-preview-batches.sh
    ./scripts/checks/check-preview-search.sh
    ./scripts/checks/check-stage-search-fast.sh
    ./scripts/checks/check-saved-search.sh
    ;;
  sessions)
    ./scripts/checks/check-session.sh
    ;;
  mapping)
    ./scripts/checks/check-boot-volume.sh
    ./scripts/checks/check-cloud-scanning.sh
    ./scripts/checks/check-discovery-cache.sh
    ./scripts/checks/check-async-mapping.sh
    ./scripts/checks/check-resumable.sh
    ./scripts/checks/check-mapping-reuse.sh
    ;;
  staging)
    ./scripts/checks/check-stage.sh
    ./scripts/checks/check-removal-journal.sh
    ./scripts/checks/check-stage-removal.sh
    ;;
  removal)
    ./scripts/checks/check-durable-removal.sh
    ./scripts/checks/check-folder-transaction.sh
    ;;
  rendering)
    ./scripts/checks/check-rendering.sh
    ./scripts/checks/check-layout.sh
    ./scripts/checks/check-folder-places.sh
    ./scripts/checks/check-places-interaction.sh
    ./scripts/checks/check-cache-reset.sh
    ;;
  *)
    print -u2 "Unknown CI suite: $1"
    exit 2
    ;;
esac
