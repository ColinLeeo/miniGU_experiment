#!/usr/bin/env python3
import argparse
import copy
import csv
import json
import math
import os
import pickle
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
FACTORJOIN_DIR = ROOT / "experiment/baseline/FactorJoin"
DEFAULT_DATA_PATH = "/home/zxz/miniGU/experiment/datasets/ldbc/sf1/{}.csv"

sys.path.insert(0, str(FACTORJOIN_DIR))


def unwrap_value(value):
    if isinstance(value, dict):
        return next(iter(value.values()))
    return value


def sql_literal(value):
    value = unwrap_value(value)
    if isinstance(value, bool):
        return "1" if value else "0"
    if isinstance(value, (int, float)):
        return str(value)
    # FactorJoin's parser is not a full SQL parser; unquoted categorical values
    # match BayesCard's categorical domains inside the trained BN.
    return str(value).replace("'", "")


def sql_op(op):
    return {
        "eq": "=",
        "ne": "!=",
        "gt": ">",
        "ge": ">=",
        "lt": "<",
        "le": "<=",
    }[op.lower()]


def _alias_sort_key(name):
    return (name[0] != "e", int(name[1:]) if name[1:].isdigit() else name)


def _joined_name(names):
    return " ".join(sorted(names, key=_alias_sort_key))


def build_left_deep_subplans(aliases, adjacency):
    if len(aliases) <= 1:
        return []

    remaining = set(aliases)
    start = sorted(remaining, key=_alias_sort_key)[0]
    joined = {start}
    remaining.remove(start)
    subplans = []

    while remaining:
        candidates = sorted(
            [name for name in remaining if adjacency.get(name, set()) & joined],
            key=_alias_sort_key,
        )
        if not candidates:
            raise ValueError(f"Cannot build connected left-deep subplan for {sorted(remaining)}")
        left = candidates[0]
        subplans.append((left, _joined_name(joined)))
        joined.add(left)
        remaining.remove(left)

    return subplans


def build_factorjoin_query(pattern_path):
    pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
    vertices = {v["id"]: v["label"].lower() for v in pattern["vertices"]}
    edges = pattern["edges"]
    predicates = pattern.get("predicates", [])

    from_terms = []
    vertex_refs = {}
    alias_to_table = {}
    adjacency = {}
    join_conditions = []
    table_queries = {}

    for edge in edges:
        alias = f"e{edge['id']}"
        table = edge["label"].lower()
        alias_to_table[alias] = table
        adjacency.setdefault(alias, set())
        from_terms.append(f"{table} AS {alias}")
        vertex_refs.setdefault(edge["src"], []).append((alias, "src"))
        vertex_refs.setdefault(edge["dst"], []).append((alias, "dst"))

    conditions = []
    for refs in vertex_refs.values():
        first_alias, first_col = refs[0]
        for alias, col in refs[1:]:
            cond = f"{alias}.{col} = {first_alias}.{first_col}"
            conditions.append(cond)
            join_conditions.append((f"{alias}.{col}", f"{first_alias}.{first_col}"))
            adjacency.setdefault(alias, set()).add(first_alias)
            adjacency.setdefault(first_alias, set()).add(alias)

    vertex_aliases = {}
    for pred in predicates:
        if pred["target"] != "vertex":
            continue
        vid = pred["id"]
        if vid not in vertex_aliases:
            alias = f"v{vid}"
            table = vertices[vid]
            vertex_aliases[vid] = alias
            alias_to_table[alias] = table
            adjacency.setdefault(alias, set())
            from_terms.append(f"{table} AS {alias}")
            pin_alias, pin_col = vertex_refs[vid][0]
            cond = f"{alias}.id = {pin_alias}.{pin_col}"
            conditions.append(cond)
            join_conditions.append((f"{alias}.id", f"{pin_alias}.{pin_col}"))
            adjacency[alias].add(pin_alias)
            adjacency.setdefault(pin_alias, set()).add(alias)

    for pred in predicates:
        alias = f"v{pred['id']}" if pred["target"] == "vertex" else f"e{pred['id']}"
        attr = f"{alias}.{pred['property']}"
        op = sql_op(pred["op"])
        value = sql_literal(pred["value"])
        conditions.append(f"{attr} {op} {value}")
        table_queries.setdefault(alias, []).append([attr, op, unwrap_value(pred["value"])])

    sql = "SELECT COUNT(*) FROM " + ", ".join(from_terms)
    if conditions:
        sql += " WHERE " + " AND ".join(conditions)

    aliases = list(alias_to_table)
    return {
        "sql": sql + ";",
        "aliases": aliases,
        "alias_to_table": alias_to_table,
        "join_conditions": join_conditions,
        "table_queries": table_queries,
        "subplans": build_left_deep_subplans(aliases, adjacency),
        "has_self_join": len(set(alias_to_table.values())) != len(alias_to_table),
    }

def pattern_to_factorjoin_sql(pattern_path):
    return build_factorjoin_query(pattern_path)["sql"]


def _alias_attr(alias, physical_attr):
    return alias + "." + physical_attr.split(".", 1)[1]


def _physical_attr(physical_table, alias_attr):
    return physical_table + "." + alias_attr.split(".", 1)[1]


class AliasBN:
    def __init__(self, physical_bn, alias, physical_table):
        self._bn = physical_bn
        self.alias = alias
        self.physical_table = physical_table
        self.nrows = getattr(physical_bn, "nrows", 1.0)

    def init_inference_method(self):
        return self._bn.init_inference_method()

    def query_id_prob(self, query, id_attrs, *args, **kwargs):
        physical_query = {
            _physical_attr(self.physical_table, attr): value for attr, value in query.items()
        }
        physical_ids = [_physical_attr(self.physical_table, attr) for attr in id_attrs]
        returned_ids, probs = self._bn.query_id_prob(physical_query, physical_ids, *args, **kwargs)
        alias_ids = [_alias_attr(self.alias, attr) for attr in returned_ids]
        return alias_ids, probs


def _rename_key_dict(d, alias, physical_table):
    return {_alias_attr(alias, key): value for key, value in d.items()}


def make_alias_bucket(bucket, alias, physical_table):
    alias_bucket = copy.copy(bucket)
    alias_bucket.table_name = alias
    alias_bucket.id_attributes = [_alias_attr(alias, key) for key in bucket.id_attributes]
    alias_bucket.bin_sizes = _rename_key_dict(bucket.bin_sizes, alias, physical_table)
    alias_bucket.oned_bin_modes = _rename_key_dict(bucket.oned_bin_modes, alias, physical_table)
    alias_bucket.twod_bin_modes = _rename_key_dict(bucket.twod_bin_modes, alias, physical_table)
    return alias_bucket


def _add_alias_filter(table_query, attr, op, value):
    value = unwrap_value(value)
    if op == "=":
        table_query[attr] = [sql_literal(value) if isinstance(value, str) else value]
    elif op == ">":
        lo, hi = table_query.get(attr, (-math.inf, math.inf))
        table_query[attr] = (max(lo, value), hi)
    elif op == ">=":
        lo, hi = table_query.get(attr, (-math.inf, math.inf))
        table_query[attr] = (max(lo, value), hi)
    elif op == "<":
        lo, hi = table_query.get(attr, (-math.inf, math.inf))
        table_query[attr] = (lo, min(hi, value))
    elif op == "<=":
        lo, hi = table_query.get(attr, (-math.inf, math.inf))
        table_query[attr] = (lo, min(hi, value))


def make_alias_model(model, context):
    alias_model = copy.copy(model)
    alias_model.bns = {}
    alias_model.table_buckets = {}
    alias_model.equivalent_keys = {}

    for alias, physical in context["alias_to_table"].items():
        alias_model.bns[alias] = AliasBN(model.bns[physical], alias, physical)
        alias_model.table_buckets[alias] = make_alias_bucket(model.table_buckets[physical], alias, physical)

    parent = {}

    def find(x):
        parent.setdefault(x, x)
        if parent[x] != x:
            parent[x] = find(parent[x])
        return parent[x]

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    for left, right in context["join_conditions"]:
        union(left, right)

    groups = {}
    for left, right in context["join_conditions"]:
        groups.setdefault(find(left), set()).update([left, right])

    alias_model.equivalent_keys = {root: sorted(keys) for root, keys in groups.items()}

    alias_table_queries = {}
    for alias, conds in context["table_queries"].items():
        for attr, op, value in conds:
            _add_alias_filter(alias_table_queries.setdefault(alias, {}), attr, op, value)

    join_cond_strings = [f"{left} = {right}" for left, right in context["join_conditions"]]
    join_keys = {}
    for left, right in context["join_conditions"]:
        for attr in (left, right):
            alias = attr.split(".", 1)[0]
            join_keys.setdefault(alias, set()).add(attr)

    tables_all = {alias: alias for alias in context["aliases"]}

    def parse_query_simple(_query):
        return tables_all, alias_table_queries, join_cond_strings, join_keys

    alias_model.parse_query_simple = parse_query_simple
    return alias_model


def estimate_factorjoin_query(model, context):
    alias_model = make_alias_model(model, context)
    bounds = alias_model.get_cardinality_bound_all(context["sql"], context["subplans"])
    return bounds[-1] if bounds else None


def read_truth(path):
    rows = []
    with Path(path).open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            rows.append(row)
    return rows


def model_path(model_dir, bucket_method, n_bins):
    return Path(model_dir) / f"model_ldbc_{bucket_method}_{n_bins}.pkl"


def build(args):
    from Evaluation.training import train_one_stats

    out = Path(args.model_dir)
    out.mkdir(parents=True, exist_ok=True)
    target = model_path(out, args.bucket_method, args.n_bins)
    if target.exists() and not args.force:
        print(f"model exists: {target}")
        return target
    train_one_stats(
        dataset="ldbc",
        data_path=args.data_path,
        model_folder=str(out),
        n_dim_dist=args.n_dim_dist,
        n_bins=args.n_bins,
        bucket_method=args.bucket_method,
        save_bucket_bins=args.save_bucket_bins,
        get_bin_means=args.get_bin_means,
        seed=args.seed,
        validate=False,
    )
    return target


def query(args):
    truth_rows = read_truth(args.truth)
    output = Path(args.output_csv)
    output.parent.mkdir(parents=True, exist_ok=True)

    with Path(args.model_path).open("rb") as f:
        model = pickle.load(f)
    if model.bns is not None:
        for bn in model.bns.values():
            bn.init_inference_method()

    with output.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["query_file", "prediction", "latency_s", "status", "notes"])
        for row in truth_rows:
            query_file = Path(row["sql_path"]).name
            try:
                context = build_factorjoin_query(row["gcard_path"])
                started = time.perf_counter()
                pred = estimate_factorjoin_query(model, context)
                note_prefix = "alias_subplan_self_join" if context["has_self_join"] else "alias_subplan"
                elapsed = time.perf_counter() - started
                if pred is None:
                    writer.writerow([query_file, 1.0, f"{elapsed:.6f}", "ok_floor", f"{note_prefix}: FactorJoin returned None; floored to 1.0"])
                elif not math.isfinite(float(pred)):
                    writer.writerow([query_file, 1.0, f"{elapsed:.6f}", "ok_floor", f"{note_prefix}: FactorJoin returned {pred}; floored to 1.0"])
                elif pred <= 0:
                    writer.writerow([query_file, 1.0, f"{elapsed:.6f}", "ok_floor", f"{note_prefix}: FactorJoin returned {pred}; floored to 1.0"])
                else:
                    writer.writerow([query_file, pred, f"{elapsed:.6f}", "ok", note_prefix])
            except Exception as exc:
                elapsed = time.perf_counter() - started if 'started' in locals() else 0.0
                writer.writerow([query_file, "", f"{elapsed:.6f}", f"failed: {type(exc).__name__}: {exc}", ""])
    print(f"Saved FactorJoin estimates: {output}")
    print(f"Queries: {len(truth_rows)}")


def main():
    parser = argparse.ArgumentParser(description="Build/query FactorJoin for LDBC LSQB/GLogS predicate workload.")
    sub = parser.add_subparsers(dest="command", required=True)

    b = sub.add_parser("build")
    b.add_argument("--data-path", default=DEFAULT_DATA_PATH)
    b.add_argument("--model-dir", required=True)
    b.add_argument("--n-dim-dist", type=int, default=2)
    b.add_argument("--n-bins", type=int, default=200)
    b.add_argument("--bucket-method", default="fixed_start_key")
    b.add_argument("--save-bucket-bins", action="store_true")
    b.add_argument("--get-bin-means", action="store_true")
    b.add_argument("--seed", type=int, default=0)
    b.add_argument("--force", action="store_true")

    q = sub.add_parser("query")
    q.add_argument("--truth", required=True)
    q.add_argument("--model-path", required=True)
    q.add_argument("--output-csv", required=True)

    args = parser.parse_args()
    if args.command == "build":
        build(args)
    elif args.command == "query":
        query(args)


if __name__ == "__main__":
    main()
