#!/bin/bash
set -eu
set -o pipefail
workspace=$(realpath "$(dirname "$0")/../../")
"$workspace/scripts/alley/build_catalog.sh" dblp "${1:-alley}" "${2:-0.001}" "${3:-0}"
