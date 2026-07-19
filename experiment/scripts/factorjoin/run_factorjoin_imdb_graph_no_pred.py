#!/usr/bin/env python3
import argparse
import copy
import csv
import json
import math
import pickle
import sys
import time
import types
from pathlib import Path

import numpy as np


EXP = Path(__file__).resolve().parents[2]
FACTORJOIN = EXP / "baseline" / "FactorJoin"
sys.path.insert(0, str(FACTORJOIN))

from Join_scheme.binning import Table_bucket  # noqa: E402
from Join_scheme.bound import Bound_ensemble  # noqa: E402
from Join_scheme.factor import Factor  # noqa: E402
from Schemas.graph_representation import SchemaGraph, Table  # noqa: E402


CATALOG = EXP / "catalogs/imdb/factorjoin_sampled_buckets_all_tables"
PATTERNS = EXP / "patterns/gcard/imdb"
HISTORICAL = (
    EXP
    / "result/sql_baselines/imdb_q1_q28_7baselines/factorjoin/estimate.csv"
)


def qerror(estimate, truth):
    estimate = max(float(estimate), 1.0)
    truth = max(float(truth), 1.0)
    return max(estimate / truth, truth / estimate)


def load_truth(paths):
    truths = {}
    for path in paths:
        with path.open(newline="", encoding="utf-8") as handle:
            for row in csv.DictReader(handle):
                value = row.get("true_cardinality") or row.get("truth_cardinality")
                if value:
                    truths[row["query"]] = float(value)
    return truths


def mode_by_bin(frame, value_col, bin_col, bin_count, scale):
    freq = frame.groupby([bin_col, value_col]).size().groupby(level=0).max()
    result = np.zeros(bin_count)
    values = freq.to_numpy(dtype=float)
    values = np.where(values == 1.0, 1.0, values * scale)
    result[freq.index.to_numpy(dtype=int)] = values
    return result


class GraphFactorJoin:
    def __init__(self):
        with (CATALOG / "samples_all_tables_1000000_seed0.pkl").open("rb") as handle:
            self.samples = pickle.load(handle)
        with (CATALOG / "factors_all_tables_200_1000000_seed0.pkl").open("rb") as handle:
            self.aggregates = pickle.load(handle)
        with (CATALOG / "global_key_buckets_200.pkl").open("rb") as handle:
            self.global_buckets = pickle.load(handle)

    def relation_factor(self, group, name, columns):
        item = self.samples[group][name]
        sample = item["sample"]
        aggregate = self.aggregates[group][name]
        variables = []
        pdfs = {}
        modes = {}
        bin_sizes = {}
        for value_col, bin_col, domain in columns:
            variable = f"{name}.{value_col}"
            bin_count = len(self.global_buckets[domain])
            counts = aggregate.groupby(bin_col)["count"].sum()
            distribution = np.zeros(bin_count)
            distribution[counts.index.to_numpy(dtype=int)] = counts.to_numpy()
            variables.append(variable)
            pdfs[variable] = distribution / float(item["rows_total"])
            modes[variable] = mode_by_bin(
                sample, value_col, bin_col, bin_count, float(item["scale"])
            )
            bin_sizes[variable] = bin_count
        factor = Factor(
            name,
            item["rows_total"],
            variables,
            pdfs,
            na_values={variable: 1.0 for variable in variables},
        )
        bucket = Table_bucket(name, variables, bin_sizes, modes)
        return factor, bucket

    def build_query_model(self, pattern):
        vertices = {
            int(vertex["id"]): vertex["label"].lower()
            for vertex in pattern["vertices"]
        }
        edges = {}
        schema = SchemaGraph()
        factors = {}
        buckets = {}

        for label in sorted(set(vertices.values())):
            item = self.samples["vertex"][label]
            schema.add_table(
                Table(label, attributes=["id"], table_size=item["rows_total"])
            )
            factors[label], buckets[label] = self.relation_factor(
                "vertex", label, [("id", "id_bin", label)]
            )

        for edge in pattern["edges"]:
            label = edge["label"].lower()
            src_label = vertices[int(edge["src"])]
            dst_label = vertices[int(edge["dst"])]
            edges[label] = (src_label, dst_label)
            item = self.samples["edge"][label]
            schema.add_table(
                Table(label, attributes=["src", "dst"], table_size=item["rows_total"])
            )
            schema.add_relationship(label, "src", src_label, "id")
            schema.add_relationship(label, "dst", dst_label, "id")
            factors[label], buckets[label] = self.relation_factor(
                "edge",
                label,
                [("src", "src_bin", src_label), ("dst", "dst_bin", dst_label)],
            )

        model = Bound_ensemble(
            buckets,
            schema,
            n_dim_dist=2,
            ground_truth_factors_no_filter=factors,
        )

        def no_filter_factors(self, query_str, query_name, tables_alias, join_keys):
            return {
                alias: copy.deepcopy(factors[table])
                for alias, table in tables_alias.items()
                if alias in join_keys and table in factors
            }

        model.get_all_id_conidtional_distribution_sample = types.MethodType(
            no_filter_factors, model
        )
        return model, vertices

    @staticmethod
    def sql_and_order(pattern, vertices):
        vertex_alias = {vertex_id: f"v{vertex_id}" for vertex_id in vertices}
        from_items = [
            f"{vertices[vertex_id]} AS {vertex_alias[vertex_id]}"
            for vertex_id in sorted(vertices)
        ]
        conditions = []
        edge_aliases = []
        for edge in sorted(pattern["edges"], key=lambda row: int(row["id"])):
            alias = f"e{edge['id']}"
            src = int(edge["src"])
            dst = int(edge["dst"])
            edge_aliases.append((alias, src, dst))
            from_items.append(f"{edge['label'].lower()} AS {alias}")
            conditions.append(f"{alias}.src = {vertex_alias[src]}.id")
            conditions.append(f"{alias}.dst = {vertex_alias[dst]}.id")
        sql = (
            "SELECT COUNT(*) FROM "
            + ", ".join(from_items)
            + " WHERE "
            + " AND ".join(conditions)
        )

        seen = set()
        order = []

        def add(alias):
            if alias not in seen:
                seen.add(alias)
                order.append(alias)

        first_edge, first_src, first_dst = edge_aliases[0]
        add(first_edge)
        add(vertex_alias[first_src])
        add(vertex_alias[first_dst])
        while len(seen) < len(from_items):
            changed = False
            for edge_alias, src, dst in edge_aliases:
                if edge_alias in seen:
                    continue
                if vertex_alias[src] in seen or vertex_alias[dst] in seen:
                    add(edge_alias)
                    add(vertex_alias[src])
                    add(vertex_alias[dst])
                    changed = True
            if not changed:
                for alias in list(vertex_alias.values()) + [
                    row[0] for row in edge_aliases
                ]:
                    add(alias)

        subplans = []
        accumulated = order[0]
        for alias in order[1:]:
            subplans.append((alias, accumulated))
            accumulated = " ".join(sorted(accumulated.split() + [alias]))
        return sql, subplans

    def prepare(self, query):
        pattern = json.loads((PATTERNS / f"{query}.json").read_text())
        model, vertices = self.build_query_model(pattern)
        sql, subplans = self.sql_and_order(pattern, vertices)
        return model, sql, subplans

    def estimate(self, query):
        model, sql, subplans = self.prepare(query)
        started = time.perf_counter()
        prediction = model.get_cardinality_bound_all(sql, subplans)[-1]
        return float(prediction), time.perf_counter() - started


def historical_q1():
    with HISTORICAL.open(newline="", encoding="utf-8") as handle:
        row = next(csv.DictReader(handle))
    if row["query"] != "q1":
        raise RuntimeError("Historical FactorJoin file does not start with q1")
    return float(row["prediction"])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--truth",
        nargs="+",
        type=Path,
        default=[EXP / "result/truecard/imdb_q29_q32_duckdb/true_card.csv"],
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=EXP
        / "result/sql_baselines/imdb_q29_q32_8baselines/factorjoin/estimate.csv",
    )
    parser.add_argument("--queries", nargs="+", default=["q29", "q30", "q31", "q32"])
    parser.add_argument("--skip-q1-validation", action="store_true")
    args = parser.parse_args()

    estimator = GraphFactorJoin()
    if not args.skip_q1_validation:
        restored, _ = estimator.estimate("q1")
        expected = historical_q1()
        print(
            "q1 audit after inverse-sampling mode correction: "
            f"corrected={restored}, historical={expected}"
        )

    truths = load_truth(args.truth)
    rows = []
    for query in args.queries:
        prediction, latency = estimator.estimate(query)
        truth = truths[query]
        rows.append(
            {
                "query": query,
                "truth_cardinality": f"{truth:.17g}",
                "prediction": f"{prediction:.17g}",
                "latency_s": f"{latency:.17g}",
                "qerror": f"{qerror(prediction, truth):.17g}",
                "status": "ok",
                "notes": "restored_bound_ensemble_sampled_all_tables;n_bins=200;sample_rows=1000000;seed=0;inverse_sampling_mode_scale=1;latency_scope=bound_inference_only",
            }
        )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    print(args.output)


if __name__ == "__main__":
    main()
