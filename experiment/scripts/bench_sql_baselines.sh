#!/usr/bin/env bash
#
# Benchmark SQL-based cardinality estimation tools on LDBC patterns.
#
# Unlike bench_all.sh, this script:
#   - Supports predicate patterns (*_P*.json) since SQL tools handle predicates
#   - Uses conda environments for each tool
#   - Outputs to a separate CSV for later merging
#
# Usage:
#   ./bench_sql_baselines.sh <sf> [pattern_dir]
#
# Output: experiment/result/comparison/comparison_sql_sf${SF}.csv

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXPERIMENT_DIR="$PROJECT_DIR/experiment"

SF=${1:?Usage: bench_sql_baselines.sh <sf> [pattern_dir]}
PATTERN_DIR="${2:-$EXPERIMENT_DIR/pattern/LDBC}"

SCHEMA="$EXPERIMENT_DIR/schemas/ldbc/ldbc_pathce_schema.json"
WRAPPER_DIR="$EXPERIMENT_DIR/scripts/sql_baselines"
TOOLS_DIR="$EXPERIMENT_DIR/tools"
TIMEOUT=300  # 5 minutes per query

RESULT_DIR="$EXPERIMENT_DIR/result/comparison"
mkdir -p "$RESULT_DIR"
OUTFILE="$RESULT_DIR/comparison_sql_sf${SF}.csv"

log() { echo "[$(date '+%H:%M:%S')] $*"; }

# CSV header
echo "sf,pattern_group,pattern,tool,cardinality,time_s,status" > "$OUTFILE"

append() {
    local group="$1" name="$2" tool="$3" card="$4" time_s="$5" status="$6"
    echo "$SF,$group,$name,$tool,$card,$time_s,$status" >> "$OUTFILE"
}

# Collect all patterns (including predicate variants)
PATTERNS=()
while IFS= read -r p; do
    PATTERNS+=("$p")
done < <(find "$PATTERN_DIR" -name "*.json" -type f | sort)

log "=== SQL Baselines Benchmark: sf${SF}, ${#PATTERNS[@]} patterns ==="

for p in "${PATTERNS[@]}"; do
    # Extract group/name from path
    rel="${p#$PATTERN_DIR/}"
    group="$(dirname "$rel")"
    name="$(basename "$rel" .json)"

    log "--- $group/$name ---"

    # ── FactorJoin ──
    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/factorjoin/sf${SF}"
    if [[ -f "$MODEL_DIR/factorjoin.pkl" ]]; then
        SQL=$(python3 "$TOOLS_DIR/minigu2sql.py" -p "$p" 2>/dev/null || echo "")
        if [[ -n "$SQL" ]]; then
            result=$(timeout "$TIMEOUT" conda run -n minigu-sql \
                python3 "$WRAPPER_DIR/run_factorjoin.py" estimate \
                    --model-dir "$MODEL_DIR" --query-sql "$SQL" 2>/dev/null || echo "")
            if [[ -n "$result" ]]; then
                IFS=',' read -r card time_s <<< "$result"
                append "$group" "$name" "factorjoin" "$card" "$time_s" "ok"
                log "  factorjoin: $group/$name -> card=$card time=$time_s (ok)"
            else
                append "$group" "$name" "factorjoin" "" "" "error"
                log "  factorjoin: $group/$name -> (error)"
            fi
        fi
    fi

    # ── BayesCard ──
    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/bayescard/sf${SF}"
    if [[ -f "$MODEL_DIR/bayescard.pkl" ]]; then
        result=$(timeout "$TIMEOUT" conda run -n minigu-sql \
            python3 "$WRAPPER_DIR/run_bayescard.py" estimate \
                --model-dir "$MODEL_DIR" --query-pattern "$p" 2>/dev/null || echo "")
        if [[ -n "$result" ]]; then
            IFS=',' read -r card time_s <<< "$result"
            append "$group" "$name" "bayescard" "$card" "$time_s" "ok"
            log "  bayescard: $group/$name -> card=$card time=$time_s (ok)"
        else
            append "$group" "$name" "bayescard" "" "" "error"
            log "  bayescard: $group/$name -> (error)"
        fi
    fi

    # ── FLAT ──
    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/flat/sf${SF}"
    if [[ -f "$MODEL_DIR/flat.pkl" ]]; then
        result=$(timeout "$TIMEOUT" conda run -n minigu-sql \
            python3 "$WRAPPER_DIR/run_flat.py" estimate \
                --model-dir "$MODEL_DIR" --query-pattern "$p" 2>/dev/null || echo "")
        if [[ -n "$result" ]]; then
            IFS=',' read -r card time_s <<< "$result"
            append "$group" "$name" "flat" "$card" "$time_s" "ok"
            log "  flat: $group/$name -> card=$card time=$time_s (ok)"
        else
            append "$group" "$name" "flat" "" "" "error"
            log "  flat: $group/$name -> (error)"
        fi
    fi

    # ── SafeBound ──
    MODEL_DIR="$EXPERIMENT_DIR/catalogs/ldbc/safebound/sf${SF}"
    if [[ -f "$MODEL_DIR/safebound.pkl" ]]; then
        result=$(timeout "$TIMEOUT" conda run -n minigu-safebound \
            python3 "$WRAPPER_DIR/run_safebound.py" estimate \
                --model-dir "$MODEL_DIR" --query "$p" 2>/dev/null || echo "")
        if [[ -n "$result" ]]; then
            IFS=',' read -r card time_s <<< "$result"
            append "$group" "$name" "safebound" "$card" "$time_s" "ok"
            log "  safebound: $group/$name -> card=$card time=$time_s (ok)"
        else
            append "$group" "$name" "safebound" "" "" "error"
            log "  safebound: $group/$name -> (error)"
        fi
    fi
done

log "Results saved to: $OUTFILE"
