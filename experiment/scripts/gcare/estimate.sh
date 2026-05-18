#!/bin/bash
set -eu
set -o pipefail

method=$1
graph_dir=$(realpath "$2")
pattern=$(realpath "$3")
ratio=${4:-0.03}
repeat=${5:-30}
seed=${6:-0}

workspace=$(realpath "$(dirname "$0")/../../")

if [[ $method == "cs" ]] || [[ $method == "bsk" ]]; then
  bin="$workspace/baseline/gcare/build/gcare_relation"
else
  bin="$workspace/baseline/gcare/build/gcare_graph"
fi

"$bin" -q -m "$method" -i "$pattern" -d "$graph_dir" -p "$ratio" -n "$repeat" -s "$seed"
