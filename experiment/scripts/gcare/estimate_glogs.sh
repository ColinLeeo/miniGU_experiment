#!/bin/bash
set -eu
set -o pipefail

method=$1
sf=${2:-1}
ratio=${3:-0.03}
repeat=${4:-30}
seed=${5:-0}

workspace=$(realpath "$(dirname "$0")/../../")
graph_dir=${GLOGS_GCARE_GRAPH_DIR:-$workspace/catalogs/ldbc/gcare/sf$sf}
pattern_dir=${GLOGS_GCARE_PATTERN_DIR:-$workspace/patterns/gcare/glogs}
log_file=$workspace/result/baselines/gcare/estimate/glogs_sf${sf}_${method}.log
mkdir -p "$(dirname "$log_file")"
: > "$log_file"

if [[ ! -f "$graph_dir.graph.meta" ]]; then
  echo "gcare binary graph not found: $graph_dir.graph.meta" >&2
  echo "Build GCare catalog first, or set GLOGS_GCARE_GRAPH_DIR to a GCare data prefix." >&2
  exit 1
fi
if [[ ! -d "$pattern_dir" ]]; then
  echo "pattern dir not found: $pattern_dir" >&2
  exit 1
fi

mapfile -t patterns < <(find "$pattern_dir" -name '*.gcare' -type f | sort -V)
if [[ ${#patterns[@]} -eq 0 ]]; then
  echo "no pattern gcare files found under: $pattern_dir" >&2
  exit 1
fi

for pattern in "${patterns[@]}"; do
  rel="${pattern#$pattern_dir/}"
  result=$(timeout -v 5m "$workspace/scripts/gcare/estimate.sh" "$method" "$graph_dir" "$pattern" "$ratio" "$repeat" "$seed" 2>&1 | tail -n 1 || true)
  echo "$rel: $result" | tee -a "$log_file"
done

echo "Log: $log_file"
