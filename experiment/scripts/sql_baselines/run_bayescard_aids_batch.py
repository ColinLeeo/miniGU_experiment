#!/usr/bin/env python3
import argparse
import csv
import json
import math
import pickle
import time
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
EXPERIMENT = ROOT / "experiment"
BAYESCARD_DIR = EXPERIMENT / "baseline/SafeBound/bayescard"
sys.path.insert(0, str(BAYESCARD_DIR))
DEFAULT_MODEL_DIR = EXPERIMENT / "result/sql_baselines/aids_merged_7baselines/bayescard"
DEFAULT_PATTERN_DIR = EXPERIMENT / "patterns/gcard/aids_merged"
DEFAULT_TRUTH = EXPERIMENT / "result/truecard/aids_merged_current/true_card.csv"
DEFAULT_OUTPUT = EXPERIMENT / "result/sql_baselines/aids_merged_7baselines/bayescard/estimate.csv"


def row_num(bn):
    return float(getattr(bn, "row_num", getattr(bn, "nrows", 1.0)))


def table_row_num(all_bns, table):
    if table not in all_bns:
        raise KeyError(f"Missing BayesCard BN for table {table}")
    return row_num(all_bns[table])


def validate_model_covers_patterns(model, pattern_paths):
    all_bns = model.get("bns", {})
    required = set()
    for pattern_path in pattern_paths:
        pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
        required.update(str(v["label"]).lower() for v in pattern["vertices"])
        required.update(str(e["label"]).lower() for e in pattern["edges"])
    missing = sorted(required - set(all_bns))
    if missing:
        raise RuntimeError(f"BayesCard model is missing required BNs: {missing}")


def estimate_one(model, pattern_path):
    started = time.perf_counter()
    pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
    vertices = {v["id"]: v["label"].lower() for v in pattern["vertices"]}
    all_bns = model.get("bns", {})

    cardinality = 1.0
    for edge in pattern["edges"]:
        table = str(edge["label"]).lower()
        cardinality *= table_row_num(all_bns, table)

    vertex_refs = {}
    for edge in pattern["edges"]:
        table = str(edge["label"]).lower()
        vertex_refs.setdefault(edge["src"], []).append(table)
        vertex_refs.setdefault(edge["dst"], []).append(table)

    for vid, tables in vertex_refs.items():
        if len(tables) <= 1:
            continue
        v_label = vertices[vid]
        domain_size = table_row_num(all_bns, v_label)
        for _ in range(len(tables) - 1):
            cardinality /= domain_size

    elapsed = time.perf_counter() - started
    return max(1.0, float(cardinality)), elapsed


def main():
    parser = argparse.ArgumentParser(description="Batch BayesCard AIDS estimates for miniGU JSON patterns.")
    parser.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL_DIR)
    parser.add_argument("--pattern-dir", type=Path, default=DEFAULT_PATTERN_DIR)
    parser.add_argument("--truth", type=Path, default=DEFAULT_TRUTH)
    parser.add_argument("--output-csv", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()

    with (args.model_dir / "bayescard.pkl").open("rb") as f:
        model = pickle.load(f)
    for bn in model.get("bns", {}).values():
        bn.init_inference_method()
        bn.infer_algo = "exact"

    rows = list(csv.DictReader(args.truth.open(newline="", encoding="utf-8")))
    pattern_paths = [args.pattern_dir / (row["pattern"] + ".json") for row in rows]
    validate_model_covers_patterns(model, pattern_paths)
    args.output_csv.parent.mkdir(parents=True, exist_ok=True)
    with args.output_csv.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["query_file", "prediction", "latency_s", "status", "notes", "topology", "query", "pattern", "true_cardinality"])
        for row in rows:
            pattern_rel = row["pattern"] + ".json"
            try:
                pred, elapsed = estimate_one(model, args.pattern_dir / pattern_rel)
                status = "ok" if math.isfinite(pred) and pred > 0 else "ok_floor"
                if status != "ok":
                    pred = 1.0
                writer.writerow([pattern_rel, f"{pred:.17g}", f"{elapsed:.6f}", status, "bayescard_independent_join_wrapper_edge_bn_required", row["topology"], row["query"], row["pattern"], row["true_cardinality"]])
            except Exception as exc:
                writer.writerow([pattern_rel, "", "0.000000", f"failed: {type(exc).__name__}: {exc}", "", row["topology"], row["query"], row["pattern"], row["true_cardinality"]])
    print(f"Saved BayesCard estimates: {args.output_csv}")
    print(f"Queries: {len(rows)}")


if __name__ == "__main__":
    main()
