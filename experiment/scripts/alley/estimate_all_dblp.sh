#!/bin/bash
set -eu
set -o pipefail
workspace=$(realpath "$(dirname "$0")/../../")
"$workspace/scripts/alley/estimate_all.sh" dblp "${1:-alley}" "${2:-0.001}" "${3:-30}" "${4:-0}"
