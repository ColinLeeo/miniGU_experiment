#!/usr/bin/env python3
import argparse
import csv
import glob
import os
import pickle
import re
import sys
import time
from pathlib import Path

import pandas as pd


def log(message: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


def append_safe_bound_source_to_path(workspace: Path) -> None:
    source_dir = workspace / "baseline" / "SafeBound" / "Source"
    sys.path.insert(0, str(source_dir))


def split_comma_outside_parens(s: str) -> list[str]:
    parts = []
    cur = []
    depth = 0
    for ch in s:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth = max(depth - 1, 0)
        elif ch == "," and depth == 0:
            parts.append("".join(cur).strip())
            cur = []
            continue
        cur.append(ch)
    if cur:
        parts.append("".join(cur).strip())
    return [p for p in parts if p]


def split_and_conditions(where_clause: str) -> list[str]:
    # Workloads here are conjunction-only SQL patterns.
    return [x.strip() for x in re.split(r"\s+and\s+", where_clause, flags=re.IGNORECASE) if x.strip()]


def parse_value(value: str):
    value = value.strip()
    if value.endswith("::timestamp"):
        value = value[: -len("::timestamp")]
    if value.startswith("'") and value.endswith("'") and len(value) >= 2:
        return value[1:-1]
    if value.startswith("(") and value.endswith(")"):
        inner = value[1:-1].strip()
        if inner == "":
            return []
        return [parse_value(x.strip()) for x in split_comma_outside_parens(inner)]
    try:
        return int(value)
    except ValueError:
        pass
    try:
        return float(value)
    except ValueError:
        pass
    return value


def parse_sql_to_join_query_graph(sql: str, join_graph_cls):
    sql = sql.strip().rstrip(";")
    m = re.match(
        r"^\s*select\s+count\s*\(\s*\*\s*\)\s+from\s+(?P<from>.+?)(?:\s+where\s+(?P<where>.+))?$",
        sql,
        flags=re.IGNORECASE | re.DOTALL,
    )
    if not m:
        raise ValueError(f"Unsupported SQL format: {sql}")

    from_clause = m.group("from").strip()
    where_clause = (m.group("where") or "").strip()

    query = join_graph_cls()

    def add_table_alias(term: str) -> None:
        tokens = term.split()
        if len(tokens) == 2:
            table_name, alias = tokens
        elif len(tokens) == 3 and tokens[1].lower() == "as":
            table_name, alias = tokens[0], tokens[2]
        else:
            raise ValueError(f"Unsupported FROM term: {term}")
        query.addAlias(table_name, alias)

    extra_conditions = []
    if re.search(r"\b(?:cross\s+join|join)\b", from_clause, flags=re.IGNORECASE):
        protected_from = re.sub(r"\bcross\s+join\b", "CROSS_JOIN", from_clause, flags=re.IGNORECASE)
        join_terms = re.split(r"\s+(?=(?:CROSS_JOIN|join)\s+)", protected_from, flags=re.IGNORECASE)
        add_table_alias(join_terms[0].strip())
        for join_term in join_terms[1:]:
            cross = re.match(r"^CROSS_JOIN\s+(.+)$", join_term, flags=re.IGNORECASE | re.DOTALL)
            if cross:
                add_table_alias(cross.group(1).strip())
                continue
            jm = re.match(r"^join\s+(.+?)\s+on\s+(.+)$", join_term, flags=re.IGNORECASE | re.DOTALL)
            if not jm:
                raise ValueError(f"Unsupported JOIN term: {join_term}")
            add_table_alias(jm.group(1).strip())
            extra_conditions.extend(split_and_conditions(jm.group(2).strip()))
    else:
        table_terms = split_comma_outside_parens(from_clause)
        for term in table_terms:
            add_table_alias(term)

    conditions = []
    if extra_conditions:
        conditions.extend(extra_conditions)
    if where_clause:
        conditions.extend(split_and_conditions(where_clause))

    if conditions:
        for cond in conditions:
            cond = cond.strip().strip("()")
            join_match = re.match(
                r"^([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*=\s*([A-Za-z_]\w*)\.([A-Za-z_]\w*)$",
                cond,
                flags=re.IGNORECASE,
            )
            if join_match:
                l_alias, l_col, r_alias, r_col = join_match.groups()
                query.addJoin(l_alias, l_col, r_alias, r_col)
                continue

            pred_match = re.match(
                r"^([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*(=|!=|<=|>=|<|>|LIKE|IN|IS\s+NOT\s+NULL|IS\s+NULL)\s*(.*)$",
                cond,
                flags=re.IGNORECASE,
            )
            if not pred_match:
                raise ValueError(f"Unsupported WHERE condition: {cond}")
            alias, col, op, rhs = pred_match.groups()
            op_upper = re.sub(r"\s+", " ", op.strip().upper())
            if op_upper in {"IS NULL", "IS NOT NULL"}:
                query.addPredicate(alias, col, op_upper, None)
            elif op_upper == "IN":
                query.addPredicate(alias, col, op_upper, parse_value(rhs))
            else:
                query.addPredicate(alias, col, op_upper, parse_value(rhs))

    query.buildJoinGraph()
    return query


def collect_query_graphs(sql_files: list[Path], join_graph_cls):
    graphs = []
    for sql_file in sql_files:
        text = sql_file.read_text(encoding="utf-8")
        graphs.append(parse_sql_to_join_query_graph(text, join_graph_cls))
    return graphs


def infer_required_columns(query_graphs):
    required = {}
    for q in query_graphs:
        for vertex in q.vertexDict.values():
            table = vertex.tableName
            required.setdefault(table, {"join": set(), "filter": set()})
            for c in vertex.outputJoinCols:
                required[table]["join"].add(c)
            for p in vertex.predicates:
                required[table]["filter"].add(p.colName)
    return required


def load_table_df(dataset_dir: Path, table_name_upper: str, required_cols_upper: list[str]) -> pd.DataFrame:
    csv_files = list(dataset_dir.glob("*.csv"))
    lower_name_to_file = {f.stem.lower(): f for f in csv_files}
    key = table_name_upper.lower()
    if key not in lower_name_to_file:
        raise FileNotFoundError(f"Cannot find CSV for table '{table_name_upper}' in {dataset_dir}")

    table_file = lower_name_to_file[key]
    df = pd.read_csv(table_file, low_memory=False)
    col_map = {c.lower(): c for c in df.columns}

    selected_actual = []
    for req in required_cols_upper:
        req_lower = req.lower()
        if req_lower not in col_map:
            raise KeyError(
                f"Column '{req}' required by workload is missing in {table_file}. "
                f"Available columns: {list(df.columns)}"
            )
        selected_actual.append(col_map[req_lower])

    # Keep deterministic ordering and remove duplicates.
    dedup = []
    seen = set()
    for c in selected_actual:
        if c not in seen:
            dedup.append(c)
            seen.add(c)
    return df[dedup].copy()


def sorted_sql_files(pattern_dir: Path) -> list[Path]:
    files = [Path(p) for p in glob.glob(str(pattern_dir / "**" / "*.sql"), recursive=True)]
    files.sort()
    if not files:
        raise FileNotFoundError(f"No .sql files found under {pattern_dir}")
    return files


def build_dblp_partitioned_fk_to_k(table_names: list[str]) -> dict[str, list[list[str]]]:
    fk_to_k = {}
    available = {t.lower() for t in table_names}
    edge_re = re.compile(r"^pe0_(\d+)_(\d+)$", flags=re.IGNORECASE)
    for table in table_names:
        m = edge_re.match(table)
        if not m:
            continue
        src_label, dst_label = m.groups()
        src_table = f"v{src_label}"
        dst_table = f"v{dst_label}"
        refs = []
        if src_table.lower() in available:
            refs.append(["src", "id", src_table])
        if dst_table.lower() in available:
            refs.append(["dst", "id", dst_table])
        if refs:
            fk_to_k[table] = refs
    return fk_to_k


def run_build(args):
    workspace = Path(args.workspace).resolve()
    append_safe_bound_source_to_path(workspace)
    from SafeBoundUtils import SafeBound
    from JoinGraphUtils import JoinQueryGraph

    dataset_dir = Path(args.dataset_dir).resolve()
    pattern_dir = Path(args.pattern_dir).resolve()
    output_path = Path(args.output_stats).resolve()
    output_path.parent.mkdir(parents=True, exist_ok=True)

    log(f"collecting SQL files from {pattern_dir}")
    sql_files = sorted_sql_files(pattern_dir)
    log(f"parsing {len(sql_files)} SQL files")
    query_graphs = collect_query_graphs(sql_files, JoinQueryGraph)
    log("inferring required columns")
    required = infer_required_columns(query_graphs)

    table_names = sorted(required.keys())
    log(f"loading {len(table_names)} tables from {dataset_dir}")
    table_dfs = []
    join_cols = []
    filter_cols = []

    for table in table_names:
        table_join_cols = sorted(required[table]["join"])
        table_filter_cols = sorted(required[table]["filter"])
        needed = sorted(set(table_join_cols + table_filter_cols))
        log(f"loading table {table} ({len(needed)} columns)")
        df = load_table_df(dataset_dir, table, needed)
        table_dfs.append(df)
        join_cols.append(table_join_cols)
        filter_cols.append(table_filter_cols)

    fk_to_k = {}
    if args.dblp_partitioned_fk:
        fk_to_k = build_dblp_partitioned_fk_to_k(table_names)
        log(f"using DBLP partitioned FK/PK relationships for {len(fk_to_k)} edge tables")

    log(f"building SafeBound stats with {args.num_cores} cores")
    start = time.perf_counter()
    stats = SafeBound(
        table_dfs,
        table_names,
        join_cols,
        args.relative_error_per_segment,
        filter_cols,
        args.num_buckets,
        args.num_equality_outliers,
        fk_to_k,
        args.num_outliers,
        args.track_nulls,
        args.track_trigrams,
        args.num_cores,
        args.grouping_method,
        args.model_cdf,
        args.verbose,
    )
    elapsed = time.perf_counter() - start
    log("serializing SafeBound stats")

    with open(output_path, "wb") as f:
        pickle.dump(stats, f)

    print(f"Built SafeBound stats: {output_path}")
    print(f"Build time: {elapsed:.6f} s")
    print(f"Stats memory: {stats.memory()} bytes")
    print(f"Tables: {len(table_names)}, Queries: {len(sql_files)}")


def run_query(args):
    workspace = Path(args.workspace).resolve()
    append_safe_bound_source_to_path(workspace)
    from JoinGraphUtils import JoinQueryGraph

    pattern_dir = Path(args.pattern_dir).resolve()
    stats_path = Path(args.stats_path).resolve()
    output_csv = Path(args.output_csv).resolve()
    output_csv.parent.mkdir(parents=True, exist_ok=True)

    with open(stats_path, "rb") as f:
        stats = pickle.load(f)

    sql_files = sorted_sql_files(pattern_dir)
    query_graphs = collect_query_graphs(sql_files, JoinQueryGraph)
    stats_size = stats.memory()

    with open(output_csv, "w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["query", "estimate", "inference_time_s", "stats_size_bytes"])
        for sql_file, query_graph in zip(sql_files, query_graphs):
            start = time.perf_counter()
            estimate = stats.functionalFrequencyBound(query_graph)
            elapsed = time.perf_counter() - start
            rel = sql_file.relative_to(pattern_dir).as_posix()
            writer.writerow([rel, estimate, f"{elapsed:.6f}", stats_size])

    print(f"Saved inference results: {output_csv}")
    print(f"Queries: {len(sql_files)}")


def main():
    parser = argparse.ArgumentParser(description="Build/query SafeBound for miniGU patterns/sql workloads.")
    sub = parser.add_subparsers(dest="command", required=True)

    build = sub.add_parser("build", help="Build SafeBound stats object (catalog equivalent).")
    build.add_argument("--workspace", required=True)
    build.add_argument("--dataset-dir", required=True)
    build.add_argument("--pattern-dir", required=True)
    build.add_argument("--output-stats", required=True)
    build.add_argument("--num-cores", type=int, default=8)
    build.add_argument("--relative-error-per-segment", type=float, default=0.01)
    build.add_argument("--num-buckets", type=int, default=32)
    build.add_argument("--num-equality-outliers", type=int, default=512)
    build.add_argument("--num-outliers", type=int, default=6)
    build.add_argument("--track-nulls", action="store_true", default=False)
    build.add_argument("--track-trigrams", action="store_true", default=False)
    build.add_argument("--grouping-method", default="CompleteClustering")
    build.add_argument("--model-cdf", action="store_true", default=True)
    build.add_argument("--verbose", action="store_true", default=False)
    build.add_argument("--dblp-partitioned-fk", action="store_true", default=False)

    query = sub.add_parser("query", help="Run SafeBound estimates for all SQL files.")
    query.add_argument("--workspace", required=True)
    query.add_argument("--pattern-dir", required=True)
    query.add_argument("--stats-path", required=True)
    query.add_argument("--output-csv", required=True)

    args = parser.parse_args()
    if args.command == "build":
        run_build(args)
    elif args.command == "query":
        run_query(args)


if __name__ == "__main__":
    main()
