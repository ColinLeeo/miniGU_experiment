#!/bin/bash
#
# sync_remote.sh — 本地开发 <-> 远程测试 同步脚本
#
# 用法:
#   ./sync_remote.sh sync              # 同步代码到远程
#   ./sync_remote.sh sync --dry-run    # 预览将同步的文件（不实际传输）
#   ./sync_remote.sh build             # 同步 + 远程编译
#   ./sync_remote.sh test              # 同步 + 远程运行测试
#   ./sync_remote.sh ssh               # 直接 SSH 到远程机器
#   ./sync_remote.sh pull <远程路径>    # 从远程拉取文件/目录到本地对应位置
#   ./sync_remote.sh diff              # 显示本地 git 变更的文件列表
#   ./sync_remote.sh sync-changed      # 仅同步 git 变更的文件（增量同步，更快）
#
# 首次使用前请修改下面的配置项
#

set -euo pipefail

# ============================================================
# 配置区 — 请根据实际情况修改
# ============================================================
REMOTE_HOST="newDev"                       # SSH config 中的 Host 别名
REMOTE_DIR="/home/shuolin/miniGU"              # 远程项目目录
LOCAL_DIR="$(cd "$(dirname "$0")/.." && pwd)"   # 本地项目根目录（脚本上一级）

# SSH 选项（复用连接，加速多次操作；用户名/端口/跳板机由 ~/.ssh/config 管理）
SSH_CONTROL_PATH="/tmp/ssh-minigu-%r@%h:%p"
SSH_OPTS="-o ControlMaster=auto -o ControlPath=${SSH_CONTROL_PATH} -o ControlPersist=600"

# 远程编译命令
BUILD_CMD="cd ${REMOTE_DIR} && cargo build --release --bin=minigu"

# 远程测试命令
TEST_CMD="cd ${REMOTE_DIR} && cargo test 2>&1 | tail -100"

# ============================================================
# rsync 排除规则（基于 .gitignore + 额外排除）
# ============================================================
RSYNC_EXCLUDES=(
    # 版本控制
    ".git/"

    # IDE / 编辑器
    ".vscode/"
    ".idea/"
    "*.DS_Store"

    # Rust / Cargo 构建产物
    "target/"

    # 实验数据（大文件，远程应已有或单独准备）
    "experiment/dataset/ldbc/sf0.3/"
    "experiment/dataset/ldbc/sf0.1/"
    "experiment/dataset/ldbc/sf1/"
    "experiment/dataset/ldbc/sf3/"
    "experiment/dataset/stats_ceb/"

    # load_ldbc 生成的大型缓存文件（远程自动生成，无需同步）
    "*.lightgraph.bin"
    "*.scanned_hops.bin"

    # 实验结果
    "experiment/result/"

    # baseline（含 boost 等大量源码，远程单独构建）
    "experiment/baseline/"
    "experiment/dataset/ldbc/*.bin"
    "experiment/dataset/ldbc/sf1_pipe/"

    # baseline 准备的数据产物
    "experiment/catalogs/"
    "experiment/graphs/"

    # 杂项
    "experiment/duckdb"
    "*.log"
)

# ============================================================
# 函数
# ============================================================

_ssh() {
    ssh ${SSH_OPTS} "${REMOTE_HOST}" "$@"
}

_build_exclude_args() {
    local args=()
    for pattern in "${RSYNC_EXCLUDES[@]}"; do
        args+=(--exclude "$pattern")
    done
    echo "${args[@]}"
}

cmd_sync() {
    echo ">>> 同步代码到 ${REMOTE_HOST}:${REMOTE_DIR}"
    echo "    本地: ${LOCAL_DIR}"

    # 确保远程目录存在
    _ssh "mkdir -p ${REMOTE_DIR}"

    rsync -avz --delete \
        -e "ssh ${SSH_OPTS}" \
        $(_build_exclude_args) \
        "$@" \
        "${LOCAL_DIR}/" \
        "${REMOTE_HOST}:${REMOTE_DIR}/"

    echo ">>> 同步完成"
}

cmd_sync_changed() {
    echo ">>> 仅同步 git 变更的文件"

    # 获取相对于工作树根目录的变更文件列表（已修改 + 未跟踪）
    cd "${LOCAL_DIR}"
    local changed_files
    changed_files=$({ git diff --name-only HEAD 2>/dev/null; git diff --name-only 2>/dev/null; git diff --name-only --cached 2>/dev/null; } | sort -u || true)
    local untracked_files
    untracked_files=$(git ls-files --others --exclude-standard 2>/dev/null || true)

    local all_files
    all_files=$(echo -e "${changed_files}\n${untracked_files}" | sort -u | grep -v '^$' || true)

    if [ -z "$all_files" ]; then
        echo "    没有检测到变更文件"
        return 0
    fi

    echo "    变更文件:"
    echo "$all_files" | sed 's/^/      /'

    # 确保远程目录存在
    _ssh "mkdir -p ${REMOTE_DIR}"

    # 逐文件同步
    echo "$all_files" | while IFS= read -r file; do
        if [ -f "${LOCAL_DIR}/${file}" ]; then
            # 确保远程子目录存在
            local remote_subdir
            remote_subdir=$(dirname "$file")
            if [ "$remote_subdir" != "." ]; then
                _ssh "mkdir -p ${REMOTE_DIR}/${remote_subdir}"
            fi
            rsync -avz -e "ssh ${SSH_OPTS}" \
                "${LOCAL_DIR}/${file}" \
                "${REMOTE_HOST}:${REMOTE_DIR}/${file}"
        fi
    done

    echo ">>> 增量同步完成"
}

cmd_build() {
    cmd_sync
    echo ""
    echo ">>> 远程编译..."
    _ssh "${BUILD_CMD}"
}

cmd_test() {
    cmd_sync
    echo ""
    echo ">>> 远程测试..."
    _ssh "${TEST_CMD}"
}

cmd_ssh() {
    echo ">>> SSH 连接到 ${REMOTE_HOST}"
    _ssh
}

cmd_pull() {
    local remote_path="$1"
    echo ">>> 从远程拉取: ${remote_path}"
    rsync -avz \
        -e "ssh ${SSH_OPTS}" \
        "${REMOTE_HOST}:${REMOTE_DIR}/${remote_path}" \
        "${LOCAL_DIR}/${remote_path}"
    echo ">>> 拉取完成"
}

cmd_diff() {
    cd "${LOCAL_DIR}"
    echo ">>> 本地 git 变更文件:"
    git diff --stat HEAD 2>/dev/null || true
    echo ""
    echo ">>> 未跟踪文件:"
    git ls-files --others --exclude-standard 2>/dev/null || true
}

cmd_help() {
    cat <<'HELP'
用法: ./sync_remote.sh <命令> [参数]

命令:
  sync              全量同步代码到远程（rsync，自动跳过构建产物）
  sync --dry-run    预览同步（不实际传输）
  sync-changed      仅同步 git 变更的文件（增量，更快）
  build             同步 + 远程编译
  test              同步 + 远程测试
  ssh               SSH 登录远程机器
  pull <路径>       从远程拉取指定文件/目录到本地
  diff              查看本地 git 变更
  help              显示此帮助

示例:
  ./sync_remote.sh sync                          # 全量同步
  ./sync_remote.sh sync-changed                  # 只同步改动文件
  ./sync_remote.sh build                         # 同步并编译
  ./sync_remote.sh pull src/some/test/result.log # 拉取远程测试结果
  ./sync_remote.sh ssh                           # 登录远程调试
HELP
}

# ============================================================
# 入口
# ============================================================
case "${1:-help}" in
    sync)         shift; cmd_sync "$@" ;;
    sync-changed) cmd_sync_changed ;;
    build)        cmd_build ;;
    test)         cmd_test ;;
    ssh)          cmd_ssh ;;
    pull)         shift; cmd_pull "$@" ;;
    diff)         cmd_diff ;;
    help|*)       cmd_help ;;
esac
