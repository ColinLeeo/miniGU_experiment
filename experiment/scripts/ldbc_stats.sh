#!/bin/bash
# 统计 LDBC 数据集的点数、边数和总文件大小
# 用法: bash ldbc_stats.sh <数据集目录>
# 例如: bash ldbc_stats.sh experiment/dataset/ldbc/sf1

DIR="${1:?用法: $0 <数据集目录>}"

if [ ! -d "$DIR" ]; then
    echo "错误: 目录 $DIR 不存在"
    exit 1
fi

total_vertices=0
total_edges=0

echo "=== 点表统计 ==="
for f in "$DIR"/*.csv; do
    base=$(basename "$f" .csv)
    # 边表包含下划线分隔的三段: vertex_edge_vertex
    # 点表是单个名称（不含下划线，或只有一个词）
    if echo "$base" | grep -qE '^[^_]+_[^_]+_[^_]+$'; then
        continue
    fi
    count=$(($(wc -l < "$f") - 1))  # 减去 header
    total_vertices=$((total_vertices + count))
    printf "  %-30s %'10d\n" "$base" "$count"
done

echo ""
echo "=== 边表统计 ==="
for f in "$DIR"/*.csv; do
    base=$(basename "$f" .csv)
    if echo "$base" | grep -qE '^[^_]+_[^_]+_[^_]+$'; then
        count=$(($(wc -l < "$f") - 1))
        total_edges=$((total_edges + count))
        printf "  %-45s %'10d\n" "$base" "$count"
    fi
done

echo ""
echo "=== 总文件大小 ==="
total_size=$(du -sh "$DIR" | cut -f1)
echo "  $total_size"

echo ""
echo "=== 汇总 ==="
printf "  总点数: %'d\n" "$total_vertices"
printf "  总边数: %'d\n" "$total_edges"
