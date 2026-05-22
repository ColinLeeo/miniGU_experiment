#!/usr/bin/env bash
#
# Benchmark: create_catalog build time for large LDBC datasets (sf10+).
#
# Uses load_ldbc to build a ScannedHops bincode (one-time), then benchmarks
# create_catalog with different thread counts in a SINGLE process.
# The graph and scanned_hops are loaded once; all thread configs reuse the cache.
#
# Tests different thread counts, capturing:
#   - Scan time (seconds)       — scan_all_hops (or bincode load)
#   - Compute time (seconds)    — compute_from_scanned_hops
#   - Total build time (seconds)
#   - Statistic serialized size (MB)
#   - Peak RSS memory (MB)
#
# Usage:
#   ./bench_catalog_sf10.sh --dataset /path/to/sf10 [--threads "1 2 4 8"] [--repeats 3] [--max-k 2]
#
# Before running:
#   1. Prepare LDBC CSV dataset with manifest.json (e.g. experiment/datasets/ldbc/sf10/)
#   2. Build minigu: cargo build -r --features std,serde,miette
#

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

MINIGU="$PROJECT_DIR/target/release/minigu"
GRAPH_NAME="ldbc"
MAX_K=2
DATASET_DIR=""
THREAD_COUNTS="1 2 4 8 16 32"
REPEATS=3
SKIP_PREPARE=false

# ============================================================================
# Parse arguments
# ============================================================================
while [[ $# -gt 0 ]]; do
    case "$1" in
        --threads)      THREAD_COUNTS="$2"; shift 2 ;;
        --dataset)      DATASET_DIR="$2";   shift 2 ;;
        --repeats)      REPEATS="$2";       shift 2 ;;
        --max-k)        MAX_K="$2";         shift 2 ;;
        --binary)       MINIGU="$2";        shift 2 ;;
        --graph)        GRAPH_NAME="$2";    shift 2 ;;
        --skip-prepare) SKIP_PREPARE=true;  shift ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 --dataset /path/to/sf10 [--threads \"1 2 4 8\"] [--repeats 3] [--max-k 2] [--skip-prepare]"
            exit 1
            ;;
    esac
done

if [[ -z "$DATASET_DIR" ]]; then
    echo "ERROR: --dataset is required (path to LDBC CSV directory with manifest.json)"
    exit 1
fi

DATASET_DIR="$(cd "$DATASET_DIR" && pwd)"
SF_NAME="$(basename "$DATASET_DIR")"

# ============================================================================
# Setup
# ============================================================================
RESULT_DIR="$PROJECT_DIR/experiment/result/build_catalog"
mkdir -p "$RESULT_DIR"

OUTPUT_CSV="$RESULT_DIR/bench_catalog_${SF_NAME}_results.csv"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

SCANNED_HOPS_BIN="$DATASET_DIR/${GRAPH_NAME}.scanned_hops.bin"

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

if [[ ! -f "$DATASET_DIR/manifest.json" ]]; then
    echo "ERROR: manifest.json not found in $DATASET_DIR"
    exit 1
fi

# ============================================================================
# Phase 0: Prepare ScannedHops bincode (one-time)
# ============================================================================
if [[ "$SKIP_PREPARE" == "true" && -f "$SCANNED_HOPS_BIN" ]]; then
    log "Skipping preparation (--skip-prepare, bincode exists)"
else
    log "=== Phase 0: Building ScannedHops bincode via load_ldbc ==="
    log "  Dataset:   $DATASET_DIR"
    log "  Max K:     $MAX_K"

    prepare_script="$TMP_DIR/prepare.gql"
    cat > "$prepare_script" <<EOGQL
call load_ldbc("${DATASET_DIR}", "${GRAPH_NAME}", ${MAX_K})
EOGQL

    prepare_out="$TMP_DIR/prepare_out.txt"
    "$MINIGU" execute "$prepare_script" > "$prepare_out" 2>&1 || {
        log "  FAILED to prepare ScannedHops bincode"
        cat "$prepare_out"
        exit 1
    }

    if [[ ! -f "$SCANNED_HOPS_BIN" ]]; then
        log "  ERROR: Expected ScannedHops bincode not found at $SCANNED_HOPS_BIN"
        cat "$prepare_out"
        exit 1
    fi

    sh_size=$(du -h "$SCANNED_HOPS_BIN" | awk '{print $1}')
    log "  ScannedHops bincode ready: $SCANNED_HOPS_BIN ($sh_size)"
fi

# ============================================================================
# Phase 1: Benchmark create_catalog with different thread counts
#           All configs run in a SINGLE process — load once, reuse cache.
# ============================================================================
log ""
log "=== Catalog Build Benchmark (${SF_NAME}) ==="
log "Binary:        $MINIGU"
log "Dataset:       $DATASET_DIR"
log "ScannedHops:   $SCANNED_HOPS_BIN"
log "Graph:         $GRAPH_NAME"
log "Max K:         $MAX_K"
log "Thread counts: $THREAD_COUNTS"
log "Repeats:       $REPEATS"
log ""

# CSV header
echo "sf,threads,repeat,scan_time_s,compute_time_s,build_time_s,stat_size_mb,compressed_size_mb,peak_mem_mb" > "$OUTPUT_CSV"

# Build a single GQL script: load_ldbc once, then all create_catalog calls
script_file="$TMP_DIR/bench_${SF_NAME}_all.gql"
cat > "$script_file" <<EOGQL
call load_ldbc("${DATASET_DIR}", "${GRAPH_NAME}", ${MAX_K})
session set graph ${GRAPH_NAME}
EOGQL

for threads in $THREAD_COUNTS; do
    for repeat in $(seq 1 "$REPEATS"); do
        echo "call create_catalog(\"${GRAPH_NAME}\", ${MAX_K}, ${threads}, ${threads}, \"${SCANNED_HOPS_BIN}\")" >> "$script_file"
    done
done

stdout_file="$TMP_DIR/stdout_${SF_NAME}.txt"
time_file="$TMP_DIR/time_${SF_NAME}.txt"

time_flag="-l"
if [[ "$(uname)" == "Linux" ]]; then
    time_flag="-v"
fi

log "Running all configs in a single process (graph loaded once)..."
/usr/bin/time $time_flag "$MINIGU" execute "$script_file" \
    > "$stdout_file" 2> "$time_file" || {
    log "  FAILED (exit code $?)"
    cat "$stdout_file"
    for threads in $THREAD_COUNTS; do
        for repeat in $(seq 1 "$REPEATS"); do
            echo "$SF_NAME,$threads,$repeat,ERROR,ERROR,ERROR,,,," >> "$OUTPUT_CSV"
        done
    done
    exit 1
}

# Parse peak memory from /usr/bin/time (one value for entire run)
if [[ "$(uname)" == "Linux" ]]; then
    peak_kb=$(grep "Maximum resident set size" "$time_file" | awk '{print $NF}') || peak_kb=0
    peak_mb=$(python3 -c "print(round($peak_kb / 1024, 2))")
else
    peak_bytes=$(grep "maximum resident set size" "$time_file" | awk '{print $1}') || peak_bytes=0
    peak_mb=$(python3 -c "print(round($peak_bytes / 1024 / 1024, 2))")
fi

# Parse per-call results from create_catalog output
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

        echo "$SF_NAME,$threads,$repeat,$scan_time,$compute_time,$build_time,$stat_mb,$comp_mb,$peak_mb" >> "$OUTPUT_CSV"
        log "  threads=$threads repeat=$repeat -> scan=${scan_time}s  compute=${compute_time}s  total=${build_time}s  stat=${stat_mb}MB  peak=${peak_mb}MB"
    done
done

log ""
log "=== Results ==="
column -t -s',' "$OUTPUT_CSV"
log ""
log "Results saved to $OUTPUT_CSV"
