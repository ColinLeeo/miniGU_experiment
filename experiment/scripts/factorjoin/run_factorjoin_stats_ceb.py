#!/usr/bin/env python3
import argparse
import csv
import math
import os
import pickle
import sys
import time
from pathlib import Path

import pandas as pd


REPO_ROOT = Path(__file__).resolve().parents[2]
FACTORJOIN_ROOT = REPO_ROOT / "baseline" / "FactorJoin"
sys.path.insert(0, str(FACTORJOIN_ROOT))

from Evaluation.training import train_one_stats  # noqa: E402
from Join_scheme.data_prepare import convert_time_to_int  # noqa: E402


TABLES = {
    "badges": {
        "source": "badges.csv",
        "target": "badges.csv",
        "columns": {"id": "Id", "userid": "UserId", "date": "Date"},
    },
    "votes": {
        "source": "votes.csv",
        "target": "votes.csv",
        "columns": {
            "id": "Id",
            "postid": "PostId",
            "votetypeid": "VoteTypeId",
            "creationdate": "CreationDate",
            "userid": "UserId",
            "bountyamount": "BountyAmount",
        },
    },
    "postHistory": {
        "source": "posthistory.csv",
        "target": "postHistory.csv",
        "columns": {
            "id": "Id",
            "posthistorytypeid": "PostHistoryTypeId",
            "postid": "PostId",
            "creationdate": "CreationDate",
            "userid": "UserId",
        },
    },
    "posts": {
        "source": "posts.csv",
        "target": "posts.csv",
        "columns": {
            "id": "Id",
            "posttypeid": "PostTypeId",
            "creationdate": "CreationDate",
            "score": "Score",
            "viewcount": "ViewCount",
            "owneruserid": "OwnerUserId",
            "answercount": "AnswerCount",
            "commentcount": "CommentCount",
            "favoritecount": "FavoriteCount",
            "lasteditoruserid": "LastEditorUserId",
        },
    },
    "users": {
        "source": "users.csv",
        "target": "users.csv",
        "columns": {
            "id": "Id",
            "reputation": "Reputation",
            "creationdate": "CreationDate",
            "views": "Views",
            "upvotes": "UpVotes",
            "downvotes": "DownVotes",
        },
    },
    "comments": {
        "source": "comments.csv",
        "target": "comments.csv",
        "columns": {
            "id": "Id",
            "postid": "PostId",
            "score": "Score",
            "creationdate": "CreationDate",
            "userid": "UserId",
        },
    },
    "postLinks": {
        "source": "postlinks.csv",
        "target": "postLinks.csv",
        "columns": {
            "id": "Id",
            "creationdate": "CreationDate",
            "postid": "PostId",
            "relatedpostid": "RelatedPostId",
            "linktypeid": "LinkTypeId",
        },
    },
    "tags": {
        "source": "tags.csv",
        "target": "tags.csv",
        "columns": {"id": "Id", "count": "Count", "excerptpostid": "ExcerptPostId"},
    },
}


def prepare_data(source_dir: Path, target_dir: Path) -> None:
    marker = target_dir / ".prepared"
    if marker.exists():
        return
    target_dir.mkdir(parents=True, exist_ok=True)
    for spec in TABLES.values():
        df = pd.read_csv(source_dir / spec["source"])
        df = df.rename(columns=spec["columns"])
        expected = list(spec["columns"].values())
        missing = [col for col in expected if col not in df.columns]
        if missing:
            raise ValueError(f"{spec['source']} missing columns after rename: {missing}")
        df = df[expected]
        df.to_csv(target_dir / spec["target"], index=False)
    convert_time_to_int(str(target_dir))
    marker.write_text("ok\n")


def load_truth(truth_csv: Path) -> pd.DataFrame:
    truth = pd.read_csv(truth_csv)
    truth["query"] = truth["query"].astype(str)
    return truth


def qerror(prediction: float, truth: float) -> float:
    pred = max(float(prediction), 1.0)
    true = max(float(truth), 1.0)
    return max(pred / true, true / pred)


def estimate(model_path: Path, sql_dir: Path, truth: pd.DataFrame) -> pd.DataFrame:
    with model_path.open("rb") as f:
        model = pickle.load(f)
    for bn in model.bns.values():
        bn.init_inference_method()

    rows = []
    for row in truth.itertuples(index=False):
        query = row.query
        sql_path = sql_dir / f"{query}.sql"
        sql = sql_path.read_text().strip()
        start = time.time()
        status = "ok"
        notes = ""
        try:
            pred = model.get_cardinality_bound_one(sql)
            if pred is None or not math.isfinite(float(pred)):
                status = "fallback_one"
                notes = f"non-finite prediction: {pred}"
                pred = 1.0
            pred = max(float(pred), 1.0)
        except Exception as exc:
            status = "error"
            notes = repr(exc)
            pred = 1.0
        latency = time.time() - start
        true_card = float(row.truth_cardinality)
        rows.append(
            {
                "query": query,
                "truth_cardinality": true_card,
                "prediction": pred,
                "latency_s": latency,
                "qerror": qerror(pred, true_card),
                "status": status,
                "notes": notes,
                "num_tables": row.num_tables,
                "num_edges": row.num_edges,
                "num_predicates": row.num_predicates,
            }
        )
    return pd.DataFrame(rows)


def write_distribution(df: pd.DataFrame, path: Path) -> None:
    buckets = [
        ("<=1.5", lambda s: s <= 1.5),
        ("(1.5,2]", lambda s: (s > 1.5) & (s <= 2)),
        ("(2,5]", lambda s: (s > 2) & (s <= 5)),
        ("(5,10]", lambda s: (s > 5) & (s <= 10)),
        ("(10,100]", lambda s: (s > 10) & (s <= 100)),
        ("(100,1000]", lambda s: (s > 100) & (s <= 1000)),
        (">1000", lambda s: s > 1000),
    ]
    total = len(df)
    with path.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["bucket", "count", "fraction"])
        writer.writeheader()
        for name, mask_fn in buckets:
            count = int(mask_fn(df["qerror"]).sum())
            writer.writerow({"bucket": name, "count": count, "fraction": count / total if total else 0.0})


def write_summary(df: pd.DataFrame, path: Path) -> None:
    q = df["qerror"]
    summary = {
        "count": int(q.count()),
        "mean": float(q.mean()),
        "median": float(q.median()),
        "p90": float(q.quantile(0.90)),
        "p95": float(q.quantile(0.95)),
        "p99": float(q.quantile(0.99)),
        "max": float(q.max()),
        "avg_latency_s": float(df["latency_s"].mean()),
        "total_latency_s": float(df["latency_s"].sum()),
        "ok": int((df["status"] == "ok").sum()),
        "errors": int((df["status"] == "error").sum()),
    }
    pd.DataFrame([summary]).to_csv(path, index=False)


def update_baseline_outputs(df: pd.DataFrame, baseline_csv: Path, summary_csv: Path) -> None:
    if not baseline_csv.exists():
        return
    base = pd.read_csv(baseline_csv)
    qmap = df.set_index("query")["qerror"].to_dict()
    base["FactorJoin"] = base["query"].map(qmap)
    base.to_csv(baseline_csv, index=False)

    methods = [col for col in base.columns if col not in ("dataset", "query")]
    rows = []
    for method in methods:
        vals = pd.to_numeric(base[method], errors="coerce").dropna()
        rows.append(
            {
                "dataset": "stats_ceb",
                "method": method,
                "count": int(vals.count()),
                "mean": float(vals.mean()),
                "median": float(vals.median()),
                "p90": float(vals.quantile(0.90)),
                "p95": float(vals.quantile(0.95)),
                "p99": float(vals.quantile(0.99)),
                "max": float(vals.max()),
            }
        )
    pd.DataFrame(rows).to_csv(summary_csv, index=False)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-data", type=Path, default=REPO_ROOT / "dataset" / "stats_ceb")
    parser.add_argument("--sql-dir", type=Path, default=REPO_ROOT / "patterns" / "sql" / "stats_ceb")
    parser.add_argument("--truth", type=Path, default=REPO_ROOT / "patterns" / "gcard" / "stats_ceb" / "truth.csv")
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=REPO_ROOT / "result" / "sql_baselines" / "stats_ceb_factorjoin",
    )
    parser.add_argument("--bins", type=int, default=200)
    parser.add_argument("--bucket-method", default="greedy")
    parser.add_argument("--force-train", action="store_true")
    args = parser.parse_args()

    data_dir = args.output_dir / "data"
    model_dir = args.output_dir / "model"
    factorjoin_dir = args.output_dir / "factorjoin"
    factorjoin_dir.mkdir(parents=True, exist_ok=True)
    model_dir.mkdir(parents=True, exist_ok=True)

    prepare_data(args.source_data, data_dir)
    model_path = model_dir / f"model_stats_{args.bucket_method}_{args.bins}.pkl"
    if args.force_train or not model_path.exists():
        train_one_stats(
            "stats",
            str(data_dir),
            str(model_dir),
            n_dim_dist=2,
            n_bins=args.bins,
            bucket_method=args.bucket_method,
            validate=False,
        )

    truth = load_truth(args.truth)
    estimates = estimate(model_path, args.sql_dir, truth)
    estimates.to_csv(factorjoin_dir / "estimate.csv", index=False)
    estimates.to_csv(args.output_dir / "stats_ceb_factorjoin_qerror.csv", index=False)
    write_summary(estimates, args.output_dir / "stats_ceb_factorjoin_qerror_summary.csv")
    write_distribution(estimates, args.output_dir / "stats_ceb_factorjoin_qerror_distribution.csv")

    update_baseline_outputs(
        estimates,
        Path("/home/zxz/miniGU/experiment/results/qerror_imdb_stats_k2_d5_latest/stats_ceb_k2_d5_qerror_baselines.csv"),
        Path("/home/zxz/miniGU/experiment/results/qerror_imdb_stats_k2_d5_latest/stats_ceb_k2_d5_qerror_summary.csv"),
    )

    print(estimates["status"].value_counts(dropna=False).to_string())
    print(pd.read_csv(args.output_dir / "stats_ceb_factorjoin_qerror_summary.csv").to_string(index=False))
    print(pd.read_csv(args.output_dir / "stats_ceb_factorjoin_qerror_distribution.csv").to_string(index=False))


if __name__ == "__main__":
    main()
