#!/bin/bash
#
# run_insert_impact.sh — 更新实验：衡量 random_insert 对 GCard 估计的影响
#
# 实验设计：
#   - Pattern：experiment/patterns/gcard/lsqb 下 8 个 LSQB 查询 (q1-q9 去 q7)
#   - 每个查询从 JSON 顺序抽边，过滤 LDBC N:1 约束边后得到候选；
#     middle = 候选首条；endpoint = 候选末条；若候选只剩 1 条则两者合并为 1 个位置
#   - 谓词变体：experiment/patterns/gcard/lsqb_glogs_with_pred_10/lsqb 下
#     每个查询随机抽 NUM_PRED_VARIANTS 个 _NN.json 变体
#   - 4 个独立更新百分比 (1/2/5/10%)，从原图直达不累积
#
# 关键：整个实验在单个 minigu 进程内完成，图只加载一次。
# 通过 gcard_snapshot() 记录原图，每个百分比前 gcard_restore() 在内存里重置，
# 避免反复从磁盘重新加载。

set -euo pipefail

if [ $# -lt 2 ]; then
    echo "用法: $0 <db_path> <dataset_dir>"
    exit 1
fi

DB_PATH="$(cd "$1" && pwd)"
DATASET_DIR="$(cd "$2" && pwd)"

PROJ_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MINIGU_BIN="${PROJ_ROOT}/target/release/minigu"
PATTERN_NP_DIR="${PROJ_ROOT}/experiment/patterns/gcard/lsqb"
PATTERN_P_DIR="${PROJ_ROOT}/experiment/patterns/gcard/lsqb_glogs_with_pred_10/lsqb"
RESULT_DIR="${PROJ_ROOT}/experiment/result/insert_impact"
RESULT_CSV="${RESULT_DIR}/insert_impact_results.csv"
TRUE_CARD_SCRIPT="${PROJ_ROOT}/experiment/scripts/true_cardinality.py"

GRAPH_NAME=ldbc
BASE_SEED=42
K=2
SAMPLE_SIZE=500
PRED_MODE=0
NUM_PRED_VARIANTS=1

# 独立更新百分比（非累积），及对应目录标签
PERCENTAGES=(0.01 0.02 0.05 0.10 0.20 0.40)
PCT_LABELS=(1 2 5 10 20 40)

# LDBC schema 中的 N:1 / N:0..1 约束边 —— random_insert 均匀抽样会破坏约束，
# 估计与真实基数变化模式都失真，所以从可插入候选中剔除。
N_TO_1_EDGES=(
    "tag_hastype_tagclass"
    "tagclass_issubclassof_tagclass"
    "person_islocatedin_city"
    "comment_islocatedin_country"
    "post_islocatedin_country"
    "company_islocatedin_country"
    "university_islocatedin_city"
    "city_ispartof_country"
    "country_ispartof_continent"
    "post_hascreator_person"
    "comment_hascreator_person"
    "comment_replyof_post"
    "comment_replyof_comment"
    "forum_containerof_post"
    "forum_hasmoderator_person"
)

BASE_QUERIES=(q1 q2 q3 q4 q5 q6 q8 q9)

if [ ! -f "${MINIGU_BIN}" ]; then
    echo "错误: 未找到 ${MINIGU_BIN}"
    exit 1
fi

HAS_DUCKDB=false
if python3 -c "import duckdb" 2>/dev/null; then
    HAS_DUCKDB=true
    echo ">>> DuckDB 可用，将自动计算真实基数和 Q-error"
else
    echo ">>> 警告: DuckDB 不可用 (pip install duckdb)，跳过真实基数计算"
fi

mkdir -p "${RESULT_DIR}"

# ── 备份 / 恢复原始 DB 文件（仅在实验开始前恢复一次，保证干净起点）──
BACKUP_DIR="${RESULT_DIR}/.backup"
if [ ! -d "${BACKUP_DIR}" ]; then
    echo ">>> 备份原始数据到 ${BACKUP_DIR}"
    mkdir -p "${BACKUP_DIR}"
    for f in "${DB_PATH}"/*; do
        [ -f "$f" ] && cp "$f" "${BACKUP_DIR}/"
    done
else
    echo ">>> 备份已存在: ${BACKUP_DIR}"
fi

restore_original() {
    echo ">>> 恢复原始 DB 文件..."
    for f in "${BACKUP_DIR}"/*; do
        [ -f "$f" ] && cp "$f" "${DB_PATH}/"
    done
}

# 拼一个临时数据目录：原始数据集 CSV 全部 symlink，仅被更新的边 label CSV 用真实文件覆盖
assemble_data_dir() {
    local out_dir="$1"
    local updated_label="$2"
    local updated_csv="$3"
    rm -rf "${out_dir}"
    mkdir -p "${out_dir}"
    for csv in "${DATASET_DIR}"/*.csv; do
        [ -f "$csv" ] || continue
        ln -s "$csv" "${out_dir}/$(basename "$csv")"
    done
    local target="${out_dir}/${updated_label}.csv"
    rm -f "${target}"
    cp "${updated_csv}" "${target}"
}

# DuckDB 真实基数计算超时阈值（秒）；超时返回 TIMEOUT 标记
TRUE_CARD_TIMEOUT=${TRUE_CARD_TIMEOUT:-60}

compute_true_card() {
    local data_dir="$1"
    local query_json="$2"
    local out rc
    out=$(timeout "${TRUE_CARD_TIMEOUT}s" python3 "${TRUE_CARD_SCRIPT}" \
        "${data_dir}" "${query_json}" 2>>"${STDERR_LOG}")
    rc=$?
    if [ $rc -eq 124 ]; then
        echo "TIMEOUT"
        return 0
    fi
    if [ $rc -ne 0 ]; then
        echo "ERR"
        return 0
    fi
    echo "$out" | sed -n 's/.*: \([0-9][0-9]*\).*/\1/p' | head -1
}

qerror() {
    local est="$1"
    local tru="$2"
    python3 -c "
e, t = $est, $tru
if t == 0 and e == 0:
    print('1.0')
elif t == 0:
    print('inf')
elif e == 0:
    print(str(t))
else:
    print(f'{max(e/t, t/e):.4f}')
"
}

# 从一行 gcard_query 输出里抽取字段（cardinality / estimate_time / build_time）
card_field() {
    local line="$1"
    local key="$2"
    echo "$line" | sed -n "s/.*${key}: \([0-9.][0-9.]*\).*/\1/p" | head -1
}

# 从 query JSON 顺序抽出可插入边（过滤 N:1 黑名单 + 去重），输出格式：
#   "<middle_edge>\t<endpoint_edge>\t<n_candidates>"  或者空串（候选=0）
extract_insertable_edges() {
    local json="$1"
    local blacklist_csv
    blacklist_csv=$(IFS=,; echo "${N_TO_1_EDGES[*]}")
    python3 - "$json" "$blacklist_csv" <<'PYEOF'
import json, sys
fn, blacklist_csv = sys.argv[1], sys.argv[2]
blacklist = set(blacklist_csv.split(','))
with open(fn) as f:
    data = json.load(f)
labels, seen = [], set()
for e in data.get('edges', []):
    lab = e.get('label')
    if not lab or lab in blacklist or lab in seen:
        continue
    seen.add(lab)
    labels.append(lab)
if not labels:
    print("")
elif len(labels) == 1:
    print(f"{labels[0]}\t{labels[0]}\t1")
else:
    print(f"{labels[0]}\t{labels[-1]}\t{len(labels)}")
PYEOF
}

# 为 base query 随机抽 NUM_PRED_VARIANTS 个谓词变体（_NN.json），输出 TAB 分隔
sample_pred_variants() {
    local base="$1"
    local n="$2"
    python3 - "${PATTERN_P_DIR}" "$base" "$n" "$BASE_SEED" <<'PYEOF'
import os, sys, random
pdir, base, n, seed = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
candidates = sorted(
    f for f in os.listdir(pdir)
    if f.startswith(f"{base}_") and f.endswith(".json")
)
random.seed(seed + (hash(base) & 0xffff))
pick = random.sample(candidates, min(n, len(candidates)))
print("\t".join(pick))
PYEOF
}

# ── 构建 EXPERIMENTS 数组 ────────────────────────────────────────────
# 每条 entry: "pattern|middle_edge|endpoint_edge|pred_variant_csv"
EXPERIMENTS=()
for q in "${BASE_QUERIES[@]}"; do
    NP_JSON="${PATTERN_NP_DIR}/${q}.json"
    if [ ! -f "$NP_JSON" ]; then
        echo ">>> WARN: ${NP_JSON} 不存在，跳过 ${q}"
        continue
    fi
    EXT=$(extract_insertable_edges "$NP_JSON")
    if [ -z "$EXT" ]; then
        echo ">>> WARN: ${q} 所有边都是 N:1 约束，跳过"
        continue
    fi
    IFS=$'\t' read -r MIDDLE_EDGE ENDPOINT_EDGE NCAND <<< "$EXT"
    VARIANTS_TSV=$(sample_pred_variants "$q" "$NUM_PRED_VARIANTS")
    if [ -z "$VARIANTS_TSV" ]; then
        echo ">>> WARN: ${q} 没有可用谓词变体，跳过"
        continue
    fi
    PRED_CSV=$(echo "$VARIANTS_TSV" | tr '\t' ',')
    EXPERIMENTS+=("${q}|${MIDDLE_EDGE}|${ENDPOINT_EDGE}|${PRED_CSV}")
    echo ">>> ${q}: middle=${MIDDLE_EDGE}, endpoint=${ENDPOINT_EDGE}, n_cand=${NCAND}, pred=${PRED_CSV}"
done

if [ "${#EXPERIMENTS[@]}" -eq 0 ]; then
    echo "错误: 没有可执行的 query"
    exit 1
fi

restore_original

# ── 生成单个 GQL 脚本（整个实验一次跑完）──────────────────────────────
GQL_SCRIPT="${RESULT_DIR}/experiment.gql"
{
    echo "session set graph ${GRAPH_NAME}"
    echo "call gcard_snapshot()"
} > "${GQL_SCRIPT}"

for exp in "${EXPERIMENTS[@]}"; do
    IFS='|' read -r PATTERN MIDDLE_EDGE ENDPOINT_EDGE PRED_CSV <<< "$exp"
    NP_JSON="${PATTERN_NP_DIR}/${PATTERN}.json"
    IFS=',' read -r -a PRED_FILES <<< "$PRED_CSV"

    POSITIONS=(middle)
    [ "$MIDDLE_EDGE" != "$ENDPOINT_EDGE" ] && POSITIONS+=(endpoint)

    for POSITION in "${POSITIONS[@]}"; do
        if [ "$POSITION" = "middle" ]; then
            TARGET_EDGE="$MIDDLE_EDGE"
        else
            TARGET_EDGE="$ENDPOINT_EDGE"
        fi
        EXP_DIR="${RESULT_DIR}/${PATTERN}/${POSITION}"

        # baseline: 原图直接查询（1 无谓词 + NUM_PRED_VARIANTS 个有谓词）
        {
            echo "-- ${PATTERN}/${POSITION} baseline"
            echo "call gcard_restore()"
            echo "call gcard_query('${NP_JSON}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)"
            for pred_f in "${PRED_FILES[@]}"; do
                echo "call gcard_query('${PATTERN_P_DIR}/${pred_f}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)"
            done
        } >> "${GQL_SCRIPT}"

        # 4 个独立百分比
        for k in "${!PERCENTAGES[@]}"; do
            PCT="${PERCENTAGES[$k]}"
            LABEL="${PCT_LABELS[$k]}"
            SEED=$((BASE_SEED + k))
            CSV_OUT="${EXP_DIR}/pct${LABEL}/${TARGET_EDGE}.csv"
            mkdir -p "${EXP_DIR}/pct${LABEL}"
            {
                echo "-- ${PATTERN}/${POSITION} pct=${PCT}"
                echo "call gcard_restore()"
                echo "call random_insert(${PCT}, '', ${SEED}, '', '${TARGET_EDGE}')"
                echo "call gcard_query('${NP_JSON}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)"
                for pred_f in "${PRED_FILES[@]}"; do
                    echo "call gcard_query('${PATTERN_P_DIR}/${pred_f}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)"
                done
                echo "call export_edge_csv('${TARGET_EDGE}', '${CSV_OUT}')"
            } >> "${GQL_SCRIPT}"
        done
    done
done
echo ":quit" >> "${GQL_SCRIPT}"

# ── 执行（单进程，图只加载一次）────────────────────────────────────────
STDOUT_LOG="${RESULT_DIR}/stdout.log"
STDERR_LOG="${RESULT_DIR}/stderr.log"
echo ">>> 执行 ${GQL_SCRIPT}（单 minigu 进程）"
"${MINIGU_BIN}" execute "${GQL_SCRIPT}" --path "${DB_PATH}" \
    > "${STDOUT_LOG}" 2> "${STDERR_LOG}" || {
    echo "错误: 实验执行失败，查看 ${STDERR_LOG}"
    tail -30 "${STDERR_LOG}"
    exit 1
}

# ── 解析输出 ───────────────────────────────────────────────────────────
mapfile -t CARD_LINES   < <(grep 'cardinality:' "${STDOUT_LOG}" || true)
mapfile -t INSERTED_LINES < <(grep -E '^[0-9]+,[0-9]+$' "${STDOUT_LOG}" || true)
mapfile -t INSERT_TIMES < <(grep -oE '\[random_insert\] total: [0-9.]+s' "${STDERR_LOG}" \
                            | grep -oE '[0-9.]+' || true)

# 每个 (pattern, position) group 行数：
#   baseline 块 + 4 个百分比块 = 5 个块
#   每块 1 个无谓词 query + NUM_PRED_VARIANTS 个有谓词 query = (1 + N) 行
QPER=$((1 + NUM_PRED_VARIANTS))
GROUP_CARDS=$(( (1 + ${#PERCENTAGES[@]}) * QPER ))
GROUP_INS=${#PERCENTAGES[@]}

NUM_GROUPS=0
for exp in "${EXPERIMENTS[@]}"; do
    IFS='|' read -r _ M E _ <<< "$exp"
    NUM_GROUPS=$(( NUM_GROUPS + 1 ))
    [ "$M" != "$E" ] && NUM_GROUPS=$(( NUM_GROUPS + 1 ))
done
EXP_CARD=$(( NUM_GROUPS * GROUP_CARDS ))
EXP_INS=$(( NUM_GROUPS * GROUP_INS ))
echo ">>> 解析: cardinality 行 ${#CARD_LINES[@]}/${EXP_CARD}, inserted 行 ${#INSERTED_LINES[@]}/${EXP_INS}, insert 计时 ${#INSERT_TIMES[@]}/${EXP_INS}"
if [ "${#CARD_LINES[@]}" -ne "${EXP_CARD}" ]; then
    echo ">>> 警告: cardinality 行数不符，结果可能错位"
fi

echo "pattern,query_variant,query_type,position,update_pct,edge_label,inserted_edges,estimated_card,true_card,qerror,insert_time_s,build_time_s,estimate_time_s,query_time_s" > "${RESULT_CSV}"

declare -A BASELINE_TRUE_NP
declare -A BASELINE_TRUE_P

emit_row() {
    local pattern="$1" qvar="$2" qtype="$3" position="$4" upct="$5" edge="$6"
    local inserted="$7" card_line="$8" true_c="$9" insert_t="${10}"
    local est build est_t qerr qt
    est=$(card_field "$card_line" "cardinality")
    build=$(card_field "$card_line" "build_time")
    est_t=$(card_field "$card_line" "estimate_time")
    est="${est:-0}"; build="${build:-0}"; est_t="${est_t:-0}"
    qt=$(python3 -c "print(f'{${build} + ${est_t}:.6f}')")
    if [ "$true_c" = "N/A" ] || [ "$true_c" = "ERR" ] || [ "$true_c" = "TIMEOUT" ]; then
        qerr="$true_c"
    else
        qerr=$(qerror "$est" "$true_c")
    fi
    echo "${pattern},${qvar},${qtype},${position},${upct},${edge},${inserted},${est},${true_c},${qerr},${insert_t},${build},${est_t},${qt}" >> "${RESULT_CSV}"
}

g=0
for exp in "${EXPERIMENTS[@]}"; do
    IFS='|' read -r PATTERN MIDDLE_EDGE ENDPOINT_EDGE PRED_CSV <<< "$exp"
    NP_JSON="${PATTERN_NP_DIR}/${PATTERN}.json"
    IFS=',' read -r -a PRED_FILES <<< "$PRED_CSV"

    # baseline 真实基数按 pattern + variant 缓存
    if [ "$HAS_DUCKDB" = true ] && [ -z "${BASELINE_TRUE_NP[$PATTERN]:-}" ]; then
        BASELINE_TRUE_NP[$PATTERN]=$(compute_true_card "${DATASET_DIR}" "${NP_JSON}" || echo "ERR")
        [ -z "${BASELINE_TRUE_NP[$PATTERN]}" ] && BASELINE_TRUE_NP[$PATTERN]="ERR"
        for pred_f in "${PRED_FILES[@]}"; do
            vkey="${PATTERN}::${pred_f%.json}"
            BASELINE_TRUE_P[$vkey]=$(compute_true_card "${DATASET_DIR}" "${PATTERN_P_DIR}/${pred_f}" || echo "ERR")
            [ -z "${BASELINE_TRUE_P[$vkey]}" ] && BASELINE_TRUE_P[$vkey]="ERR"
        done
    fi

    POSITIONS=(middle)
    [ "$MIDDLE_EDGE" != "$ENDPOINT_EDGE" ] && POSITIONS+=(endpoint)

    for POSITION in "${POSITIONS[@]}"; do
        if [ "$POSITION" = "middle" ]; then
            TARGET_EDGE="$MIDDLE_EDGE"
        else
            TARGET_EDGE="$ENDPOINT_EDGE"
        fi
        EXP_DIR="${RESULT_DIR}/${PATTERN}/${POSITION}"

        CBASE=$(( g * GROUP_CARDS ))
        IBASE=$(( g * GROUP_INS ))

        echo ""
        echo ">>> ${PATTERN} / ${POSITION} / edge=${TARGET_EDGE}"

        # ── baseline 块 ──
        emit_row "$PATTERN" "base" no_pred "$POSITION" 0 "$TARGET_EDGE" 0 \
            "${CARD_LINES[$CBASE]:-}" "${BASELINE_TRUE_NP[$PATTERN]:-N/A}" 0
        for i in "${!PRED_FILES[@]}"; do
            pred_f="${PRED_FILES[$i]}"
            v="${pred_f%.json}"
            emit_row "$PATTERN" "$v" with_pred "$POSITION" 0 "$TARGET_EDGE" 0 \
                "${CARD_LINES[$((CBASE+1+i))]:-}" "${BASELINE_TRUE_P[${PATTERN}::${v}]:-N/A}" 0
        done

        # ── 4 个百分比块 ──
        for k in "${!PERCENTAGES[@]}"; do
            PCT="${PERCENTAGES[$k]}"
            LABEL="${PCT_LABELS[$k]}"
            BLOCK_START=$(( CBASE + (1+k) * QPER ))
            INSERTED_EDGES=$(echo "${INSERTED_LINES[$((IBASE + k))]:-0,0}" | cut -d',' -f1)
            INSERT_T="${INSERT_TIMES[$((IBASE + k))]:-0}"

            TRUE_NP="N/A"
            declare -A TRUE_P_BY_V=()
            for pred_f in "${PRED_FILES[@]}"; do
                TRUE_P_BY_V[${pred_f%.json}]="N/A"
            done
            EDGE_CSV="${EXP_DIR}/pct${LABEL}/${TARGET_EDGE}.csv"
            if [ "$HAS_DUCKDB" = true ] && [ -f "${EDGE_CSV}" ]; then
                DATA_DIR="${EXP_DIR}/pct${LABEL}/_data"
                assemble_data_dir "${DATA_DIR}" "${TARGET_EDGE}" "${EDGE_CSV}"
                TRUE_NP=$(compute_true_card "${DATA_DIR}" "${NP_JSON}" || echo "ERR")
                [ -z "$TRUE_NP" ] && TRUE_NP="ERR"
                for pred_f in "${PRED_FILES[@]}"; do
                    v="${pred_f%.json}"
                    TRUE_P_BY_V[$v]=$(compute_true_card "${DATA_DIR}" "${PATTERN_P_DIR}/${pred_f}" || echo "ERR")
                    [ -z "${TRUE_P_BY_V[$v]}" ] && TRUE_P_BY_V[$v]="ERR"
                done
                rm -rf "${DATA_DIR}"
            fi

            emit_row "$PATTERN" "base" no_pred "$POSITION" "$PCT" "$TARGET_EDGE" \
                "$INSERTED_EDGES" "${CARD_LINES[$BLOCK_START]:-}" "$TRUE_NP" "$INSERT_T"
            for i in "${!PRED_FILES[@]}"; do
                pred_f="${PRED_FILES[$i]}"
                v="${pred_f%.json}"
                emit_row "$PATTERN" "$v" with_pred "$POSITION" "$PCT" "$TARGET_EDGE" \
                    "$INSERTED_EDGES" "${CARD_LINES[$((BLOCK_START+1+i))]:-}" \
                    "${TRUE_P_BY_V[$v]}" "$INSERT_T"
            done
            unset TRUE_P_BY_V
            echo "  pct=${PCT}: ins=${INSERTED_EDGES}, t_ins=${INSERT_T}s, true(np)=${TRUE_NP}"
        done

        g=$(( g + 1 ))
    done
done

echo ""
echo "============================================================"
echo ">>> 实验完成！结果保存在:"
echo "    ${RESULT_CSV}"
echo "    候选抽象图 + 各 candidate 基数详见 ${STDOUT_LOG}"
echo "      grep '\\[gcard-cand\\]' ${STDOUT_LOG}  (所有候选)"
echo "      grep '\\[gcard-cand-min\\]' ${STDOUT_LOG}  (取 min 选中)"
echo "============================================================"
