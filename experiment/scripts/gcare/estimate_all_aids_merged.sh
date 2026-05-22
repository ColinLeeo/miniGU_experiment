#!/bin/bash
set -eu
set -o pipefail

method=$1
ratio=${2:-0.03}
repeat=${3:-30}
seed=${4:-0}

workspace=$(realpath "$(dirname "$0")/../../")
graph_dir=$workspace/datasets/aids_merged
pattern_dir=$workspace/patterns/gcare/aids_merged
log_file=$workspace/result/gcare/estimate/aids_merged_${method}.log
mkdir -p "$(dirname "$log_file")"
: > "$log_file"

mapfile -t patterns < <(python3 - "$pattern_dir" <<'PY'
import sys
from pathlib import Path
root = Path(sys.argv[1])
for p in sorted(root.rglob("*.gcare")):
    print(str(p.resolve()))
PY
)

for pattern in "${patterns[@]}"; do
  rel="${pattern#$pattern_dir/}"
  result=$(timeout -v 11m "$workspace/scripts/gcare/estimate.sh" "$method" "$graph_dir" "$pattern" "$ratio" "$repeat" "$seed" 2>&1 | tail -n 1 || true)
  echo "$rel: $result" | tee -a "$log_file"
done

echo "Log: $log_file"