#!/usr/bin/env python3
import argparse
import csv
import json
import math
import pickle
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BAYESCARD_DIR = ROOT / "baseline/SafeBound/bayescard"
PATTERNS = ROOT / "patterns/gcard/dblp"
MODEL_DIR = ROOT / "catalogs/dblp/bayescard"
TRUTH = ROOT / "result/truecard/dblp/true_card.csv"
OUT = ROOT / "result/baselines/bayescard/estimate/dblp.csv"


def load_truth(path):
    if not path.exists():
        return {}
    with path.open(newline="", encoding="utf-8") as f:
        return {r["query"]: r for r in csv.DictReader(f) if r.get("status") == "ok"}


def load_model(model_dir):
    sys.path.insert(0, str(BAYESCARD_DIR))
    model_path = Path(model_dir) / "bayescard.pkl"
    with model_path.open("rb") as f:
        data = pickle.load(f)
    if isinstance(data, dict) and "bns" in data:
        return data["bns"]
    return data


def row_count_for_table(all_bns, label):
    bn = all_bns.get(label)
    if bn is None:
        raise KeyError(f"Missing BayesCard BN for table {label}")
    return float(getattr(bn, "row_num", getattr(bn, "nrows", 1.0)))


def validate_model_covers_patterns(all_bns, pattern_paths):
    required = set()
    for pattern_path in pattern_paths:
        pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
        required.update(v["label"].lower() for v in pattern["vertices"])
        required.update(e["label"].lower() for e in pattern["edges"])
    missing = sorted(required - set(all_bns))
    if missing:
        raise RuntimeError(f"BayesCard model is missing required BNs: {missing}")


def estimate_one(pattern_path, all_bns):
    started = time.perf_counter()
    pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
    vertices = {v["id"]: v["label"].lower() for v in pattern["vertices"]}
    edges = pattern["edges"]

    card = 1.0
    for e in edges:
        t_name = e["label"].lower()
        card *= row_count_for_table(all_bns, t_name)

    vertex_refs = {}
    for e in edges:
        for side in ("src", "dst"):
            vertex_refs.setdefault(e[side], []).append(e["label"].lower())

    for vid, tables in vertex_refs.items():
        if len(tables) > 1:
            domain_size = max(row_count_for_table(all_bns, vertices[vid]), 1.0)
            for _ in range(len(tables) - 1):
                card /= domain_size

    elapsed = time.perf_counter() - started
    return max(1.0, float(card)), elapsed


def main():
    ap = argparse.ArgumentParser(description="Batch-estimate DBLP queries with BayesCard join-independence wrapper.")
    ap.add_argument("--model-dir", type=Path, default=MODEL_DIR)
    ap.add_argument("--pattern-dir", type=Path, default=PATTERNS)
    ap.add_argument("--truth", type=Path, default=TRUTH)
    ap.add_argument("--output", type=Path, default=OUT)
    args = ap.parse_args()

    truth = load_truth(args.truth)
    all_bns = load_model(args.model_dir)
    pattern_paths = sorted(args.pattern_dir.glob("*.json"))
    validate_model_covers_patterns(all_bns, pattern_paths)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["query", "prediction", "latency_s", "status", "true_cardinality", "notes"])
        for pattern in pattern_paths:
            query = pattern.stem
            try:
                pred, elapsed = estimate_one(pattern, all_bns)
                status = "ok" if math.isfinite(pred) and pred > 0 else "ok_floor"
                if status != "ok":
                    pred = 1.0
                w.writerow([query, f"{pred:.17g}", f"{elapsed:.6f}", status, truth.get(query, {}).get("true_cardinality", ""), "bayescard_join_independence_edge_bn_required"])
            except Exception as exc:
                w.writerow([query, "", "0.000000", f"failed: {type(exc).__name__}: {exc}", truth.get(query, {}).get("true_cardinality", ""), ""])
    print(f"Saved BayesCard DBLP estimates: {args.output}")


if __name__ == "__main__":
    main()
