#!/usr/bin/env bash
#
# Benchmark: measure how random insertions degrade GCard estimation accuracy.
#
# Symmetric to bench_update_impact.sh (deletion), but uses random_insert instead.
#
# Flow:
#   1. Load LDBC data via load_ldbc (single minigu process)
#   2. Run baseline queries (round 0)
#   3. For each insert round:
#        random_insert(ratio, roundX/graph.bin)
#        export_flatgraph_snapshot(roundX/graph.bin, roundX/snapshot)
#        gcard_query on all patterns
#   4. After minigu exits, run true_cardinality.py on round 0 CSVs and each round's snapshot
#   5. Merge estimated vs true cardinality into a final CSV
#
# Usage:
#   ./bench_insert_impact.sh [OPTIONS] <SF>
#
# Options:
#   --max-k K           Maximum k for gcard_query (default: 3)
#   --sample-size N     Sample size (default: 500)
#   --max-graph N       Max spanning trees (default: 5)
#   --ratios R1,R2,...  Comma-separated insert ratios (default: 0.01,0.01,0.01,0.01,0.01)
#   --seed S            Base random seed (default: 42)
#
# Example:
#   ./bench_insert_impact.sh sf1
#   ./bench_insert_impact.sh --ratios 0.01,0.02,0.03 sf0.1

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

MINIGU="$PROJECT_DIR/target/release/minigu"
PATTERN_DIR="$PROJECT_DIR/experiment/pattern/LDBC"
TRUE_CARD_SCRIPT="$PROJECT_DIR/experiment/scripts/true_cardinality.py"

GRAPH_NAME="ldbc"
MAX_K=2
SAMPLE_SIZE=500
MAX_GRAPH=5
RATIOS="0.01,0.01,0.01,0.01,0.01"
BASE_SEED=42
SF=""

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --max-k)       MAX_K="$2"; shift 2 ;;
        --sample-size) SAMPLE_SIZE="$2"; shift 2 ;;
        --max-graph)   MAX_GRAPH="$2"; shift 2 ;;
        --ratios)      RATIOS="$2"; shift 2 ;;
        --seed)        BASE_SEED="$2"; shift 2 ;;
        --*)           echo "ERROR: Unknown option $1"; exit 1 ;;
        *)             SF="$1"; shift ;;
    esac
done

if [[ -z "$SF" ]]; then
    echo "Usage: $0 [OPTIONS] <SF>"
    echo "  e.g. $0 sf1"
    exit 1
fi

CSV_DIR="$PROJECT_DIR/experiment/datasets/ldbc/$SF"
if [[ ! -f "$CSV_DIR/manifest.json" ]]; then
    echo "ERROR: manifest.json not found at $CSV_DIR/manifest.json"
    exit 1
fi

if [[ ! -x "$MINIGU" ]]; then
    echo "ERROR: minigu binary not found at $MINIGU"
    echo "Run: cargo build --release --features std,serde,miette"
    exit 1
fi

# Parse ratios into array
IFS=',' read -ra RATIO_ARR <<< "$RATIOS"
NUM_ROUNDS=${#RATIO_ARR[@]}

RESULT_DIR="$PROJECT_DIR/experiment/result/insert_impact/${SF}"
mkdir -p "$RESULT_DIR"

log() { echo "[$(date '+%H:%M:%S')] $*"; }

# Collect pattern files
PATTERNS=()
while IFS= read -r f; do
    PATTERNS+=("$f")
done < <(find "$PATTERN_DIR" -name '*.json' -type f | sort)

log "=== Insert Impact Benchmark ==="
log "SF=$SF  max_k=$MAX_K  sample_size=$SAMPLE_SIZE  max_graph=$MAX_GRAPH"
log "Ratios: ${RATIOS}  (${NUM_ROUNDS} rounds)"
log "Patterns: ${#PATTERNS[@]}"
log "Result dir: $RESULT_DIR"

# ── Phase 1: generate GQL script and run minigu ─────────────────────────────

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

DB_PATH="$TMP_DIR/db"
mkdir -p "$DB_PATH"

SCRIPT_FILE="$TMP_DIR/bench_insert.gql"

# Write load + set graph
cat > "$SCRIPT_FILE" <<EOGQL
:time call load_ldbc("${CSV_DIR}", "${GRAPH_NAME}", ${MAX_K})
session set graph ${GRAPH_NAME}
EOGQL

# Round 0: baseline queries
for pattern_path in "${PATTERNS[@]}"; do
    echo ":time call gcard_query(\"${pattern_path}\", ${MAX_K}, ${SAMPLE_SIZE}, 0, false, ${MAX_GRAPH})" >> "$SCRIPT_FILE"
done

# Rounds 1..N: insert + export snapshot + query
for i in $(seq 0 $((NUM_ROUNDS - 1))); do
    round=$((i + 1))
    ratio="${RATIO_ARR[$i]}"
    seed=$((BASE_SEED + round))
    round_dir="${RESULT_DIR}/round${round}"
    graph_bin="${round_dir}/graph.bin"
    snapshot_dir="${round_dir}/snapshot"

    mkdir -p "$round_dir"
    echo ":time call random_insert(${ratio}, \"${graph_bin}\", ${seed})" >> "$SCRIPT_FILE"
    echo ":time call export_flatgraph_snapshot(\"${graph_bin}\", \"${snapshot_dir}\")" >> "$SCRIPT_FILE"

    for pattern_path in "${PATTERNS[@]}"; do
        echo ":time call gcard_query(\"${pattern_path}\", ${MAX_K}, ${SAMPLE_SIZE}, 0, false, ${MAX_GRAPH})" >> "$SCRIPT_FILE"
    done
done

log "Running minigu with ${NUM_ROUNDS} insert rounds + queries..."
STDOUT_FILE="$TMP_DIR/output.txt"
"$MINIGU" execute "$SCRIPT_FILE" --path "$DB_PATH" 2>&1 | tee "$STDOUT_FILE"

# ── Phase 2: parse estimated cardinalities from minigu output ────────────────

# Extract all cardinality lines in order
CARDINALITIES=()
while IFS= read -r line; do
    CARDINALITIES+=("$line")
done < <(grep -o 'cardinality: [0-9]*' "$STDOUT_FILE" | awk '{print $2}')

log "Parsed ${#CARDINALITIES[@]} cardinality values (expected $((${#PATTERNS[@]} * (NUM_ROUNDS + 1))))"

TIME_LINES=()
while IFS= read -r line; do
    TIME_LINES+=("$line")
done < <(grep -o 'Time: [0-9.]*s' "$STDOUT_FILE" | grep -o '[0-9.]*')

TIMING_CSV="${RESULT_DIR}/timing.csv"
echo "round,step,item,seconds" > "$TIMING_CSV"

time_idx=0
next_time() {
    local value="${TIME_LINES[$time_idx]:-}"
    time_idx=$((time_idx + 1))
    echo "${value:-0}"
}

echo "0,load_ldbc,${GRAPH_NAME},$(next_time)" >> "$TIMING_CSV"

for pattern_path in "${PATTERNS[@]}"; do
    rel="${pattern_path#$PATTERN_DIR/}"
    echo "0,gcard_query,${rel},$(next_time)" >> "$TIMING_CSV"
done

for i in $(seq 0 $((NUM_ROUNDS - 1))); do
    round=$((i + 1))
    ratio="${RATIO_ARR[$i]}"
    echo "${round},random_insert,ratio=${ratio},$(next_time)" >> "$TIMING_CSV"
    echo "${round},export_flatgraph_snapshot,round${round}/snapshot,$(next_time)" >> "$TIMING_CSV"
    for pattern_path in "${PATTERNS[@]}"; do
        rel="${pattern_path#$PATTERN_DIR/}"
        echo "${round},gcard_query,${rel},$(next_time)" >> "$TIMING_CSV"
    done
done

if [[ "$time_idx" -ne "${#TIME_LINES[@]}" ]]; then
    log "WARN: timing line count mismatch (${time_idx} mapped vs ${#TIME_LINES[@]} parsed)"
fi

# Write per-round estimated results
N_PAT=${#PATTERNS[@]}
for round in $(seq 0 "$NUM_ROUNDS"); do
    offset=$((round * N_PAT))
    est_file="${RESULT_DIR}/round${round}/estimates.csv"
    mkdir -p "$(dirname "$est_file")"
    echo "pattern_group,pattern,estimated_cardinality" > "$est_file"

    for j in $(seq 0 $((N_PAT - 1))); do
        idx=$((offset + j))
        rel="${PATTERNS[$j]#$PATTERN_DIR/}"
        group="$(dirname "$rel")"
        name="$(basename "$rel" .json)"
        est="${CARDINALITIES[$idx]:-0}"
        echo "${group},${name},${est}" >> "$est_file"
    done
done

# ── Phase 3: compute true cardinalities on each snapshot ─────────────────────

log "Computing true cardinalities for each round..."

for round in $(seq 0 "$NUM_ROUNDS"); do
    round_dir="${RESULT_DIR}/round${round}"
    if [[ "$round" -eq 0 ]]; then
        snapshot_dir="$CSV_DIR"
    else
        snapshot_dir="${round_dir}/snapshot"
    fi
    true_file="${round_dir}/true_cardinality.csv"
    mkdir -p "$round_dir"

    if [[ ! -d "$snapshot_dir" ]] || ! ls "$snapshot_dir"/*.csv &>/dev/null 2>&1; then
        log "  round${round}: no CSV snapshot, skipping true cardinality"
        continue
    fi

    echo "pattern_group,pattern,true_cardinality,time_s" > "$true_file"

    for pattern_path in "${PATTERNS[@]}"; do
        rel="${pattern_path#$PATTERN_DIR/}"
        group="$(dirname "$rel")"
        name="$(basename "$rel" .json)"

        result=$(python3 "$TRUE_CARD_SCRIPT" "$snapshot_dir" "$pattern_path" 2>/dev/null || echo "ERROR")
        if echo "$result" | grep -q 'ERROR'; then
            card=0
            time_s=0
        else
            # Output format: "L1: 12345  (0.23s)"
            card=$(echo "$result" | sed -n 's/.*: \([0-9]*\).*/\1/p' | head -1)
            time_s=$(echo "$result" | grep -o '([0-9.]*s)' | grep -o '[0-9.]*' || echo "0")
            card="${card:-0}"
        fi

        echo "${group},${name},${card:-0},${time_s}" >> "$true_file"
    done
    log "  round${round}: true cardinality → $true_file"
done

# ── Phase 4: merge into wide-format CSV (rounds as columns) ──────────────────

FINAL_CSV="${RESULT_DIR}/insert_impact_results.csv"

# Build header: pattern_group, pattern, then per-round columns
header="pattern_group,pattern"
cum_ratio="0.0"
for round in $(seq 0 "$NUM_ROUNDS"); do
    if [[ $round -gt 0 ]]; then
        r_idx=$((round - 1))
        cum_ratio=$(python3 -c "print(round(${cum_ratio} + ${RATIO_ARR[$r_idx]}, 4))")
    fi
    header="${header},est_r${round}(+${cum_ratio}),true_r${round}(+${cum_ratio}),qerr_r${round}(+${cum_ratio})"
done
echo "$header" > "$FINAL_CSV"

# Build rows: one per pattern, all rounds as columns
for j in $(seq 0 $((N_PAT - 1))); do
    rel="${PATTERNS[$j]#$PATTERN_DIR/}"
    group="$(dirname "$rel")"
    name="$(basename "$rel" .json)"
    row="${group},${name}"

    for round in $(seq 0 "$NUM_ROUNDS"); do
        est_file="${RESULT_DIR}/round${round}/estimates.csv"
        true_file="${RESULT_DIR}/round${round}/true_cardinality.csv"

        est="0"; true_c="0"; qerr="inf"
        if [[ -f "$est_file" ]]; then
            est=$(awk -F',' -v pat="$name" '$2==pat {print $3}' "$est_file")
            est="${est:-0}"
        fi
        if [[ -f "$true_file" ]]; then
            true_c=$(awk -F',' -v pat="$name" '$2==pat {print $3}' "$true_file")
            true_c="${true_c:-0}"
        fi

        if [[ "$true_c" != "0" ]] && [[ "$est" != "0" ]]; then
            qerr=$(python3 -c "e=max($est,$true_c); t=min($est,$true_c); print(f'{e/t:.2f}' if t>0 else 'inf')")
        fi

        row="${row},${est},${true_c},${qerr}"
    done

    echo "$row" >> "$FINAL_CSV"
done

log "=== Done ==="
log "Results: $FINAL_CSV"
log "Timing:  $TIMING_CSV"

INTERNAL_TIMING_CSV="${RESULT_DIR}/internal_timing.csv"
echo "component,round,step,seconds" > "$INTERNAL_TIMING_CSV"

awk '
BEGIN { round = 0 }
/\[random_insert\] phase1 insert_vertices:/ {
  sub(/^.*phase1 insert_vertices: /, "", $0); sub(/s.*$/, "", $0);
  print "random_insert," round ",phase1_insert_vertices," $0;
}
/\[random_insert\] phase2 insert_edges_and_append_log:/ {
  sub(/^.*phase2 insert_edges_and_append_log: /, "", $0); sub(/s.*$/, "", $0);
  print "random_insert," round ",phase2_insert_edges_and_append_log," $0;
}
/\[random_insert\] phase3 compact_log:/ {
  sub(/^.*phase3 compact_log: /, "", $0); sub(/s.*$/, "", $0);
  print "random_insert," round ",phase3_compact_log," $0;
}
/\\[compact\\] neighbor_expand:/ {
  sub(/^.*neighbor_expand: /, "", $0); sub(/s.*$/, "", $0);
  print "compact," round ",neighbor_expand," $0;
}
/\\[compact\\] apply_delta:/ {
  sub(/^.*apply_delta: /, "", $0); sub(/s.*$/, "", $0);
  print "compact," round ",apply_delta," $0;
}
/\\[compact\\] delete_vertices:/ {
  sub(/^.*delete_vertices: /, "", $0); sub(/s.*$/, "", $0);
  print "compact," round ",delete_vertices," $0;
}
/\\[compact\\] total_stat_update:/ {
  sub(/^.*total_stat_update: /, "", $0); sub(/s.*$/, "", $0);
  print "compact," round ",total_stat_update," $0;
}
/\[random_insert\] phase4a apply_pending:/ {
  sub(/^.*phase4a apply_pending: /, "", $0); sub(/s.*$/, "", $0);
  print "random_insert," round ",phase4a_apply_pending," $0;
}
/\\[flatgraph\\] apply_pending phase1 vertices:/ {
  sub(/^.*phase1 vertices: /, "", $0); sub(/s.*$/, "", $0);
  print "flatgraph," round ",apply_pending_vertices," $0;
}
/\\[flatgraph\\] apply_pending phase2 affected_buckets:/ {
  sub(/^.*phase2 affected_buckets: /, "", $0); sub(/s.*$/, "", $0);
  print "flatgraph," round ",apply_pending_affected_buckets," $0;
}
/\\[flatgraph\\] apply_pending phase3 rebuild_buckets:/ {
  sub(/^.*phase3 rebuild_buckets: /, "", $0); sub(/s.*$/, "", $0);
  print "flatgraph," round ",apply_pending_rebuild_buckets," $0;
}
/\\[flatgraph\\] apply_pending phase4 edge_maps:/ {
  sub(/^.*phase4 edge_maps: /, "", $0); sub(/s.*$/, "", $0);
  print "flatgraph," round ",apply_pending_edge_maps," $0;
}
/\\[flatgraph\\] apply_pending total:/ {
  sub(/^.*apply_pending total: /, "", $0); sub(/s.*$/, "", $0);
  print "flatgraph," round ",apply_pending_total," $0;
}
/\[random_insert\] phase4b rebuild_dsgc:/ {
  sub(/^.*phase4b rebuild_dsgc: /, "", $0); sub(/s.*$/, "", $0);
  print "random_insert," round ",phase4b_rebuild_dsgc," $0;
}
/\\[dsgc\\] encode_dirty:/ {
  sub(/^.*encode_dirty: /, "", $0); sub(/s.*$/, "", $0);
  print "dsgc," round ",encode_dirty," $0;
}
/\\[dsgc\\] apply_dirty:/ {
  sub(/^.*apply_dirty: /, "", $0); sub(/s.*$/, "", $0);
  print "dsgc," round ",apply_dirty," $0;
}
/\\[dsgc\\] update_dirty total:/ {
  sub(/^.*update_dirty total: /, "", $0); sub(/s.*$/, "", $0);
  print "dsgc," round ",update_dirty_total," $0;
}
/\[random_insert\] phase4 total:/ {
  sub(/^.*phase4 total: /, "", $0); sub(/s.*$/, "", $0);
  print "random_insert," round ",phase4_total," $0;
}
/\[random_insert\] total:/ {
  sub(/^.*total: /, "", $0); sub(/s.*$/, "", $0);
  round += 1;
  print "random_insert," round ",total," $0;
}
' "$STDOUT_FILE" >> "$INTERNAL_TIMING_CSV"

awk '
/load_ldbc: manifest loaded in / {
  sub(/^.*manifest loaded in /, "", $0); sub(/s.*$/, "", $0);
  print "load_ldbc,0,manifest_load," $0;
}
/load_ldbc: scanned hops built in / {
  sub(/^.*scanned hops built in /, "", $0); sub(/s.*$/, "", $0);
  print "load_ldbc,0,scan_build," $0;
}
/load_ldbc: degree compute done in / {
  sub(/^.*degree compute done in /, "", $0); sub(/s.*$/, "", $0);
  print "load_ldbc,0,degree_compute," $0;
}
/load_ldbc: FlatGraph built in / {
  sub(/^.*FlatGraph built in /, "", $0); sub(/s.*$/, "", $0);
  print "load_ldbc,0,flatgraph_build," $0;
}
' "$STDOUT_FILE" >> "$INTERNAL_TIMING_CSV"

log "Internal timing: $INTERNAL_TIMING_CSV"
echo ""
echo "=== Insert Impact Results ==="
column -t -s',' "$FINAL_CSV" 2>/dev/null || cat "$FINAL_CSV"
