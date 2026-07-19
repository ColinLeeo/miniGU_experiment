#!/bin/bash
set -eu
set -o pipefail

method=${1:-alley}
ratio=${2:-0.001}
seed=${3:-0}

workspace=$(realpath "$(dirname "$0")/../../")
for dataset in aids_merged dblp ldbc_sf1 imdb; do
  "$workspace/scripts/alley/build_catalog.sh" "$dataset" "$method" "$ratio" "$seed"
done
