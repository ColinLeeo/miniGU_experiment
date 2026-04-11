#!/usr/bin/env bash
#
# Benchmark: Mode 1 create_catalog 多线程扩展性测试
#
# 测试 scan / compute / merge 三阶段分别随线程数的加速比
#
# 用法:
#   ./bench_scaling.sh
#
# 修改下面的配置区适配你的环境
#

set -euo pipefail

# ============================================================================
# 配置区 — 修改这里
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

MINIGU="$PROJECT_DIR/target/release/minigu"

# 数据库路径（包含已导入的 LDBC 图）
DB_PATH="$PROJECT_DIR/experiment/dataset/ldbc/sf1/minigu_db"

# 图名
GRAPH_NAME="ldbc"

# 最大路径长度
MAX_K=2

# Mode: 1 = neighbor-cached dependency-driven
MODE=1

# 要测试的线程数
THREAD_COUNTS=(1 2 4 8 12 16 20 24 32)

# ============================================================================

RESULT_DIR="$PROJECT_DIR/experiment/result/scaling"
mkdir -p "$RESULT_DIR"
OUTPUT_FILE="$RESULT_DIR/scaling_result.txt"
SUMMARY_FILE="$RESULT_DIR/scaling_summary.csv"

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

# Check binary
if [[ ! -x "$MINIGU" ]]; then
    echo "ERROR: minigu binary not found at $MINIGU"
    echo "Run: cargo build --release --features std,serde,miette --bin=minigu"
    exit 1
fi

# Check database
if [[ ! -d "$DB_PATH" ]]; then
    echo "ERROR: database not found at $DB_PATH"
    exit 1
fi

log "Starting scaling benchmark"
log "Database: $DB_PATH"
log "Thread counts: ${THREAD_COUNTS[*]}"
log "Output: $OUTPUT_FILE"

# CSV header
echo "threads,scan_s,convert_s,compute_s,merge_s,total_s,catalog_build_s" > "$SUMMARY_FILE"

# Clear output file
> "$OUTPUT_FILE"

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

for T in "${THREAD_COUNTS[@]}"; do
    log "=== threads=$T ==="

    # Build GQL script
    script_file="$TMP_DIR/bench_t${T}.gql"
    cat > "$script_file" <<EOGQL
session set graph ${GRAPH_NAME}
call create_catalog("${GRAPH_NAME}", ${MAX_K}, ${MODE}, ${T})
EOGQL

    # Run
    stdout_file="$TMP_DIR/out_t${T}.txt"
    "$MINIGU" execute "$script_file" --path "$DB_PATH" 2>&1 | tee "$stdout_file"

    # Append to combined output
    echo "========== threads=$T ==========" >> "$OUTPUT_FILE"
    cat "$stdout_file" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"

    # Parse Mode1 timing line:
    #   [Mode1 timing] scan=X.XXXs | convert=X.XXXs | compute=X.XXXs | merge=X.XXXs | total=X.XXXs
    timing_line=$(grep '\[Mode1 timing\]' "$stdout_file" || echo "")
    if [[ -n "$timing_line" ]]; then
        scan_s=$(echo "$timing_line" | grep -o 'scan=[0-9.]*' | cut -d= -f2)
        convert_s=$(echo "$timing_line" | grep -o 'convert=[0-9.]*' | cut -d= -f2)
        compute_s=$(echo "$timing_line" | grep -o 'compute=[0-9.]*' | cut -d= -f2)
        merge_s=$(echo "$timing_line" | grep -o 'merge=[0-9.]*' | cut -d= -f2)
        total_s=$(echo "$timing_line" | grep -o 'total=[0-9.]*' | cut -d= -f2)
    else
        scan_s=0; convert_s=0; compute_s=0; merge_s=0; total_s=0
    fi

    # Parse catalog build time:
    #   Catalog build time: X.XXXs
    build_line=$(grep 'Catalog build time:' "$stdout_file" || echo "")
    if [[ -n "$build_line" ]]; then
        catalog_build_s=$(echo "$build_line" | grep -o '[0-9.]*s' | head -1 | tr -d 's')
    else
        catalog_build_s=0
    fi

    echo "$T,$scan_s,$convert_s,$compute_s,$merge_s,$total_s,$catalog_build_s" >> "$SUMMARY_FILE"
    log "  threads=$T: scan=${scan_s}s convert=${convert_s}s compute=${compute_s}s merge=${merge_s}s total=${total_s}s build=${catalog_build_s}s"
done

log "Benchmark complete."
log "Full output: $OUTPUT_FILE"
log "Summary: $SUMMARY_FILE"

echo ""
echo "=== Summary ==="
column -t -s',' "$SUMMARY_FILE"
