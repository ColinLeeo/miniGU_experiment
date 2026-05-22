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

# Estimate the 16 LSQB+GLogs no-predicate SQLs.
PREPARED_CSV_DIR="${PREPARED_CSV_DIR:-$REPRO_DIR/prepared_csv}"
MODEL_DIR="${MODEL_DIR:-$REPRO_DIR/models_2path_alias_local}"
QUERY_DIR="${QUERY_DIR:-$ROOT/experiment/result/sql_baselines/lsqb_glogs_no_pred_2path_bn/flat_sql}"
OUTPUT="${OUTPUT:-$ROOT/experiment/result/sql_baselines/lsqb_glogs_no_pred_2path_bn/bayescard/estimate.csv}"

mkdir -p "$(dirname "$OUTPUT")"
python "$REPRO_DIR/scripts/06_eval_ldbc_with_pred.py"   --alias-2path   --data-dir "$PREPARED_CSV_DIR"   --model-dir "$MODEL_DIR"   --query-dir "$QUERY_DIR"   --output "$OUTPUT"
echo "Output: $OUTPUT"
