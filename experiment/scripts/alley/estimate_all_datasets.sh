#!/bin/bash
set -eu
set -o pipefail

method=${1:-alley}
ratio=${2:-0.001}
repeat=${3:-30}
seed=${4:-0}

workspace=$(realpath "$(dirname "$0")/../../")
for dataset in aids_merged dblp ldbc_sf1 imdb; do
  "$workspace/scripts/alley/estimate_all.sh" "$dataset" "$method" "$ratio" "$repeat" "$seed"
done
