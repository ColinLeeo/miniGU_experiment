#!/usr/bin/env bash
#
# Prepare all baseline tools' data and catalogs for a given SF.
#
# Usage:
#   ./prepare_baselines.sh <sf> [tools...]
#
# Examples:
#   ./prepare_baselines.sh 1                    # prepare all tools
#   ./prepare_baselines.sh 1 pathce gcare       # prepare only pathce and gcare
#
# This script:
#   1. pathce: serialize CSV -> bincode, build catalog
#   2. gcare:  convert CSV -> G-CARE txt, build summaries
#   3. color:  build summary (requires G-CARE txt from step 2)
#   4. glogs:  build graph, build catalog

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXPERIMENT_DIR="$PROJECT_DIR/experiment"

SF=${1:?Usage: prepare_baselines.sh <sf> [tools...]}
shift
if [[ $# -eq 0 ]]; then
    TOOLS=(pathce gcare color)
else
    TOOLS=("$@")
fi

SCHEMA="$EXPERIMENT_DIR/schemas/ldbc/ldbc_pathce_schema.json"
GLOGS_SCHEMA="$EXPERIMENT_DIR/schemas/ldbc/ldbc_glogs_schema.json"
DATASET_DIR="$EXPERIMENT_DIR/dataset/ldbc/sf${SF}"
THREADS=16

log() { echo "[$(date '+%H:%M:%S')] $*"; }

should_run() {
    for t in "${TOOLS[@]}"; do
        [[ "$t" == "$1" ]] && return 0
    done
    return 1
}

# ── Symlinks: schema uses CamelCase but CSV files are lowercase ───────────
# Create symlinks so that pathce/glogs can find e.g. "Person.csv" -> "person.csv"
create_csv_symlinks() {
    local dir="$1"
    local schema="$2"
    # Extract all label names from schema (vertex_labels + edge_labels keys)
    python3 -c "
import json, os, sys
with open('$schema') as f:
    s = json.load(f)
d = '$dir'
for name in list(s.get('vertex_labels',{}).keys()) + list(s.get('edge_labels',{}).keys()):
    lower = os.path.join(d, name.lower() + '.csv')
    upper = os.path.join(d, name + '.csv')
    if os.path.exists(lower) and not os.path.exists(upper) and lower != upper:
        os.symlink(os.path.basename(lower), upper)
        print(f'  symlink: {name}.csv -> {name.lower()}.csv')
"
}

log "Ensuring CSV symlinks for CamelCase label names..."
create_csv_symlinks "$DATASET_DIR" "$SCHEMA"

# ── pathce ─────────────────────────────────────────────────────────────────
if should_run pathce; then
    PATHCE="$EXPERIMENT_DIR/baseline/pathce/target/release/pathce"
    if [[ ! -x "$PATHCE" ]]; then
        log "Building pathce..."
        (cd "$EXPERIMENT_DIR/baseline/pathce" && cargo build --release)
    fi

    GRAPH_DIR="$EXPERIMENT_DIR/graphs/ldbc/pathce"
    mkdir -p "$GRAPH_DIR"
    GRAPH="$GRAPH_DIR/ldbc_sf${SF}.bincode"
    if [[ ! -f "$GRAPH" ]]; then
        log "pathce: serializing graph sf${SF}..."
        "$PATHCE" serialize -i "$DATASET_DIR" -s "$SCHEMA" -o "$GRAPH"
    else
        log "pathce: graph already exists: $GRAPH"
    fi

    CATALOG_DIR="$EXPERIMENT_DIR/catalogs/ldbc/pathce/ldbc_sf${SF}_2_5_200"
    if [[ ! -d "$CATALOG_DIR" ]]; then
        log "pathce: building catalog sf${SF}..."
        mkdir -p "$CATALOG_DIR"
        "$PATHCE" analyze -s "$SCHEMA" -g "$GRAPH" --greedy -t "$THREADS" \
            -o "$CATALOG_DIR" --max-path-length 2 --max-star-degree 5 --buckets 200
    else
        log "pathce: catalog already exists: $CATALOG_DIR"
    fi
fi

# ── gcare ──────────────────────────────────────────────────────────────────
if should_run gcare; then
    GCARE_GRAPH="$EXPERIMENT_DIR/baseline/gcare/build/gcare_graph"
    if [[ ! -x "$GCARE_GRAPH" ]]; then
        log "Building gcare..."
        mkdir -p "$EXPERIMENT_DIR/baseline/gcare/build"
        (cd "$EXPERIMENT_DIR/baseline/gcare/build" && cmake .. && make -j)
    fi

    GCARE_TXT="$EXPERIMENT_DIR/dataset/ldbc/sf${SF}.txt"
    GCARE_DATA="$EXPERIMENT_DIR/dataset/ldbc/sf${SF}"
    # gcare binary graph marker: <data_dir>.graph.meta
    GCARE_BIN_MARKER="${GCARE_DATA}.graph.meta"

    # If binary graph doesn't exist, (re)generate txt and build everything
    if [[ ! -f "$GCARE_BIN_MARKER" ]]; then
        # Remove stale txt to ensure fresh conversion
        rm -f "$GCARE_TXT"
        log "gcare: converting CSV to G-CARE txt..."
        python3 "$EXPERIMENT_DIR/tools/convert_csv_to_gcare.py" \
            -s "$SCHEMA" -d "$DATASET_DIR" -o "$GCARE_TXT"
        log "gcare: building binary graph + WJ summary sf${SF}..."
        "$GCARE_GRAPH" -b -m wj -i "$GCARE_TXT" -d "$GCARE_DATA"
    else
        log "gcare: binary graph already exists: $GCARE_BIN_MARKER"
    fi
fi

# ── color ──────────────────────────────────────────────────────────────────
if should_run color; then
    COLOR_DIR="$EXPERIMENT_DIR/baseline/color"
    CATALOG_DIR="$EXPERIMENT_DIR/catalogs/ldbc/color"
    mkdir -p "$CATALOG_DIR"
    SUMMARY="$CATALOG_DIR/ldbc_sf${SF}_mix_6_50000.obj"

    GCARE_TXT="$EXPERIMENT_DIR/dataset/ldbc/sf${SF}.txt"
    if [[ ! -f "$GCARE_TXT" ]]; then
        log "color: SKIP - need G-CARE txt first (run: prepare_baselines.sh $SF gcare)"
    elif ! command -v julia &>/dev/null; then
        log "color: SKIP - julia not found"
    elif [[ ! -f "$SUMMARY" ]]; then
        log "color: installing Julia dependencies..."
        julia --project="$COLOR_DIR" -e 'using Pkg; Pkg.instantiate()'
        log "color: building summary sf${SF}..."
        julia --project="$COLOR_DIR" "$COLOR_DIR/scripts/build.jl" -d "$GCARE_TXT" -o "$SUMMARY"
    else
        log "color: summary already exists: $SUMMARY"
    fi
fi

# ── glogs ──────────────────────────────────────────────────────────────────
if should_run glogs; then
    GLOGS_IR="$EXPERIMENT_DIR/baseline/glogs/ir"
    # glogs depends on traitobject 0.1.0 which requires Rust < 1.65.
    # Build in a copy outside the miniGU workspace to avoid resolver = "3" conflict.
    GLOGS_BUILD_DIR="$HOME/glogs_build"
    GLOGS_BIN="$GLOGS_BUILD_DIR/ir/target/release"
    if [[ ! -x "$GLOGS_BIN/build_graph" ]]; then
        log "Building glogs IR (in $GLOGS_BUILD_DIR with Rust 1.64.0)..."
        rm -rf "$GLOGS_BUILD_DIR"
        cp -r "$EXPERIMENT_DIR/baseline/glogs" "$GLOGS_BUILD_DIR"
        (cd "$GLOGS_BUILD_DIR/ir" && rustup run 1.64.0 cargo build --release)
    fi

    GRAPH_DIR="$EXPERIMENT_DIR/graphs/ldbc/glogs"
    mkdir -p "$GRAPH_DIR"
    GRAPH="$GRAPH_DIR/ldbc_sf${SF}"
    if [[ ! -d "$GRAPH" ]]; then
        log "glogs: building graph sf${SF}..."
        DUMMY_SCHEMA="$EXPERIMENT_DIR/schemas/dummy_schema.json"
        if [[ ! -f "$DUMMY_SCHEMA" ]]; then
            echo '{"entities":[],"relations":[],"is_column_id":false,"is_table_id":true}' > "$DUMMY_SCHEMA"
        fi
        "$GLOGS_BIN/build_graph" --schema1 "$GLOGS_SCHEMA" --schema2 "$DUMMY_SCHEMA" \
            -d "$DATASET_DIR" -o "$GRAPH"
    else
        log "glogs: graph already exists: $GRAPH"
    fi

    CATALOG="$EXPERIMENT_DIR/catalogs/ldbc/glogs/ldbc_sf${SF}.bincode"
    mkdir -p "$(dirname "$CATALOG")"
    if [[ ! -f "$CATALOG" ]]; then
        log "glogs: building catalog sf${SF}..."
        SCHEMA_PATH="$GLOGS_SCHEMA" SAMPLE_PATH="$GRAPH" \
            "$GLOGS_BIN/build_catalog" -m from_meta -t "$THREADS" -p "$CATALOG"
    else
        log "glogs: catalog already exists: $CATALOG"
    fi
fi

log "Preparation complete for sf${SF}."
