#!/usr/bin/env bash
#
# 远程 benchmark 自动化：同步 → 编译 → 运行 bench_catalog → 拉取结果
#
# Usage:
#   ./run_remote_bench.sh [--sfs "sf3"] [--threads "4 8 16 32"] [--repeats 3] [--max-k 2]
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SYNC_SCRIPT="$PROJECT_DIR/experiment/sync_remote.sh"

# Remote config (must match sync_remote.sh)
REMOTE_HOST="basics-dev"
REMOTE_DIR="/home/shuolin/miniGU"
SSH_CONTROL_PATH="/tmp/ssh-minigu-%r@%h:%p"
SSH_OPTS="-o ControlMaster=auto -o ControlPath=${SSH_CONTROL_PATH} -o ControlPersist=600"

# Benchmark defaults
SFS="sf3"
THREADS="16 32"
REPEATS=1
MAX_K=2

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sfs)      SFS="$2";      shift 2 ;;
        --threads)  THREADS="$2";   shift 2 ;;
        --repeats)  REPEATS="$2";   shift 2 ;;
        --max-k)    MAX_K="$2";     shift 2 ;;
        *) echo "Unknown: $1"; exit 1 ;;
    esac
done

_ssh() {
    ssh ${SSH_OPTS} "${REMOTE_HOST}" "$@"
}

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

# Step 1: Sync code (full sync to ensure consistency)
log "=== Step 1: Syncing code ==="
"$SYNC_SCRIPT" sync

# Step 2: Remote build (release)
log "=== Step 2: Remote build ==="
_ssh "source ~/.cargo/env && cd ${REMOTE_DIR} && touch minigu/core/src/procedures/gcard_query/degree_compute_dense.rs && cargo build --release --bin=minigu --features std,serde,miette 2>&1 | tail -5"

# Step 3: Run benchmark
log "=== Step 3: Running benchmark ==="
BENCH_CMD="cd ${REMOTE_DIR}/experiment/scripts/build_catalog_bench && bash bench_catalog.sh --sfs \"${SFS}\" --threads \"${THREADS}\" --repeats ${REPEATS} --max-k ${MAX_K}"
_ssh "${BENCH_CMD}" 2>&1 | tee /tmp/remote_bench_output.txt

# Step 4: Pull results
log "=== Step 4: Pulling results ==="
"$SYNC_SCRIPT" pull experiment/result/build_catalog/

log "=== Done ==="
log "Results:"
RESULT_FILE="$PROJECT_DIR/experiment/result/build_catalog/bench_catalog_results.csv"
if [[ -f "$RESULT_FILE" ]]; then
    column -t -s',' "$RESULT_FILE"
fi
