#!/bin/bash
#
# run_insert_impact.sh — 更新实验：衡量 random_insert 对 GCard 估计的影响
#
# 对每个无谓词查询 (L1-L6)，分别在中间边/端侧边做 5 轮 0.01 插入，
# 每轮记录：估计值、真实基数、Q-error、插入耗时，并导出目标边 CSV。

set -euo pipefail

if [ $# -lt 2 ]; then
    echo "用法: $0 <db_path> <dataset_dir>"
    exit 1
fi

DB_PATH="$(cd "$1" && pwd)"
DATASET_DIR="$(cd "$2" && pwd)"

PROJ_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MINIGU_BIN="${PROJ_ROOT}/target/release/minigu"
PATTERN_DIR="${PROJ_ROOT}/experiment/pattern/LDBC"
RESULT_DIR="${PROJ_ROOT}/experiment/result/insert_impact"
RESULT_CSV="${RESULT_DIR}/insert_impact_results.csv"
TRUE_CARD_SCRIPT="${PROJ_ROOT}/experiment/scripts/true_cardinality.py"

NUM_ROUNDS=5
RATIO=0.01
BASE_SEED=42
K=2
SAMPLE_SIZE=500
PRED_MODE=0

# 格式: PATTERN_NAME|JSON_NO_PRED|JSON_PRED|MIDDLE_EDGE|ENDPOINT_EDGE
EXPERIMENTS=(
    "L1|L1_chain/L1.json|L1_chain/L1_PB.json|forum_containerof_post|tag_hastype_tagclass"
    "L2|L2_cycle/L2.json|L2_cycle/L2_PA.json|person_knows_person|comment_replyof_post"
    "L3|L3_triangle/L3.json|L3_triangle/L3_PB.json|person_knows_person|city_ispartof_country"
    "L4|L4_star/L4.json|L4_star/L4_PB.json|post_hascreator_person|post_hastag_tag"
    "L5|L5_path/L5.json|L5_path/L5_PA.json|person_knows_person|person_hasinterest_tag"
    "L6|L6_cycle_branch/L6.json|L6_cycle_branch/L6_PA.json|comment_replyof_comment|person_knows_person"
)

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

BACKUP_DIR="${RESULT_DIR}/.backup"
if [ ! -d "${BACKUP_DIR}" ]; then
    echo ">>> 备份原始数据到 ${BACKUP_DIR}"
    mkdir -p "${BACKUP_DIR}"
    for f in "${DB_PATH}"/*.bin; do
        [ -f "$f" ] && cp "$f" "${BACKUP_DIR}/"
    done
    [ -f "${DB_PATH}/catalog.json" ] && cp "${DB_PATH}/catalog.json" "${BACKUP_DIR}/"
    echo ">>> 备份完成"
fi

restore_original() {
    echo ">>> 恢复原始数据..."
    for f in "${BACKUP_DIR}"/*; do
        [ -f "$f" ] && cp "$f" "${DB_PATH}/"
    done
    echo ">>> 恢复完成"
}

assemble_data_dir() {
    local out_dir="$1"
    local updated_label="$2"
    local updated_csv="$3"
    rm -rf "${out_dir}"
    mkdir -p "${out_dir}"
    for csv in "${DATASET_DIR}"/*.csv; do
        [ -f "$csv" ] || continue
        local fname
        fname=$(basename "$csv")
        ln -s "$csv" "${out_dir}/${fname}"
    done
    local target="${out_dir}/${updated_label}.csv"
    rm -f "${target}"
    cp "${updated_csv}" "${target}"
}

compute_true_card() {
    local data_dir="$1"
    local query_json="$2"
    python3 "${TRUE_CARD_SCRIPT}" "${data_dir}" "${query_json}" 2>/dev/null \
        | sed -n 's/.*: \([0-9][0-9]*\).*/\1/p' | head -1
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

echo "pattern,query_type,position,round,edge_label,inserted_edges,estimated_card,true_card,qerror,insert_time_s,query_time_s,total_time_s" > "${RESULT_CSV}"

for exp in "${EXPERIMENTS[@]}"; do
    IFS='|' read -r PATTERN JSON_REL_NP JSON_REL_P MIDDLE_EDGE ENDPOINT_EDGE <<< "$exp"
    JSON_NP="${PATTERN_DIR}/${JSON_REL_NP}"
    JSON_P="${PATTERN_DIR}/${JSON_REL_P}"

    for POSITION in middle endpoint; do
        if [ "$POSITION" = "middle" ]; then
            TARGET_EDGE="$MIDDLE_EDGE"
        else
            TARGET_EDGE="$ENDPOINT_EDGE"
        fi

        echo ""
        echo "============================================================"
        echo ">>> ${PATTERN} / ${POSITION} / edge=${TARGET_EDGE}"
        echo "============================================================"

        restore_original

        EXP_DIR="${RESULT_DIR}/${PATTERN}/${POSITION}"
        mkdir -p "${EXP_DIR}"

        GQL_SCRIPT="${EXP_DIR}/experiment.gql"
        cat > "${GQL_SCRIPT}" << GQLEOF
session set graph ldbc
:timing
-- round 0: baseline (no insert)
call gcard_query('${JSON_NP}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)
call gcard_query('${JSON_P}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)
GQLEOF

        for ROUND in $(seq 1 ${NUM_ROUNDS}); do
            SEED=$((BASE_SEED + ROUND))
            CSV_OUT="${EXP_DIR}/round${ROUND}/${TARGET_EDGE}.csv"
            mkdir -p "${EXP_DIR}/round${ROUND}"

            cat >> "${GQL_SCRIPT}" << GQLEOF
-- round ${ROUND}
call random_insert(${RATIO}, '', ${SEED}, '', '${TARGET_EDGE}')
call gcard_query('${JSON_NP}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)
call gcard_query('${JSON_P}', ${K}, ${SAMPLE_SIZE}, ${PRED_MODE}, false)
call export_edge_csv('${TARGET_EDGE}', '${CSV_OUT}')
GQLEOF
        done

        echo ":quit" >> "${GQL_SCRIPT}"

        STDOUT_LOG="${EXP_DIR}/stdout.log"
        STDERR_LOG="${EXP_DIR}/stderr.log"

        echo ">>> 执行 ${GQL_SCRIPT}"
        "${MINIGU_BIN}" execute "${GQL_SCRIPT}" --path "${DB_PATH}" \
            > "${STDOUT_LOG}" 2> "${STDERR_LOG}" || {
            echo "错误: 实验执行失败，查看 ${STDERR_LOG}"
            tail -20 "${STDERR_LOG}"
            continue
        }

        mapfile -t INSERTED_LINES < <(grep -E '^[0-9]+,[0-9]+$' "${STDOUT_LOG}" || true)
        mapfile -t CARD_LINES < <(sed -n 's/.*, cardinality: \([0-9]*\).*/\1/p' "${STDOUT_LOG}" || true)

        mapfile -t TIME_LINES < <(grep -oP 'Time: \K[0-9.]+(?=s)' "${STDERR_LOG}" 2>/dev/null || \
                                  grep -oE 'Time: [0-9.]+s' "${STDERR_LOG}" | sed 's/Time: //;s/s//' || true)

        # ── Round 0: baseline ──
        BASELINE_NP="${CARD_LINES[0]:-0}"
        BASELINE_P="${CARD_LINES[1]:-0}"
        BASELINE_TIME_NP="${TIME_LINES[0]:-0}"
        BASELINE_TIME_P="${TIME_LINES[1]:-0}"

        BASELINE_TRUE_NP="N/A"; BASELINE_QERR_NP="N/A"
        BASELINE_TRUE_P="N/A";  BASELINE_QERR_P="N/A"
        if [ "$HAS_DUCKDB" = true ]; then
            BASELINE_TRUE_NP=$(compute_true_card "${DATASET_DIR}" "${JSON_NP}" || echo "ERR")
            if [ "$BASELINE_TRUE_NP" != "ERR" ] && [ -n "$BASELINE_TRUE_NP" ]; then
                BASELINE_QERR_NP=$(qerror "$BASELINE_NP" "$BASELINE_TRUE_NP")
            fi
            BASELINE_TRUE_P=$(compute_true_card "${DATASET_DIR}" "${JSON_P}" || echo "ERR")
            if [ "$BASELINE_TRUE_P" != "ERR" ] && [ -n "$BASELINE_TRUE_P" ]; then
                BASELINE_QERR_P=$(qerror "$BASELINE_P" "$BASELINE_TRUE_P")
            fi
        fi

        echo "${PATTERN},no_pred,${POSITION},0,${TARGET_EDGE},0,${BASELINE_NP},${BASELINE_TRUE_NP},${BASELINE_QERR_NP},0,${BASELINE_TIME_NP},${BASELINE_TIME_NP}" >> "${RESULT_CSV}"
        echo "${PATTERN},with_pred,${POSITION},0,${TARGET_EDGE},0,${BASELINE_P},${BASELINE_TRUE_P},${BASELINE_QERR_P},0,${BASELINE_TIME_P},${BASELINE_TIME_P}" >> "${RESULT_CSV}"
        echo "  round0 (baseline): no_pred est=${BASELINE_NP} true=${BASELINE_TRUE_NP} qerr=${BASELINE_QERR_NP}"
        echo "  round0 (baseline): with_pred est=${BASELINE_P} true=${BASELINE_TRUE_P} qerr=${BASELINE_QERR_P}"

        # ── Round 1-5 ──
        for ROUND in $(seq 1 ${NUM_ROUNDS}); do
            IDX=$((ROUND - 1))

            INSERTED="${INSERTED_LINES[$IDX]:-0,0}"
            INSERTED_EDGES=$(echo "$INSERTED" | cut -d',' -f1)

            CARD_IDX_NP=$((2 + IDX * 2))
            CARD_IDX_P=$((2 + IDX * 2 + 1))
            EST_NP="${CARD_LINES[$CARD_IDX_NP]:-0}"
            EST_P="${CARD_LINES[$CARD_IDX_P]:-0}"

            TIME_BASE=$((2 + IDX * 4))
            T_INSERT="${TIME_LINES[$TIME_BASE]:-0}"
            T_QUERY_NP="${TIME_LINES[$((TIME_BASE + 1))]:-0}"
            T_QUERY_P="${TIME_LINES[$((TIME_BASE + 2))]:-0}"
            T_TOTAL_NP=$(echo "${T_INSERT} + ${T_QUERY_NP}" | bc 2>/dev/null || echo "0")
            T_TOTAL_P=$(echo "${T_INSERT} + ${T_QUERY_P}" | bc 2>/dev/null || echo "0")

            TRUE_NP="N/A"; QERR_NP="N/A"
            TRUE_P="N/A";  QERR_P="N/A"
            EDGE_CSV="${EXP_DIR}/round${ROUND}/${TARGET_EDGE}.csv"

            if [ "$HAS_DUCKDB" = true ] && [ -f "${EDGE_CSV}" ]; then
                DATA_DIR="${EXP_DIR}/round${ROUND}/_data"
                assemble_data_dir "${DATA_DIR}" "${TARGET_EDGE}" "${EDGE_CSV}"

                TRUE_NP=$(compute_true_card "${DATA_DIR}" "${JSON_NP}" || echo "ERR")
                if [ "$TRUE_NP" != "ERR" ] && [ -n "$TRUE_NP" ]; then
                    QERR_NP=$(qerror "$EST_NP" "$TRUE_NP")
                else
                    TRUE_NP="ERR"; QERR_NP="ERR"
                fi

                TRUE_P=$(compute_true_card "${DATA_DIR}" "${JSON_P}" || echo "ERR")
                if [ "$TRUE_P" != "ERR" ] && [ -n "$TRUE_P" ]; then
                    QERR_P=$(qerror "$EST_P" "$TRUE_P")
                else
                    TRUE_P="ERR"; QERR_P="ERR"
                fi

                rm -rf "${DATA_DIR}"
            fi

            echo "${PATTERN},no_pred,${POSITION},${ROUND},${TARGET_EDGE},${INSERTED_EDGES},${EST_NP},${TRUE_NP},${QERR_NP},${T_INSERT},${T_QUERY_NP},${T_TOTAL_NP}" >> "${RESULT_CSV}"
            echo "${PATTERN},with_pred,${POSITION},${ROUND},${TARGET_EDGE},${INSERTED_EDGES},${EST_P},${TRUE_P},${QERR_P},${T_INSERT},${T_QUERY_P},${T_TOTAL_P}" >> "${RESULT_CSV}"
            echo "  round${ROUND}: ins=${INSERTED_EDGES}"
            echo "    no_pred:   est=${EST_NP}, true=${TRUE_NP}, qerr=${QERR_NP}, t_ins=${T_INSERT}s, t_qry=${T_QUERY_NP}s"
            echo "    with_pred: est=${EST_P}, true=${TRUE_P}, qerr=${QERR_P}, t_qry=${T_QUERY_P}s"
        done
    done
done

echo ""
echo "============================================================"
echo ">>> 实验完成！结果保存在:"
echo "    ${RESULT_CSV}"
echo "============================================================"
