#!/usr/bin/env bash
#
# Benchmark: create_catalog 构建时间（仅度序列计算，不含压缩/落盘）。
# 针对同一数据库，测试不同线程数配置下的构建耗时。
#
# Usage:
#   ./bench_build_time.sh --db <db_path> [--graph <name>] [--max-k <k>] [--threads "1 2 4 8"] [--repeats <n>]
#
# Example:
#   ./bench_build_time.sh --db experiment/dataset/ldbc/sf1/minigu_db --graph ldbc
#

set -euo pipefail

# ============================================================================
# Defaults
# ============================================================================
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

MINIGU="$PROJECT_DIR/target/release/minigu"
DB_PATH=""
GRAPH_NAME="ldbc"
MAX_K=2
THREAD_COUNTS="1 2 4 8 16"
REPEATS=3

# ============================================================================
# Parse arguments
# ============================================================================
while [[ $# -gt 0 ]]; do
    case "$1" in
        --db)       DB_PATH="$2";       shift 2 ;;
        --graph)    GRAPH_NAME="$2";    shift 2 ;;
        --max-k)    MAX_K="$2";         shift 2 ;;
        --threads)  THREAD_COUNTS="$2"; shift 2 ;;
        --repeats)  REPEATS="$2";       shift 2 ;;
        --binary)   MINIGU="$2";        shift 2 ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 --db <db_path> [--graph <name>] [--max-k <k>] [--threads \"1 2 4 8\"] [--repeats <n>]"
            exit 1
            ;;
    esac
done

if [[ -z "$DB_PATH" ]]; then
    echo "ERROR: --db <db_path> is required"
    exit 1
fi

# Resolve to absolute path
DB_PATH="$(cd "$DB_PATH" && pwd)"

# ============================================================================
# Validation
# ============================================================================
if [[ ! -x "$MINIGU" ]]; then
    echo "ERROR: minigu binary not found at $MINIGU"
    echo "Run: cargo build -r --features std,serde,miette"
    exit 1
fi

if [[ ! -d "$DB_PATH" ]]; then
    echo "ERROR: database directory not found: $DB_PATH"
    exit 1
fi

# ============================================================================
# Setup
# ============================================================================
RESULT_DIR="$PROJECT_DIR/experiment/result/build_time"
mkdir -p "$RESULT_DIR"

OUTPUT_CSV="$RESULT_DIR/bench_build_time.csv"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

# ============================================================================
# Main
# ============================================================================
log "=== Build Time Benchmark ==="
log "Binary:    $MINIGU"
log "DB path:   $DB_PATH"
log "Graph:     $GRAPH_NAME"
log "Max K:     $MAX_K"
log "Threads:   $THREAD_COUNTS"
log "Repeats:   $REPEATS"
log ""

# CSV header
echo "compute_threads,scan_threads,repeat,build_time_s,scan_time_s,compute_time_s" > "$OUTPUT_CSV"

# Generate a single GQL script with all thread configs and repeats.
# This avoids reloading the graph for every run.
script_file="$TMP_DIR/bench_all.gql"
cat > "$script_file" <<EOGQL
session set graph ${GRAPH_NAME}
EOGQL

for threads in $THREAD_COUNTS; do
    for repeat in $(seq 1 "$REPEATS"); do
        scan_threads=$threads
        echo "call create_catalog(\"${GRAPH_NAME}\", ${MAX_K}, ${threads}, ${scan_threads})" >> "$script_file"
    done
done

log "Running all configs in a single process (graph loaded once)..."
stdout_file="$TMP_DIR/stdout_all.txt"
"$MINIGU" execute "$script_file" --path "$DB_PATH" > "$stdout_file" 2>&1 || {
    log "FAILED (exit code $?)"
    cat "$stdout_file"
    exit 1
}

# Parse results: each create_catalog call prints "compute_threads=X, scan_threads=Y" and
# "Catalog build time: X.XXXs (scan: X.XXXs + compute: X.XXXs)"
idx=0
for threads in $THREAD_COUNTS; do
    for repeat in $(seq 1 "$REPEATS"); do
        idx=$((idx + 1))
        scan_threads=$threads

        # Extract the idx-th occurrence of build time
        build_time=$(grep -o 'Catalog build time: [0-9.]*s' "$stdout_file" | sed -n "${idx}p" | grep -o '[0-9.]*') || build_time="N/A"
        scan_time=$(grep -o 'scan: [0-9.]*s' "$stdout_file" | sed -n "${idx}p" | grep -o '[0-9.]*') || scan_time="N/A"
        compute_time=$(grep -o 'compute: [0-9.]*s' "$stdout_file" | sed -n "${idx}p" | grep -o '[0-9.]*') || compute_time="N/A"

        echo "$threads,$scan_threads,$repeat,$build_time,$scan_time,$compute_time" >> "$OUTPUT_CSV"
        log "  threads=$threads repeat=$repeat -> total=${build_time}s (scan=${scan_time}s compute=${compute_time}s)"
    done
done

log ""
log "=== Results ==="
column -t -s',' "$OUTPUT_CSV"
log ""
log "Results saved to $OUTPUT_CSV"
