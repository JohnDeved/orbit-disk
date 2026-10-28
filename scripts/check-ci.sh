#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h}"
if (( $# != 1 )); then
  print -u2 'Usage: scripts/check-ci.sh core|native|search|sessions|mapping|staging|removal|rendering'
  exit 2
fi
case "$1" in
  core)
    swift package resolve
    python3 Tests/test_update_copy.py
    swift run -c release OrbitCoreChecks
    ./check-updater.sh
    ;;
  native)
    ./check-app.sh --native-only
    ;;
  search)
    ./check-prepared-search.sh
    ./check-preview-batches.sh
    ./check-preview-search.sh
    ./check-stage-search-fast.sh
    ./check-saved-search.sh
    ;;
  sessions)
    ./check-session.sh
    ;;
  mapping)
    ./check-discovery-cache.sh
    ./check-async-mapping.sh
    ./check-resumable.sh
    ./check-mapping-reuse.sh
    ;;
  staging)
    ./check-stage.sh
    ./check-removal-journal.sh
    ./check-stage-removal.sh
    ;;
  removal)
    ./check-durable-removal.sh
    ./check-folder-transaction.sh
    ;;
  rendering)
    ./check-rendering.sh
    ./check-layout.sh
    ./check-folder-places.sh
    ./check-cache-reset.sh
    ;;
  *)
    print -u2 "Unknown CI suite: $1"
    exit 2
    ;;
esac
