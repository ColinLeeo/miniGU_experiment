#!/usr/bin/env bash
#
# Benchmark: create_catalog build time & statistic size across LDBC scale factors.
#
# Tests different scale factors and thread counts, capturing:
#   - Catalog build time (seconds)
#   - Statistic serialized size (MB)
#   - Compressed statistic size (MB)
#   - Peak RSS memory (MB)
#
# Usage:
#   ./bench_catalog.sh [--threads "1 2 4 8"] [--sfs "sf0.1 sf0.3 sf1"] [--repeats 3] [--max-k 2]
#
# Before running:
#   1. Prepare database directories: experiment/datasets/ldbc/<sf>/minigu_db/
#   2. Each should contain an already-imported LDBC graph named "ldbc".
#   3. Build minigu: cargo build -r --features std,serde,miette
#

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

MINIGU="$PROJECT_DIR/target/release/minigu"
DB_BASE_DIR="$PROJECT_DIR/experiment/datasets/ldbc"
GRAPH_NAME="ldbc"
MAX_K=2
SCALE_FACTORS="sf3"
THREAD_COUNTS="4 8 16 32"
REPEATS=3

# ============================================================================
# Parse arguments
# ============================================================================
while [[ $# -gt 0 ]]; do
    case "$1" in
        --threads)  THREAD_COUNTS="$2"; shift 2 ;;
        --sfs)      SCALE_FACTORS="$2"; shift 2 ;;
        --repeats)  REPEATS="$2";       shift 2 ;;
        --max-k)    MAX_K="$2";         shift 2 ;;
        --binary)   MINIGU="$2";        shift 2 ;;
        --graph)    GRAPH_NAME="$2";    shift 2 ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--threads \"1 2 4 8\"] [--sfs \"sf0.1 sf0.3\"] [--repeats 3] [--max-k 2]"
            exit 1
            ;;
    esac
done

# ============================================================================
# Setup
# ============================================================================
RESULT_DIR="$PROJECT_DIR/experiment/result/build_catalog"
mkdir -p "$RESULT_DIR"

OUTPUT_CSV="$RESULT_DIR/bench_catalog_results.csv"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

# ============================================================================
# Validation
# ============================================================================
if [[ ! -x "$MINIGU" ]]; then
    echo "ERROR: minigu binary not found at $MINIGU"
    echo "Run: cargo build -r --features std,serde,miette"
    exit 1
fi

# ============================================================================
# Main
# ============================================================================
log "=== Catalog Build Benchmark ==="
log "Binary:        $MINIGU"
log "DB base dir:   $DB_BASE_DIR"
log "Graph:         $GRAPH_NAME"
log "Max K:         $MAX_K"
log "Scale factors: $SCALE_FACTORS"
log "Thread counts: $THREAD_COUNTS"
log "Repeats:       $REPEATS"
log ""

# CSV header
echo "sf,threads,repeat,scan_time_s,compute_time_s,build_time_s,stat_size_mb,compressed_size_mb,peak_mem_mb" > "$OUTPUT_CSV"

for sf in $SCALE_FACTORS; do
    db_path="$DB_BASE_DIR/$sf/minigu_db"
    if [[ ! -d "$db_path" ]]; then
        log "SKIP $sf: directory $db_path not found"
        continue
    fi

    log "=== Scale Factor: $sf ==="

    # Generate a single GQL script with all thread configs and repeats.
    # Graph is loaded once; create_catalog is called for each combination.
    script_file="$TMP_DIR/bench_${sf}_all.gql"
    cat > "$script_file" <<EOGQL
session set graph ${GRAPH_NAME}
EOGQL

    for threads in $THREAD_COUNTS; do
        for repeat in $(seq 1 "$REPEATS"); do
            echo "call create_catalog(\"${GRAPH_NAME}\", ${MAX_K}, ${threads}, ${threads})" >> "$script_file"
        done
    done

    # Run minigu once per scale factor and capture output + peak memory.
    stdout_file="$TMP_DIR/stdout_${sf}.txt"
    time_file="$TMP_DIR/time_${sf}.txt"

    time_flag="-l"
    if [[ "$(uname)" == "Linux" ]]; then
        time_flag="-v"
    fi

    log "  Running all configs in a single process (graph loaded once)..."
    /usr/bin/time $time_flag "$MINIGU" execute "$script_file" --path "$db_path" \
        > "$stdout_file" 2> "$time_file" || {
        log "    FAILED (exit code $?)"
        cat "$stdout_file"
        for threads in $THREAD_COUNTS; do
            for repeat in $(seq 1 "$REPEATS"); do
                echo "$sf,$threads,$repeat,ERROR,ERROR,ERROR,,,," >> "$OUTPUT_CSV"
            done
        done
        continue
    }

    # Parse peak memory from /usr/bin/time (one value per sf)
    if [[ "$(uname)" == "Linux" ]]; then
        peak_kb=$(grep "Maximum resident set size" "$time_file" | awk '{print $NF}') || peak_kb=0
        peak_mb=$(python3 -c "print(round($peak_kb / 1024, 2))")
    else
        peak_bytes=$(grep "maximum resident set size" "$time_file" | awk '{print $1}') || peak_bytes=0
        peak_mb=$(python3 -c "print(round($peak_bytes / 1024 / 1024, 2))")
    fi

    # Parse results: each create_catalog call prints timing and size info.
    # Extract the idx-th occurrence of each metric.
    idx=0
    for threads in $THREAD_COUNTS; do
        for repeat in $(seq 1 "$REPEATS"); do
            idx=$((idx + 1))

            scan_time=$(grep -o 'Scan time: [0-9.]*s' "$stdout_file" \
                | sed -n "${idx}p" | grep -o '[0-9.]*') || scan_time="N/A"

            compute_time=$(grep -o 'Compute time: [0-9.]*s' "$stdout_file" \
                | sed -n "${idx}p" | grep -o '[0-9.]*') || compute_time="N/A"

            build_time=$(grep -o 'Catalog build time: [0-9.]*s' "$stdout_file" \
                | sed -n "${idx}p" | grep -o '[0-9.]*') || build_time="N/A"

            stat_bytes=$(grep "Statistic serialized size:" "$stdout_file" \
                | sed -n "${idx}p" | grep -oE '[0-9]+ bytes' | awk '{print $1}') || stat_bytes=0
            stat_mb=$(python3 -c "print(round(${stat_bytes:-0} / 1024 / 1024, 2))" 2>/dev/null || echo "0")

            comp_bytes=$(grep "Compressed statistic size:" "$stdout_file" \
                | sed -n "${idx}p" | grep -oE '[0-9]+') || comp_bytes=0
            comp_mb=$(python3 -c "print(round(${comp_bytes:-0} / 1024 / 1024, 2))" 2>/dev/null || echo "0")

            echo "$sf,$threads,$repeat,$scan_time,$compute_time,$build_time,$stat_mb,$comp_mb,$peak_mb" >> "$OUTPUT_CSV"
            log "    threads=$threads repeat=$repeat -> scan=${scan_time}s  compute=${compute_time}s  total=${build_time}s  stat=${stat_mb}MB  compressed=${comp_mb}MB"
        done
    done
    log "  peak_mem=${peak_mb}MB (for entire $sf run)"
done

log ""
log "=== Results ==="
column -t -s',' "$OUTPUT_CSV"
log ""
log "Results saved to $OUTPUT_CSV"
