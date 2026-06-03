#!/usr/bin/env python3
"""Per-tree estimated cardinality across update percentages.

Reads:
  experiment.gql   - GQL script with `-- pattern/position pct=X` comments
  stdout.log       - minigu output with `[gcard-cand]` blocks per gcard_query

Output:
  <out>/per_tree_long.csv               - long-format per (pattern, position, qvar, tree_fp, pct)
  <out>/plots/<pattern>_<position>_<qvar>.png  - line chart per group
"""

import argparse
import hashlib
import re
from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd

CAND_RE = re.compile(
    r"^\[gcard-cand\] query=(\S+) idx=(\d+)/(\d+) score=(\S+) card=(\S+)"
)
AE_RE = re.compile(
    r"^\s+ae(\d+): (\d+)\([^)]+\) -> (\d+)\([^)]+\) sel=\S+ src_rows=\S+ dst_rows=\S+ path=(.*)$"
)
CAND_MIN_RE = re.compile(
    r"^\[gcard-cand-min\] query=(\S+) selected=\S+ score=\S+ card=(\S+) total_candidates=(\d+)"
)
SECTION_RE = re.compile(
    r"^-- (q\w+)/(middle|endpoint) (baseline|pct=([\d.]+))"
)
CALL_RE = re.compile(r"call gcard_query\('([^']+)'")
INSERT_RE = re.compile(r"call random_insert\([^,]+,\s*'[^']*',\s*\d+,\s*'[^']*',\s*'([^']+)'\)")


def parse_experiment_gql(path):
    """每个 gcard_query 调用 → {pattern, position, pct, qvar, stem, target_edge}."""
    blocks = []
    cur = None
    cur_target_edge = None  # 最近一次 random_insert 的边类型
    for line in path.read_text().splitlines():
        m = SECTION_RE.match(line)
        if m:
            pattern, position, kind = m.group(1), m.group(2), m.group(3)
            pct_str = m.group(4)
            pct = 0.0 if kind == "baseline" else float(pct_str)
            cur = (pattern, position, pct)
            if kind == "baseline":
                cur_target_edge = None
            continue
        m = INSERT_RE.search(line)
        if m:
            cur_target_edge = m.group(1)
            continue
        m = CALL_RE.search(line)
        if m and cur:
            stem = Path(m.group(1)).stem
            qvar = "no_pred" if "_" not in stem else "with_pred"
            blocks.append({
                "pattern": cur[0],
                "position": cur[1],
                "pct": cur[2],
                "qvar": qvar,
                "stem": stem,
                "target_edge": cur_target_edge,  # baseline 时为 None
            })
    return blocks


def parse_stdout(path):
    """stdout.log → list of {stem, trees: [{idx, total, score, card, edges}], min_card, total_candidates}."""
    blocks = []
    cur_block = None
    cur_tree = None
    for line in path.read_text().splitlines():
        m = CAND_RE.match(line)
        if m:
            stem, idx, total, score, card = m.groups()
            if cur_block is None or cur_block["stem"] != stem:
                if cur_block is not None and not cur_block.get("_finalized"):
                    blocks.append(cur_block)
                cur_block = {"stem": stem, "trees": []}
            cur_tree = {
                "idx": int(idx),
                "total": int(total),
                "score": score,
                "card": float(card),
                "edges": [],
            }
            cur_block["trees"].append(cur_tree)
            continue
        m = AE_RE.match(line)
        if m and cur_tree is not None:
            _, src, dst, path_str = m.groups()
            cur_tree["edges"].append({
                "src": int(src),
                "dst": int(dst),
                "path": path_str.strip(),
            })
            continue
        m = CAND_MIN_RE.match(line)
        if m and cur_block is not None:
            cur_block["min_card"] = float(m.group(2))
            cur_block["total_candidates"] = int(m.group(3))
            cur_block["_finalized"] = True
            blocks.append(cur_block)
            cur_block = None
            cur_tree = None
            continue
    if cur_block is not None and not cur_block.get("_finalized"):
        blocks.append(cur_block)
    return blocks


def tree_fingerprint(edges):
    """方向无关的稳定指纹：每条 abstract edge = (frozenset({src, dst}), path)."""
    fp = frozenset(
        (frozenset({e["src"], e["dst"]}), e["path"])
        for e in edges
    )
    # 用 sha1 短哈希做 column 名（frozenset 不能直接当 pandas column）
    return hashlib.sha1(repr(sorted(fp)).encode()).hexdigest()[:8]


def parse_path(path_str):
    """把 'country -> city_ispartof_country ->city -> ' 解析为 [(vlabel, elabel, vlabel), ...]
    返回 hop tuples，每个 tuple = (src_vlabel, edge_label, dst_vlabel).
    """
    # 规范化空格：minigu 输出有时是 ->city 有时 -> city
    tokens = [t.strip() for t in path_str.replace("->", " -> ").split("->")]
    tokens = [t for t in tokens if t]
    hops = []
    for i in range(0, len(tokens) - 2, 2):
        v_src, e_lab, v_dst = tokens[i], tokens[i + 1], tokens[i + 2]
        hops.append((v_src, e_lab, v_dst))
    return hops


def compact_path(path_str):
    """紧凑形式：'country -[city_ispartof_country]-> city -[person_islocatedin_city]-> person'"""
    hops = parse_path(path_str)
    if not hops:
        return path_str.strip()
    out = hops[0][0]
    for v_src, e_lab, v_dst in hops:
        out += f" -[{e_lab}]-> {v_dst}"
    return out


def tree_summary(edges):
    parts = []
    for e in sorted(edges, key=lambda x: (min(x["src"], x["dst"]), max(x["src"], x["dst"]))):
        a, b = sorted([e["src"], e["dst"]])
        n_hops = len(parse_path(e["path"]))
        parts.append(f"{{{a},{b}}}/{n_hops}h")
    return " ".join(parts)


def tree_decomposition_text(edges):
    """整棵树的多行紧凑描述，用于图下方 panel。"""
    lines = []
    for i, e in enumerate(
        sorted(edges, key=lambda x: (min(x["src"], x["dst"]), max(x["src"], x["dst"])))
    ):
        cp = compact_path(e["path"])
        lines.append(f"  ae{i + 1}: v{e['src']}->v{e['dst']}  {cp}")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--gql", required=True, type=Path)
    ap.add_argument("--log", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument(
        "--qvar",
        default="no_pred",
        choices=["no_pred", "with_pred", "both"],
        help="Plot which qvar (default: no_pred)",
    )
    args = ap.parse_args()

    gql_blocks = parse_experiment_gql(args.gql)
    cand_blocks = parse_stdout(args.log)

    print(f"gql:  {len(gql_blocks)} gcard_query calls")
    print(f"log:  {len(cand_blocks)} [gcard-cand-min] blocks")

    if len(gql_blocks) != len(cand_blocks):
        print("WARN: lengths differ; will align by min length")
    n = min(len(gql_blocks), len(cand_blocks))

    rows = []
    # Cache one (edges) example per (pattern, position, qvar, tree_fp) — first time
    # we see this tree, remember its abstract edge structure so we can later
    # render the decomposition panel.
    decomp_cache: dict[tuple, list] = {}
    for meta, cand in zip(gql_blocks[:n], cand_blocks[:n]):
        if meta["stem"] != cand["stem"]:
            print(f"WARN: stem mismatch at idx: gql={meta['stem']} cand={cand['stem']}")
        for tree in cand["trees"]:
            fp = tree_fingerprint(tree["edges"])
            key = (meta["pattern"], meta["position"], meta["qvar"], fp)
            decomp_cache.setdefault(key, tree["edges"])
            rows.append({
                "pattern": meta["pattern"],
                "position": meta["position"],
                "qvar": meta["qvar"],
                "pct": meta["pct"],
                "target_edge": meta["target_edge"] or "",
                "tree_fp": fp,
                "tree_summary": tree_summary(tree["edges"]),
                "tree_idx": tree["idx"],
                "tree_total": tree["total"],
                "n_edges": len(tree["edges"]),
                "card": tree["card"],
            })

    df = pd.DataFrame(rows)
    args.out.mkdir(parents=True, exist_ok=True)
    long_csv = args.out / "per_tree_long.csv"
    df.to_csv(long_csv, index=False)
    print(f"wrote {long_csv} ({len(df)} rows)")

    plots_dir = args.out / "plots"
    plots_dir.mkdir(exist_ok=True)

    qvars = ["no_pred", "with_pred"] if args.qvar == "both" else [args.qvar]
    n_plots = 0
    for qvar in qvars:
        sub = df[df["qvar"] == qvar]
        for (pattern, position), group in sub.groupby(["pattern", "position"]):
            pivot = group.pivot_table(
                index="pct", columns="tree_fp", values="card", aggfunc="first"
            )
            summaries = (
                group.drop_duplicates("tree_fp")
                .set_index("tree_fp")["tree_summary"]
                .to_dict()
            )
            # 排序：用 baseline 的 card 升序，画图更顺眼
            base_card = pivot.loc[0.0] if 0.0 in pivot.index else pivot.iloc[0]
            ordered_cols = base_card.sort_values().index.tolist()

            # 准备图下方的分解 panel 文本
            decomp_lines = ["Spanning tree decompositions:"]
            for col in ordered_cols:
                edges = decomp_cache.get((pattern, position, qvar, col))
                if not edges:
                    continue
                decomp_lines.append(f"[{col}]  {summaries.get(col, '')}")
                decomp_lines.append(tree_decomposition_text(edges))
            decomp_text = "\n".join(decomp_lines)

            # figure 高度按"上半折线 + 下半 panel 行数"动态分配
            n_decomp_lines = decomp_text.count("\n") + 1
            decomp_height = max(0.18, min(0.6, n_decomp_lines * 0.022))
            fig_h = 5.5 + n_decomp_lines * 0.18
            fig = plt.figure(figsize=(11, fig_h))
            ax = fig.add_axes([0.08, decomp_height + 0.05, 0.55, 0.85 - decomp_height])

            for col in ordered_cols:
                series = pivot[col]
                ax.plot(
                    series.index,
                    series.values,
                    marker="o",
                    linewidth=1.5,
                    markersize=6,
                    label=f"{col} {summaries.get(col, '')}"[:80],
                )
            # 同一个 (pattern, position) 下所有 pct 块插入的 target_edge 是同一种
            target_edges = (
                group.loc[group["target_edge"] != "", "target_edge"]
                .dropna()
                .unique()
                .tolist()
            )
            target_label = (
                target_edges[0] if len(target_edges) == 1 else ",".join(target_edges)
            )
            ax.set_xlabel("update pct")
            ax.set_ylabel("estimated cardinality (log)")
            ax.set_yscale("log")
            ax.set_title(
                f"{pattern} / {position} / {qvar}\n"
                f"inserted edge: {target_label}"
            )
            ax.grid(True, which="both", alpha=0.3)
            ax.legend(fontsize=7, loc="center left", bbox_to_anchor=(1.05, 0.5))

            # 下方 panel：完整 abstract-edge 分解
            ax_text = fig.add_axes([0.05, 0.02, 0.9, decomp_height])
            ax_text.axis("off")
            ax_text.text(
                0,
                1,
                decomp_text,
                fontsize=7,
                family="monospace",
                verticalalignment="top",
                horizontalalignment="left",
                transform=ax_text.transAxes,
            )

            path = plots_dir / f"{pattern}_{position}_{qvar}.png"
            fig.savefig(path, dpi=120, bbox_inches="tight")
            plt.close(fig)
            n_plots += 1
    print(f"wrote {n_plots} plots to {plots_dir}")


if __name__ == "__main__":
    main()
