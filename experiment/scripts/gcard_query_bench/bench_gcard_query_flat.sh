#!/usr/bin/env bash
#
# Benchmark: gcard_query (FlatGraph path) across all LDBC patterns and scale factors.
#
# Uses load_ldbc instead of load_catalog so queries run entirely without
# MemoryGraph / MVCC — FlatGraph is the only graph data structure in memory.
#
# Usage:
#   ./bench_gcard_query_flat.sh [OPTIONS] [SF...]
#
# Options:
#   --max-k K           Maximum k for gcard_query (default: 3)
#   --max-graph N       Maximum number of spanning trees (default: 5)
#   --sample-size N     Sample size (default: 500)
#   --query-timeout S   Per-query timeout in seconds, 0 = no limit (default: 0)
#
# Examples:
#   ./bench_gcard_query_flat.sh sf0.1
#   ./bench_gcard_query_flat.sh sf0.1 sf1
#   ./bench_gcard_query_flat.sh --max-k 2 sf0.1
#
# Before running:
#   Build minigu in release mode: cargo build --release --bin=minigu
#   (No prior import_graph / GCard_build needed — load_ldbc builds everything.)
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
QUERY_TIMEOUT=0
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
        --query-timeout)
            QUERY_TIMEOUT="$2"; shift 2 ;;
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
OUTPUT_CSV="$RESULT_DIR/gcard_query_flat_results_k${MAX_K}_mg${MAX_GRAPH}_ss${SAMPLE_SIZE}.csv"

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

log "Starting gcard_query FlatGraph benchmark"
log "Scale factors: ${SCALE_FACTORS[*]}"
log "Patterns: ${#PATTERNS[@]}"
log "max_k=$MAX_K  max_graph=$MAX_GRAPH  sample_size=$SAMPLE_SIZE"
log "Output: $OUTPUT_CSV"

# CSV header
echo "sf,pattern_group,pattern,cardinality,sample_time_s,estimate_time_s,build_time_s,total_time_s,cycle_check_s,score_tree_s,pivot_path_s,abstract_edge_s,pcf_lookup_s" > "$OUTPUT_CSV"

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

for SF in "${SCALE_FACTORS[@]}"; do
    CSV_DIR="$PROJECT_DIR/experiment/dataset/ldbc/$SF"
    MANIFEST="$CSV_DIR/manifest.json"

    if [[ ! -f "$MANIFEST" ]]; then
        log "SKIP $SF: manifest.json not found at $MANIFEST"
        continue
    fi

    # Use a fresh temporary db directory so load_ldbc can register the graph cleanly.
    DB_PATH="$TMP_DIR/db_${SF}"
    mkdir -p "$DB_PATH"

    log "=== Scale Factor: $SF ==="
    log "  CSV dir : $CSV_DIR"
    log "  Temp DB : $DB_PATH"

    # Build a single GQL script: load_ldbc once, then run all queries
    script_file="$TMP_DIR/bench_flat_${SF}.gql"
    cat > "$script_file" <<EOGQL
call load_ldbc("${CSV_DIR}", "${GRAPH_NAME}", ${MAX_K})
session set graph ${GRAPH_NAME}
EOGQL

    for pattern_path in "${PATTERNS[@]}"; do
        if [[ "$QUERY_TIMEOUT" -gt 0 ]]; then
            echo ":time call gcard_query(\"${pattern_path}\", ${MAX_K}, ${SAMPLE_SIZE}, 0, false, ${MAX_GRAPH})" >> "$script_file"
        else
            echo ":time call gcard_query(\"${pattern_path}\", ${MAX_K}, ${SAMPLE_SIZE}, 0, false, ${MAX_GRAPH})" >> "$script_file"
        fi
    done

    # Run single minigu process for all queries in this SF
    stdout_file="$TMP_DIR/out_flat_${SF}.txt"
    log "  Running ${#PATTERNS[@]} queries in one process..."

    if [[ "$QUERY_TIMEOUT" -gt 0 ]]; then
        timeout "$QUERY_TIMEOUT" "$MINIGU" execute "$script_file" --path "$DB_PATH" 2>&1 | tee "$stdout_file" || true
    else
        "$MINIGU" execute "$script_file" --path "$DB_PATH" 2>&1 | tee "$stdout_file" || true
    fi

    # Parse results
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

    cycle_check_times=()
    while IFS= read -r line; do
        cycle_check_times+=("$line")
    done < <(grep '\[build-prof\]' "$stdout_file" | grep -o 'cycle_check: [0-9.]*' | awk '{print $2}')

    score_tree_times=()
    while IFS= read -r line; do
        score_tree_times+=("$line")
    done < <(grep '\[build-prof\]' "$stdout_file" | grep -o 'score+tree_enum: [0-9.]*' | awk '{print $2}')

    pivot_path_times=()
    while IFS= read -r line; do
        pivot_path_times+=("$line")
    done < <(grep '\[build-prof\]' "$stdout_file" | grep -o 'pivot+path: [0-9.]*' | awk '{print $2}')

    abstract_edge_times=()
    while IFS= read -r line; do
        abstract_edge_times+=("$line")
    done < <(grep '\[build-prof\]' "$stdout_file" | grep -o 'abstract_edge: [0-9.]*' | awk '{print $2}')

    pcf_lookup_times=()
    while IFS= read -r line; do
        pcf_lookup_times+=("$line")
    done < <(grep '\[build-prof\]' "$stdout_file" | grep -o 'pcf_lookup: [0-9.]*' | awk '{print $2}')

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
        cycle_t=0
        score_tree_t=0
        pivot_path_t=0
        abstract_edge_t=0
        pcf_lookup_t=0

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
        if [[ $idx -lt ${#cycle_check_times[@]} ]]; then
            cycle_t="${cycle_check_times[$idx]}"
        fi
        if [[ $idx -lt ${#score_tree_times[@]} ]]; then
            score_tree_t="${score_tree_times[$idx]}"
        fi
        if [[ $idx -lt ${#pivot_path_times[@]} ]]; then
            pivot_path_t="${pivot_path_times[$idx]}"
        fi
        if [[ $idx -lt ${#abstract_edge_times[@]} ]]; then
            abstract_edge_t="${abstract_edge_times[$idx]}"
        fi
        if [[ $idx -lt ${#pcf_lookup_times[@]} ]]; then
            pcf_lookup_t="${pcf_lookup_times[$idx]}"
        fi

        echo "$SF,$group,$pattern,$cardinality,$sample_t,$estimate_t,$build_t,$total_t,$cycle_t,$score_tree_t,$pivot_path_t,$abstract_edge_t,$pcf_lookup_t" >> "$OUTPUT_CSV"
        log "  $group/$pattern -> cardinality=$cardinality sample=${sample_t}s estimate=${estimate_t}s build=${build_t}s total=${total_t}s | cycle=${cycle_t}s score_tree=${score_tree_t}s pivot_path=${pivot_path_t}s abstract_edge=${abstract_edge_t}s pcf_lookup=${pcf_lookup_t}s"

        idx=$((idx + 1))
    done
done

log "Benchmark complete. Results saved to $OUTPUT_CSV"
echo ""
echo "=== Results ==="
column -t -s',' "$OUTPUT_CSV"
