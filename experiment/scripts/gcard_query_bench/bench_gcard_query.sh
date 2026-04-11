#!/usr/bin/env bash
#
# Benchmark: gcard_query across all LDBC patterns and scale factors.
#
# Usage:
#   ./bench_gcard_query.sh [OPTIONS] [SF...]
#
# Options:
#   --max-k K           Maximum k for gcard_query (default: 3)
#   --max-graph N       Maximum number of spanning trees (default: 5)
#   --sample-size N     Sample size (default: 500)
#   --query-timeout S   Per-query timeout in seconds, 0 = no limit (default: 0)
#
# Examples:
#   ./bench_gcard_query.sh                               # default: sf1, k=3, max_graph=5
#   ./bench_gcard_query.sh sf0.1 sf1                     # specify scale factors
#   ./bench_gcard_query.sh --max-k 2 sf1                 # k=2
#   ./bench_gcard_query.sh --max-k 3 --max-graph 3 sf1
#   ./bench_gcard_query.sh --query-timeout 120 sf1       # kill each query after 120s, record as timeout
#
# Before running:
#   1. Each SF should have imported graph + built catalog (statistic.bin)
#   2. Build minigu in release mode: cargo build --release --bin=minigu
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

MINIGU="$PROJECT_DIR/target/release/minigu"
PATTERN_DIR="$PROJECT_DIR/experiment/pattern/LDBC"

GRAPH_NAME="ldbc"
MAX_K=3
MAX_GRAPH=5
SAMPLE_SIZE=500
SCALE_FACTORS=()

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --max-k)
            MAX_K="$2"; shift 2 ;;
        --max-graph)
            MAX_GRAPH="$2"; shift 2 ;;
        --sample-size)
            SAMPLE_SIZE="$2"; shift 2 ;;
        --*)
            echo "ERROR: Unknown option $1"; exit 1 ;;
        *)
            SCALE_FACTORS+=("$1"); shift ;;
    esac
done

# Default scale factor if none specified
if [[ ${#SCALE_FACTORS[@]} -eq 0 ]]; then
    SCALE_FACTORS=(sf1)
fi

RESULT_DIR="$PROJECT_DIR/experiment/result/gcard_query"
mkdir -p "$RESULT_DIR"
OUTPUT_CSV="$RESULT_DIR/gcard_query_results_k${MAX_K}_mg${MAX_GRAPH}_ss${SAMPLE_SIZE}.csv"

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

# Check binary
if [[ ! -x "$MINIGU" ]]; then
    echo "ERROR: minigu binary not found at $MINIGU"
    echo "Run: cargo build --release --bin=minigu"
    exit 1
fi

# Collect all pattern JSON files
PATTERNS=()
while IFS= read -r f; do
    PATTERNS+=("$f")
done < <(find "$PATTERN_DIR" -name '*.json' -type f | sort)

log "Starting gcard_query benchmark"
log "Scale factors: ${SCALE_FACTORS[*]}"
log "Patterns: ${#PATTERNS[@]}"
log "max_k=$MAX_K  max_graph=$MAX_GRAPH  sample_size=$SAMPLE_SIZE"
log "Output: $OUTPUT_CSV"

# CSV header
echo "sf,pattern_group,pattern,cardinality,sample_time_s,estimate_time_s,build_time_s,total_time_s" > "$OUTPUT_CSV"

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

for SF in "${SCALE_FACTORS[@]}"; do
    DB_PATH="$PROJECT_DIR/experiment/dataset/ldbc/$SF/minigu_db"

    if [[ ! -d "$DB_PATH" ]]; then
        log "SKIP $SF: database not found at $DB_PATH"
        continue
    fi

    log "=== Scale Factor: $SF ==="

    # Build a single GQL script for this SF: load graph + catalog once, run all queries
    script_file="$TMP_DIR/bench_${SF}.gql"
    cat > "$script_file" <<EOGQL
session set graph ${GRAPH_NAME}
call load_catalog("${GRAPH_NAME}")
EOGQL

    for pattern_path in "${PATTERNS[@]}"; do
        echo ":time call gcard_query(\"${pattern_path}\", ${MAX_K}, ${SAMPLE_SIZE}, 0, false, ${MAX_GRAPH})" >> "$script_file"
    done

    # Run single minigu process for all queries in this SF
    stdout_file="$TMP_DIR/out_${SF}.txt"
    log "  Running ${#PATTERNS[@]} queries in one process..."
    "$MINIGU" execute "$script_file" --path "$DB_PATH" 2>&1 | tee "$stdout_file" || true

    # Parse results: each query produces a line containing:
    #   total: N, min_es index: ..., sample_time: X.XXXXXX, estimate_time: Y.YYYYYY, pattern, cardinality: C
    # followed by a Time: line from :time

    cardinalities=()
    while IFS= read -r line; do
        cardinalities+=("$line")
    done < <(grep 'cardinality:' "$stdout_file" | grep -o 'cardinality: [0-9]*' | awk '{print $2}')

    sample_times=()
    while IFS= read -r line; do
        sample_times+=("$line")
    done < <(grep -o 'sample_time: [0-9.]*' "$stdout_file" | awk '{print $2}')

    estimate_times=()
    while IFS= read -r line; do
        estimate_times+=("$line")
    done < <(grep -o 'estimate_time: [0-9.]*' "$stdout_file" | awk '{print $2}')

    build_times=()
    while IFS= read -r line; do
        build_times+=("$line")
    done < <(grep -o 'build_time: [0-9.]*' "$stdout_file" | awk '{print $2}')

    total_times=()
    while IFS= read -r line; do
        total_times+=("$line")
    done < <(grep -o 'Time: [0-9.]*s' "$stdout_file" | grep -o '[0-9.]*')

    # Map results back to pattern order
    idx=0
    for pattern_path in "${PATTERNS[@]}"; do
        rel_path="${pattern_path#$PATTERN_DIR/}"
        group="$(dirname "$rel_path")"
        pattern="$(basename "$rel_path" .json)"

        cardinality=0
        sample_t=0
        estimate_t=0
        build_t=0
        total_t=0

        if [[ $idx -lt ${#cardinalities[@]} ]]; then
            cardinality="${cardinalities[$idx]}"
        fi
        if [[ $idx -lt ${#sample_times[@]} ]]; then
            sample_t="${sample_times[$idx]}"
        fi
        if [[ $idx -lt ${#estimate_times[@]} ]]; then
            estimate_t="${estimate_times[$idx]}"
        fi
        if [[ $idx -lt ${#build_times[@]} ]]; then
            build_t="${build_times[$idx]}"
        fi
        if [[ $idx -lt ${#total_times[@]} ]]; then
            total_t="${total_times[$idx]}"
        fi

        echo "$SF,$group,$pattern,$cardinality,$sample_t,$estimate_t,$build_t,$total_t" >> "$OUTPUT_CSV"
        log "  $group/$pattern -> cardinality=$cardinality sample=${sample_t}s estimate=${estimate_t}s build=${build_t}s total=${total_t}s"

        idx=$((idx + 1))
    done
done

log "Benchmark complete. Results saved to $OUTPUT_CSV"
echo ""
echo "=== Results ==="
column -t -s',' "$OUTPUT_CSV"
