#!/usr/bin/env bash
#
# Unified benchmark: run all cardinality estimators on miniGU LDBC patterns.
#
# Combines gcard_query, baselines, and ground truth into a single output file.
#
# Usage:
#   ./bench_all.sh <sf> [pattern_dir]
#
# Examples:
#   ./bench_all.sh 1                                       # all LDBC patterns
#   ./bench_all.sh 0.1 experiment/pattern/LDBC/L4_cycle    # specific subset
#
# Output CSV columns:
#   sf, pattern_group, pattern, query_type, tool, pred_mode, cardinality, time_s, status
#
# Tools run:
#   - truth:     DuckDB exact count (all patterns, with/without predicates)
#   - gcard:     miniGU GCard (all patterns; INNER mode for all)
#   - pathce:    PathCE estimator (base patterns only)
#   - gcare-wj:  G-CARE WanderJoin (base patterns only)
#   - color:     Color estimator (base patterns only)
#   - glogs:     GLogS estimator (base patterns only)
#
# Prerequisites (run once per SF):
#   1. miniGU:  import graph + create_catalog (or load_catalog)
#   2. pathce:  ./prepare_baselines.sh <sf> pathce
#   3. gcare:   ./prepare_baselines.sh <sf> gcare
#   4. color:   ./prepare_baselines.sh <sf> color
#   5. glogs:   ./prepare_baselines.sh <sf> glogs

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXPERIMENT_DIR="$PROJECT_DIR/experiment"

SF=${1:?Usage: bench_all.sh <sf> [pattern_dir]}
PATTERN_SOURCE="${2:-$EXPERIMENT_DIR/pattern/LDBC}"

# ── Config ─────────────────────────────────────────────────────────────────
SCHEMA="$EXPERIMENT_DIR/schemas/ldbc/ldbc_pathce_schema.json"
# No timeout — we want to see actual query latency
GRAPH_NAME="ldbc"
MAX_K=2
SAMPLE_SIZE=500

# Tool binaries
MINIGU="$PROJECT_DIR/target/release/minigu"
PATHCE="$EXPERIMENT_DIR/baseline/pathce/target/release/pathce"
GCARE_GRAPH="$EXPERIMENT_DIR/baseline/gcare/build/gcare_graph"
GLOGS_BIN="$EXPERIMENT_DIR/baseline/glogs/ir/target/release/pattern_count"

# Data paths
DATASET_DIR="$EXPERIMENT_DIR/dataset/ldbc/sf${SF}"
MINIGU_DB="$DATASET_DIR/minigu_db"
PATHCE_CATALOG="$EXPERIMENT_DIR/catalogs/ldbc/pathce/ldbc_sf${SF}_2_5_200"
PATHCE_GRAPH="$EXPERIMENT_DIR/graphs/ldbc/pathce/ldbc_sf${SF}.bincode"
GCARE_DATA_DIR="$DATASET_DIR"
COLOR_SUMMARY="$EXPERIMENT_DIR/catalogs/ldbc/color/ldbc_sf${SF}_mix_6_50000.obj"
GLOGS_CATALOG="$EXPERIMENT_DIR/catalogs/ldbc/glogs/ldbc_sf${SF}.bincode"

RESULT_DIR="$EXPERIMENT_DIR/result/comparison"
mkdir -p "$RESULT_DIR"
OUTPUT_CSV="$RESULT_DIR/comparison_sf${SF}.csv"

log() { echo "[$(date '+%H:%M:%S')] $*"; }

# ── Helper: extract query_type from pattern directory name ────────────────
# L1_path -> path, L4_cycle -> cycle, L6_cycle_branch -> cycle_branch
get_query_type() {
    local group="$1"
    echo "$group" | sed 's/^L[0-9]*_//'
}

# ── Step 0: Collect and convert patterns ──────────────────────────────────
TMP_DIR=$(mktemp -d /tmp/bench_all_XXXX)
trap 'rm -rf "$TMP_DIR"' EXIT

PATHCE_PATTERN_DIR="$TMP_DIR/pathce"
GCARE_PATTERN_DIR="$TMP_DIR/gcare"

log "Converting patterns: $PATTERN_SOURCE"
python3 "$EXPERIMENT_DIR/tools/minigu2pathce.py" \
    -s "$SCHEMA" \
    -d "$PATTERN_SOURCE" \
    -o "$PATHCE_PATTERN_DIR" \
    --gcare "$GCARE_PATTERN_DIR"

# Collect ALL patterns (including predicate variants)
ALL_PATTERNS=()
while IFS= read -r f; do
    ALL_PATTERNS+=("$f")
done < <(find "$PATTERN_SOURCE" -name '*.json' -type f | sort)

# Base patterns only (no predicates) — for baselines that don't support predicates
BASE_PATTERNS=()
PRED_PATTERNS=()
for f in "${ALL_PATTERNS[@]}"; do
    base=$(basename "$f" .json)
    if [[ "$base" == *_P* ]]; then
        PRED_PATTERNS+=("$f")
    else
        BASE_PATTERNS+=("$f")
    fi
done

log "SF=$SF, base_patterns=${#BASE_PATTERNS[@]}, pred_patterns=${#PRED_PATTERNS[@]}, total=${#ALL_PATTERNS[@]}"

# Helper: given a pattern path, derive the relative name (e.g. L4_cycle/L4)
rel_name() {
    local p="$1"
    local rel="${p#$PATTERN_SOURCE/}"
    echo "${rel%.json}"
}

# ── CSV header ─────────────────────────────────────────────────────────────
echo "sf,pattern_group,pattern,query_type,tool,pred_mode,cardinality,time_s,status" > "$OUTPUT_CSV"

append() {
    # $1=group $2=name $3=tool $4=pred_mode $5=cardinality $6=time_s $7=status
    local qtype
    qtype=$(get_query_type "$1")
    echo "$SF,$1,$2,$qtype,$3,$4,$5,$6,$7" >> "$OUTPUT_CSV"
    log "  $3: $1/$2 [$qtype] pred=$4 -> card=$5 time=$6 ($7)"
}

# ── 1. Ground truth (DuckDB) ─────────────────────────────────────────────
if python3 -c "import duckdb" 2>/dev/null && [[ -d "$DATASET_DIR" ]]; then
    log "=== ground truth (duckdb) ==="
    for p in "${ALL_PATTERNS[@]}"; do
        rn=$(rel_name "$p")
        group=$(dirname "$rn")
        name=$(basename "$rn")
        pred_mode="none"
        [[ "$name" == *_P* ]] && pred_mode="predicate"

        # true_cardinality.py outputs "<name>: <cardinality>  (<time>s)"
        raw=$(python3 "$SCRIPT_DIR/true_cardinality.py" "$DATASET_DIR" "$p" 2>/dev/null || echo "")
        card=$(echo "$raw" | grep -oE ': [0-9]+' | head -1 | grep -oE '[0-9]+' || echo "")
        time_s=$(echo "$raw" | grep -oE '\([0-9.]+s\)' | grep -oE '[0-9.]+' || echo "0")

        if [[ -n "$card" ]]; then
            append "$group" "$name" "truth" "$pred_mode" "$card" "$time_s" "ok"
        else
            append "$group" "$name" "truth" "$pred_mode" "-1" "0" "error"
        fi
    done
elif [[ -x "$PATHCE" ]] && [[ -f "$PATHCE_GRAPH" ]]; then
    log "=== ground truth (pathce count, base patterns only) ==="
    for p in "${BASE_PATTERNS[@]}"; do
        rn=$(rel_name "$p")
        group=$(dirname "$rn")
        name=$(basename "$rn")
        pp="$PATHCE_PATTERN_DIR/${rn}.json"

        result=$("$PATHCE" count -g "$PATHCE_GRAPH" -p "$pp" -s path 2>/dev/null || echo "timeout")
        if [[ "$result" == "timeout" ]]; then
            append "$group" "$name" "truth" "none" "-1" "0" "timeout"
        else
            append "$group" "$name" "truth" "none" "$result" "0" "ok"
        fi
    done
else
    log "SKIP ground truth (neither duckdb nor pathce available)"
fi

# ── 2. GCard (miniGU, INNER mode for all patterns) ───────────────────────
if [[ -x "$MINIGU" ]] && [[ -d "$MINIGU_DB" ]]; then
    log "=== gcard ==="

    # Build GQL script: load catalog once, then run all queries
    TMP_GQL="$TMP_DIR/bench_gcard.gql"
    cat > "$TMP_GQL" <<EOF
session set graph ${GRAPH_NAME}
call load_catalog("${GRAPH_NAME}")
EOF

    # All patterns use INNER mode (pred_type=0)
    # - Base patterns: no predicates in the JSON, so INNER behaves same as IGNORE
    # - Predicate patterns: INNER applies predicates during PCF computation
    for p in "${ALL_PATTERNS[@]}"; do
        echo ":time call gcard_query(\"${p}\", ${MAX_K}, ${SAMPLE_SIZE}, 0, false)" >> "$TMP_GQL"
    done

    MINIGU_OUT="$TMP_DIR/minigu_out.txt"
    log "  Running ${#ALL_PATTERNS[@]} gcard queries in one process..."
    "$MINIGU" execute "$TMP_GQL" --path "$MINIGU_DB" 2>&1 | tee "$MINIGU_OUT" || true

    # Parse output: extract cardinality and time lines in order
    cardinalities=()
    while IFS= read -r line; do
        cardinalities+=("$line")
    done < <(grep -oE 'cardinality: [0-9]+' "$MINIGU_OUT" | awk '{print $2}')

    times=()
    while IFS= read -r line; do
        times+=("$line")
    done < <(grep -oE 'Time: [0-9.]+s' "$MINIGU_OUT" | grep -oE '[0-9.]+')

    log "  Parsed ${#cardinalities[@]} cardinalities, ${#times[@]} times for ${#ALL_PATTERNS[@]} queries"

    idx=0
    for p in "${ALL_PATTERNS[@]}"; do
        rn=$(rel_name "$p")
        group=$(dirname "$rn")
        name=$(basename "$rn")

        card="0"; time_s="0"
        if [[ $idx -lt ${#cardinalities[@]} ]]; then
            card="${cardinalities[$idx]}"
        fi
        if [[ $idx -lt ${#times[@]} ]]; then
            time_s="${times[$idx]}"
        fi

        if [[ "$card" == "0" ]]; then
            append "$group" "$name" "gcard" "INNER" "$card" "$time_s" "error"
        else
            append "$group" "$name" "gcard" "INNER" "$card" "$time_s" "ok"
        fi
        idx=$((idx + 1))
    done
else
    log "SKIP gcard (binary=$MINIGU or db=$MINIGU_DB not found)"
fi

# ── 3. pathce (base patterns only) ───────────────────────────────────────
if [[ -x "$PATHCE" ]] && [[ -d "$PATHCE_CATALOG" ]]; then
    log "=== pathce ==="
    for p in "${BASE_PATTERNS[@]}"; do
        rn=$(rel_name "$p")
        group=$(dirname "$rn")
        name=$(basename "$rn")
        pp="$PATHCE_PATTERN_DIR/${rn}.json"

        result=$("$PATHCE" estimate \
            -c "$PATHCE_CATALOG" -p "$pp" \
            --max-path-length 2 --max-star-degree 5 2>&1 || echo "0,0")
        IFS=',' read -r card time_s <<< "$result"
        status="ok"
        [[ "$card" == "0" ]] && [[ "$time_s" == "0" ]] && status="error"
        append "$group" "$name" "pathce" "none" "$card" "$time_s" "$status"
    done
else
    log "SKIP pathce (binary or catalog not found)"
fi

# ── 4. gcare (WanderJoin, base patterns only) ────────────────────────────
if [[ -x "$GCARE_GRAPH" ]] && [[ -d "$GCARE_DATA_DIR" ]]; then
    log "=== gcare-wj ==="
    for p in "${BASE_PATTERNS[@]}"; do
        rn=$(rel_name "$p")
        group=$(dirname "$rn")
        name=$(basename "$rn")
        gp="$GCARE_PATTERN_DIR/${rn}.txt"

        result=$(\
            "$GCARE_GRAPH" -q -m wj -n 1 -i "$gp" -d "$GCARE_DATA_DIR" 2>/dev/null \
            | tail -1 || echo "")
        if [[ "$result" == *","* ]]; then
            IFS=',' read -r card time_s <<< "$result"
            append "$group" "$name" "gcare-wj" "none" "$card" "$time_s" "ok"
        else
            append "$group" "$name" "gcare-wj" "none" "0" "0" "error"
        fi
    done
else
    log "SKIP gcare (binary or data not found)"
fi

# ── 5. color (base patterns only) ────────────────────────────────────────
if command -v julia &>/dev/null && [[ -f "$COLOR_SUMMARY" ]]; then
    log "=== color ==="
    COLOR_DIR="$EXPERIMENT_DIR/baseline/color"
    for p in "${BASE_PATTERNS[@]}"; do
        rn=$(rel_name "$p")
        group=$(dirname "$rn")
        name=$(basename "$rn")
        gp="$GCARE_PATTERN_DIR/${rn}.txt"

        result=$(\
            julia --project="$COLOR_DIR" "$COLOR_DIR/scripts/estimate.jl" \
            -q "$gp" -s "$COLOR_SUMMARY" 2>/dev/null || echo "0,0")
        IFS=',' read -r card time_s <<< "$result"
        append "$group" "$name" "color" "none" "$card" "$time_s" "ok"
    done
else
    log "SKIP color (julia or summary not found)"
fi

# ── 6. glogs (base patterns only) ────────────────────────────────────────
if [[ -x "$GLOGS_BIN" ]] && [[ -f "$GLOGS_CATALOG" ]]; then
    log "=== glogs ==="
    for p in "${BASE_PATTERNS[@]}"; do
        rn=$(rel_name "$p")
        group=$(dirname "$rn")
        name=$(basename "$rn")
        pp="$PATHCE_PATTERN_DIR/${rn}.json"

        result=$("$GLOGS_BIN" -c "$GLOGS_CATALOG" -p "$pp" 2>/dev/null || echo "0,0")
        IFS=',' read -r card time_s <<< "$result"
        append "$group" "$name" "glogs" "none" "$card" "$time_s" "ok"
    done
else
    log "SKIP glogs (binary or catalog not found)"
fi

# ── Summary ───────────────────────────────────────────────────────────────
log "Done. Results: $OUTPUT_CSV"
echo ""
echo "=== Results ==="
column -t -s',' "$OUTPUT_CSV"

# Print q-error summary
echo ""
echo "=== Q-Error Summary ==="
python3 - "$OUTPUT_CSV" <<'PYEOF'
import csv, sys
from collections import defaultdict

rows = list(csv.DictReader(open(sys.argv[1])))

# Collect truth values: key = (pattern, pred_mode)
truth = {}
for r in rows:
    if r["tool"] == "truth" and r["status"] == "ok":
        key = (r["pattern"], r["pred_mode"])
        truth[key] = float(r["cardinality"])

# Compute q-errors
tool_errors = defaultdict(list)
tool_type_errors = defaultdict(list)

for r in rows:
    if r["tool"] == "truth" or r["status"] != "ok":
        continue
    key = (r["pattern"], r["pred_mode"])
    if key not in truth or truth[key] <= 0:
        continue
    est = float(r["cardinality"])
    t = truth[key]
    if est <= 0:
        qerr = t
    else:
        qerr = max(est / t, t / est)

    tool_label = r["tool"]
    if r["pred_mode"] not in ("none", "IGNORE"):
        tool_label = f"{r['tool']}({r['pred_mode']})"

    tool_errors[tool_label].append(qerr)
    tool_type_errors[(tool_label, r["query_type"])].append(qerr)

def fmt(errors):
    if not errors:
        return "n/a"
    s = sorted(errors)
    median = s[len(s) // 2]
    mean = sum(s) / len(s)
    return f"median={median:.2f}  mean={mean:.2f}  max={max(s):.2f}  n={len(s)}"

print(f"\n{'Tool':<20} {'Overall Q-Error'}")
print("-" * 70)
for tool in sorted(tool_errors):
    print(f"{tool:<20} {fmt(tool_errors[tool])}")

print(f"\n{'Tool':<20} {'QueryType':<16} {'Q-Error'}")
print("-" * 80)
for (tool, qtype) in sorted(tool_type_errors):
    print(f"{tool:<20} {qtype:<16} {fmt(tool_type_errors[(tool, qtype)])}")
PYEOF
