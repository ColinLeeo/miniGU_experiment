#!/usr/bin/env bash
#
# Sweep: run bench_gcard_query.sh across all combinations of
#        max_k x max_graph x sample_size.
#
# max_k values    : 1 2 3
# max_graph values: 5 10
# sample_size     : 200 500
#
# For each max_k the matching statistic file is activated:
#   ldbc.statistic.bin.k{k}  →  ldbc.statistic.bin
#
# Usage:
#   ./sweep_gcard_query.sh [SF...]     # default SF: sf1
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BENCH="$SCRIPT_DIR/bench_gcard_query.sh"

DB_BASE="$PROJECT_DIR/experiment/dataset/ldbc"

MAX_K_VALUES=(1 2 3)
MAX_GRAPH_VALUES=(5 10)
SAMPLE_SIZE_VALUES=(200 500)

# Scale factors from CLI args, default sf1
if [[ $# -eq 0 ]]; then
    SCALE_FACTORS=(sf1)
else
    SCALE_FACTORS=("$@")
fi

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

# Sanity checks
if [[ ! -x "$BENCH" ]]; then
    echo "ERROR: bench script not found at $BENCH"
    exit 1
fi

for SF in "${SCALE_FACTORS[@]}"; do
    DB_PATH="$DB_BASE/$SF/minigu_db"
    if [[ ! -d "$DB_PATH" ]]; then
        echo "ERROR: database not found at $DB_PATH"
        exit 1
    fi

    for K in "${MAX_K_VALUES[@]}"; do
        STAT_SRC="$DB_PATH/ldbc.statistic.bin.k${K}"
        STAT_DST="$DB_PATH/ldbc.statistic.bin"

        if [[ ! -f "$STAT_SRC" ]]; then
            log "SKIP k=$K for $SF: $STAT_SRC not found"
            continue
        fi

        log "=== Activating statistic for k=$K ($SF) ==="
        cp "$STAT_SRC" "$STAT_DST"

        for MG in "${MAX_GRAPH_VALUES[@]}"; do
            for SS in "${SAMPLE_SIZE_VALUES[@]}"; do
                log "--- k=$K  max_graph=$MG  sample_size=$SS  sf=$SF ---"
                "$BENCH" --max-k "$K" --max-graph "$MG" --sample-size "$SS" "$SF"
            done
        done
    done
done

log "Sweep complete."
