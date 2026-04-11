#!/usr/bin/env bash
#
# Prepare SQL-based cardinality estimation baselines for LDBC.
#
# Usage:
#   ./prepare_sql_baselines.sh <sf> [tools...]
#
# Examples:
#   ./prepare_sql_baselines.sh 1                              # all tools
#   ./prepare_sql_baselines.sh 1 factorjoin safebound         # specific tools
#
# Conda environments (only 2 needed):
#
#   minigu-sql: FactorJoin + BayesCard + FLAT (same author, same deps)
#     conda create -p /mnt/sdb1/conda_envs/minigu-sql python=3.7 -y
#     pip install numpy==1.21.6 pandas==1.3.5 scipy==1.7.3 scikit-learn==1.0.2
#     pip install pgmpy==0.1.13 pomegranate==0.11.1 networkx==2.6.3
#     pip install torch==1.7.1+cpu -f https://download.pytorch.org/whl/torch_stable.html
#     pip install joblib tqdm sqlparse jenkspy psycopg2-binary spflow==0.0.34
#
#   minigu-safebound: SafeBound (different deps)
#     conda create -p /mnt/sdb1/conda_envs/minigu-safebound python=3.10 -y
#     pip install numpy pandas scipy cython==0.29.30 scikit-learn matplotlib

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXPERIMENT_DIR="$PROJECT_DIR/experiment"

SF=${1:?Usage: prepare_sql_baselines.sh <sf> [tools...]}
shift
if [[ $# -eq 0 ]]; then
    TOOLS=(factorjoin bayescard flat safebound)
else
    TOOLS=("$@")
fi

DATASET_DIR="$EXPERIMENT_DIR/dataset/ldbc/sf${SF}"
SCHEMA="$EXPERIMENT_DIR/schemas/ldbc/ldbc_pathce_schema.json"
WRAPPER_DIR="$EXPERIMENT_DIR/scripts/sql_baselines"

# Shared conda env for FactorJoin/BayesCard/FLAT
SQL_ENV="minigu-sql"
SB_ENV="minigu-safebound"

log() { echo "[$(date '+%H:%M:%S')] $*"; }

should_run() {
    for t in "${TOOLS[@]}"; do
        [[ "$t" == "$1" ]] && return 0
    done
    return 1
}

if [[ ! -d "$DATASET_DIR" ]]; then
    log "ERROR: dataset not found: $DATASET_DIR"
    exit 1
fi

# ── FactorJoin ────────────────────────────────────────────────────────────
if should_run factorjoin; then
    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/factorjoin/sf${SF}"
    MARKER="$MODEL_DIR/factorjoin.pkl"
    if [[ ! -f "$MARKER" ]]; then
        log "factorjoin: training model for sf${SF}..."
        conda run -n "$SQL_ENV" python3 "$WRAPPER_DIR/run_factorjoin.py" build \
            --data-dir "$DATASET_DIR" \
            --model-dir "$MODEL_DIR"
    else
        log "factorjoin: model already exists: $MARKER"
    fi
fi

# ── BayesCard ─────────────────────────────────────────────────────────────
if should_run bayescard; then
    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/bayescard/sf${SF}"
    MARKER="$MODEL_DIR/bayescard.pkl"
    if [[ ! -f "$MARKER" ]]; then
        log "bayescard: training model for sf${SF}..."
        conda run -n "$SQL_ENV" python3 "$WRAPPER_DIR/run_bayescard.py" build \
            --data-dir "$DATASET_DIR" \
            --model-dir "$MODEL_DIR"
    else
        log "bayescard: model already exists: $MARKER"
    fi
fi

# ── FLAT ──────────────────────────────────────────────────────────────────
if should_run flat; then
    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/flat/sf${SF}"
    MARKER="$MODEL_DIR/flat.pkl"
    if [[ ! -f "$MARKER" ]]; then
        log "flat: training model for sf${SF}..."
        conda run -n "$SQL_ENV" python3 "$WRAPPER_DIR/run_flat.py" build \
            --data-dir "$DATASET_DIR" \
            --model-dir "$MODEL_DIR"
    else
        log "flat: model already exists: $MARKER"
    fi
fi

# ── SafeBound ─────────────────────────────────────────────────────────────
if should_run safebound; then
    # SafeBound needs Cython build first
    SB_SRC="$EXPERIMENT_DIR/baseline/safebound/Source"
    if ! ls "$SB_SRC"/SafeBoundUtils.cpython*.so &>/dev/null; then
        log "safebound: building Cython extensions..."
        conda run -n "$SB_ENV" bash -c "cd '$SB_SRC' && python CythonBuild.py build_ext --inplace"
    fi

    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/safebound/sf${SF}"
    MARKER="$MODEL_DIR/safebound.pkl"
    if [[ ! -f "$MARKER" ]]; then
        log "safebound: building statistics for sf${SF}..."
        conda run -n "$SB_ENV" python3 "$WRAPPER_DIR/run_safebound.py" build \
            --data-dir "$DATASET_DIR" \
            --schema "$SCHEMA" \
            --model-dir "$MODEL_DIR"
    else
        log "safebound: statistics already exist: $MARKER"
    fi
fi

log "SQL baselines preparation complete for sf${SF}."
