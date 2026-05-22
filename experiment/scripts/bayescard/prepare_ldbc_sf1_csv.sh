#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
REPRO_DIR="$ROOT/experiment/baseline/SafeBound/repro_bayescard_ldbc_sf1"
VENV="${VENV:-/home/zxz/miniGU/experiment/baseline/SafeBound/repro_bayescard_ldbc_sf1/env}"

if [[ ! -f "$VENV/bin/activate" ]]; then
  echo "BayesCard venv not found: $VENV" >&2
  echo "Set VENV=/path/to/bayescard/env" >&2
  exit 1
fi
source "$VENV/bin/activate"

# Prepare LDBC SF1 CSVs for BayesCard.
RAW_DATA_DIR="${RAW_DATA_DIR:-$ROOT/experiment/datasets/ldbc/sf1}"
PREPARED_CSV_DIR="${PREPARED_CSV_DIR:-$REPRO_DIR/prepared_csv}"
FORCE="${FORCE:-0}"

args=(
  --source-dir "$RAW_DATA_DIR"
  --output-dir "$PREPARED_CSV_DIR"
)
if [[ "$FORCE" == "1" ]]; then
  args+=(--force)
fi

python "$REPRO_DIR/scripts/00_prepare_csv.py" "${args[@]}"
echo "Prepared CSVs: $PREPARED_CSV_DIR"
