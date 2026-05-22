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

# Train alias-aware 2path BNs for LSQB+GLogs SQLs.
PREPARED_CSV_DIR="${PREPARED_CSV_DIR:-$REPRO_DIR/prepared_csv}"
QUERY_DIR="${QUERY_DIR:-$ROOT/experiment/result/sql_baselines/lsqb_glogs_with_pred_10_unique/flat_sql}"
HDF_DIR="${HDF_DIR:-$REPRO_DIR/hdf_2path_alias_local}"
MODEL_DIR="${MODEL_DIR:-$REPRO_DIR/models_2path_alias_local}"
RESULT_DIR="${RESULT_DIR:-$REPRO_DIR/results}"
WORKLIST="${WORKLIST:-$RESULT_DIR/ldbc_2path_alias_local_worklist.csv}"
START_INDEX="${START_INDEX:-0}"
END_INDEX="${END_INDEX:-}"
SAMPLE_SIZE="${SAMPLE_SIZE:-10000}"
JOIN_SAMPLE_SIZE="${JOIN_SAMPLE_SIZE:-50000}"
MAX_TABLE_DATA="${MAX_TABLE_DATA:-20000000}"
OVERWRITE="${OVERWRITE:-0}"

mkdir -p "$RESULT_DIR"

python "$REPRO_DIR/scripts/01_generate_hdf_2path_alias.py"   --data-dir "$PREPARED_CSV_DIR"   --query-dir "$QUERY_DIR"   --hdf-dir "$HDF_DIR"   --worklist "$WORKLIST"   --max-table-data "$MAX_TABLE_DATA"

args=(
  --data-dir "$PREPARED_CSV_DIR"
  --query-dir "$QUERY_DIR"
  --hdf-dir "$HDF_DIR"
  --model-dir "$MODEL_DIR"
  --worklist "$WORKLIST"
  --sample-size "$SAMPLE_SIZE"
  --join-sample-size "$JOIN_SAMPLE_SIZE"
  --start-index "$START_INDEX"
)
if [[ -n "$END_INDEX" ]]; then
  args+=(--end-index "$END_INDEX")
fi
if [[ "$OVERWRITE" == "1" ]]; then
  args+=(--overwrite)
fi

python "$REPRO_DIR/scripts/02_train_bn_2path_alias.py" "${args[@]}"
echo "Models: $MODEL_DIR"
echo "Worklist: $WORKLIST"
