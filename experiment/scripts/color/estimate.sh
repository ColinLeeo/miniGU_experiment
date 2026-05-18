#!/bin/bash
set -eu
set -o pipefail

summary=$(realpath $1)
pattern=$(realpath $2)

workspace=$(realpath "$(dirname "$0")/../../")

julia_bin="${JULIA_BIN:-julia}"
if ! command -v "$julia_bin" >/dev/null 2>&1; then
  julia_bin="$HOME/.local/bin/julia"
fi

"$julia_bin" --project="$workspace/baseline/color" "$workspace/baseline/color/scripts/estimate.jl" -q "$pattern" -s "$summary"
