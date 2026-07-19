#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import math
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXP = ROOT
REPO = EXP.parent
TRUE_DIR = EXP / "patterns/truecard/youtube"
NUM_RE = re.compile(r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?")

BASELINES = ["gcard", "safebound", "bayescard", "factorjoin", "color", "pathce", "gcare", "alley"]
PY_DEPS = os.environ.get("BASELINE_PYTHON", sys.executable)


def natural_key(path: Path):
    return [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", path.as_posix())]


def truecards() -> dict[str, float]:
    out = {}
    for p in TRUE_DIR.glob("*.graph"):
        txt = p.read_text(encoding="utf-8").strip()
        if txt:
            out[p.stem] = float(txt)
    return out


def qerror(est, truth):
    est = max(float(est), 1.0)
    truth = max(float(truth), 1.0)
    return max(est / truth, truth / est)


def run(cmd, *, cwd=REPO, log: Path | None = None, env=None, timeout=None):
    if log is not None:
        log.parent.mkdir(parents=True, exist_ok=True)
        with log.open("w", encoding="utf-8") as f:
            proc = subprocess.run(cmd, cwd=str(cwd), stdout=f, stderr=subprocess.STDOUT, text=True, env=env, timeout=timeout)
    else:
        proc = subprocess.run(cmd, cwd=str(cwd), text=True, env=env, timeout=timeout)
    if proc.returncode != 0:
        where = f"; see {log}" if log else ""
        raise RuntimeError(f"command failed ({proc.returncode}): {' '.join(map(str, cmd))}{where}")
    return proc


def output_csv(baseline: str) -> Path:
    return EXP / f"result/baselines/{baseline}/estimate/youtube.csv"


def write_rows(path: Path, rows: list[dict[str, object]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fields = ["query", "estimate", "latency_s", "status", "error", "true_cardinality", "qerror"]
    with path.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for row in rows:
            w.writerow({field: row.get(field, "") for field in fields})
    print(f"saved={path}")


def annotate_rows(rows):
    truth = truecards()
    for row in rows:
        query = str(row.get("query", ""))
        est = row.get("estimate", "")
        true = truth.get(query)
        if true is not None:
            row["true_cardinality"] = f"{true:.17g}"
        if str(row.get("status", "")).startswith("ok") and est not in ("", None) and true is not None:
            try:
                row["qerror"] = f"{qerror(float(est), true):.17g}"
            except Exception:
                row["qerror"] = ""
    return rows


def julia_env():
    env = os.environ.copy()
    home = Path.home()
    env.setdefault("JULIA_DEPOT_PATH", str(home / ".julia"))
    env["PATH"] = f"{home / '.local/julia/julia-1.10.4/bin'}:{home / '.local/bin'}:" + env.get("PATH", "")
    return env

def julia_bin() -> str:
    candidates = [os.environ.get("JULIA_BIN", "julia"), str(Path.home() / ".local/bin/julia"), str(Path.home() / ".local/julia/julia-1.10.4/bin/julia")]
    for c in candidates:
        if not c:
            continue
        try:
            subprocess.run([c, "--version"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            return c
        except Exception:
            pass
    raise FileNotFoundError("julia not found")


def build_color(args):
    jb = julia_bin()
    summary = EXP / "catalogs/youtube/color/youtube_mix_6_50000.obj"
    if summary.exists() and not args.force:
        print(f"exists={summary}")
        return
    run([jb, f"--project={EXP / 'baseline/color'}", str(EXP / "baseline/color/scripts/build.jl"), "-d", str(EXP / "catalogs/youtube/color/youtube.graph"), "-o", str(summary)], log=EXP / "result/baselines/color/build-logs/youtube.log", env=julia_env())


def estimate_color(args):
    build_color(args)
    jb = julia_bin()
    patterns = sorted((EXP / "patterns/gcare/youtube").glob("*.graph"), key=natural_key)
    if args.limit:
        patterns = patterns[: args.limit]
    qfile = EXP / "run_tmp/color_youtube_queries.txt"
    qfile.parent.mkdir(parents=True, exist_ok=True)
    qfile.write_text("\n".join(str(p) for p in patterns) + "\n", encoding="utf-8")
    raw = EXP / "result/baselines/color/estimate/youtube_raw.csv"
    run([jb, f"--project={EXP / 'baseline/color'}", str(EXP / "baseline/color/scripts/estimate_batch.jl"), "-f", str(qfile), "-s", str(EXP / "catalogs/youtube/color/youtube_mix_6_50000.obj"), "-o", str(raw)], env=julia_env())
    rows = []
    with raw.open(newline="", encoding="utf-8") as f:
        for r in csv.DictReader(f):
            rows.append({"query": r.get("query", ""), "estimate": r.get("estimate", ""), "latency_s": r.get("latency_s", ""), "status": r.get("status", ""), "error": r.get("error", "")})
    write_rows(output_csv("color"), annotate_rows(rows))


def build_gcare(args):
    graph_dir = EXP / "catalogs/youtube/gcare/youtube"
    if Path(str(graph_dir) + ".graph.meta").exists() and not args.force:
        print(f"exists={graph_dir}.graph.meta")
        return
    run([str(EXP / "baseline/gcare/build/gcare_graph"), "-b", "-m", "wj", "-i", str(EXP / "catalogs/youtube/gcare/youtube.graph"), "-d", str(graph_dir)], log=EXP / "result/baselines/gcare/build-logs/youtube_wj.log")


def parse_last_number(text: str) -> str:
    nums = NUM_RE.findall(text)
    return nums[-1] if nums else ""


def estimate_gcare(args):
    build_gcare(args)
    patterns = sorted((EXP / "patterns/gcare/youtube").glob("*.graph"), key=natural_key)
    if args.limit:
        patterns = patterns[: args.limit]
    graph_dir = EXP / "catalogs/youtube/gcare/youtube"
    rows = []
    for p in patterns:
        start = time.perf_counter()
        try:
            proc = subprocess.run([str(EXP / "baseline/gcare/build/gcare_graph"), "-q", "-m", "wj", "-i", str(p), "-d", str(graph_dir), "-p", str(args.ratio), "-n", str(args.repeat), "-s", str(args.seed)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=args.timeout)
            elapsed = time.perf_counter() - start
        except subprocess.TimeoutExpired:
            rows.append({"query": p.stem, "estimate": "", "latency_s": f"{time.perf_counter()-start:.6f}", "status": "timeout", "error": f"timeout_s={args.timeout:g}"})
            continue
        est = parse_last_number(proc.stdout)
        if proc.returncode == 0 and est:
            rows.append({"query": p.stem, "estimate": est, "latency_s": f"{elapsed:.6f}", "status": "ok", "error": ""})
        else:
            rows.append({"query": p.stem, "estimate": "", "latency_s": f"{elapsed:.6f}", "status": "failed", "error": (proc.stderr or proc.stdout).replace("\n", " ").replace(",", " ")[:500]})
    write_rows(output_csv("gcare"), annotate_rows(rows))


def build_pathce(args):
    graph = EXP / "graphs/youtube/pathce/youtube.bincode"
    if not graph.exists() or args.force:
        graph.parent.mkdir(parents=True, exist_ok=True)
        run([str(EXP / "baseline/pathce/target/release/pathce"), "serialize", "-i", str(EXP / "datasets/youtube_pathce_pair"), "-s", str(EXP / "schemas/youtube/youtube_pathce_pair_schema.json"), "-o", str(graph)], log=EXP / "result/baselines/pathce/build-logs/youtube_serialize.log")
    catalog = EXP / f"catalogs/youtube/pathce/youtube_{args.k}_{args.d}_{args.m}"
    if catalog.exists() and not args.force:
        print(f"exists={catalog}")
        return
    catalog.parent.mkdir(parents=True, exist_ok=True)
    run([str(EXP / "baseline/pathce/target/release/pathce"), "analyze", "-s", str(EXP / "schemas/youtube/youtube_pathce_pair_schema.json"), "-g", str(graph), "--greedy", "-t", str(args.threads), "-o", str(catalog), "--buckets", str(args.m), "--max-path-length", str(args.k), "--max-star-degree", str(args.d)], log=EXP / f"result/baselines/pathce/build-logs/youtube_{args.k}_{args.d}_{args.m}.log")


def pathce_parse(stdout: str) -> str:
    val = ""
    for line in stdout.splitlines():
        stripped = line.strip()
        if NUM_RE.fullmatch(stripped):
            val = stripped
    return val or (NUM_RE.findall(stdout)[-1] if NUM_RE.findall(stdout) else "")


def estimate_pathce(args):
    build_pathce(args)
    catalog = EXP / f"catalogs/youtube/pathce/youtube_{args.k}_{args.d}_{args.m}"
    patterns = sorted((EXP / "patterns/pathce/youtube").glob("*.json"), key=natural_key)
    if args.limit:
        patterns = patterns[: args.limit]
    rows = []
    for p in patterns:
        start = time.perf_counter()
        try:
            proc = subprocess.run([str(EXP / "baseline/pathce/target/release/pathce"), "estimate", "-c", str(catalog), "-p", str(p), "--max-path-length", str(args.k), "--max-star-degree", str(args.d)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=args.timeout)
        except subprocess.TimeoutExpired:
            rows.append({"query": p.stem, "estimate": "", "latency_s": f"{time.perf_counter()-start:.6f}", "status": "timeout", "error": f"timeout_s={args.timeout:g}"})
            continue
        elapsed = time.perf_counter() - start
        if proc.returncode == 0:
            rows.append({"query": p.stem, "estimate": pathce_parse(proc.stdout), "latency_s": f"{elapsed:.6f}", "status": "ok", "error": ""})
        else:
            rows.append({"query": p.stem, "estimate": "", "latency_s": f"{elapsed:.6f}", "status": "failed", "error": (proc.stderr or proc.stdout).replace("\n", " ").replace(",", " ")[:500]})
    write_rows(output_csv("pathce"), annotate_rows(rows))


def build_bayescard(args):
    model = EXP / "catalogs/youtube/bayescard/youtube/bayescard.pkl"
    if model.exists() and not args.force:
        print(f"exists={model}")
        return
    run([PY_DEPS, str(EXP / "scripts/sql_baselines/run_bayescard.py"), "build", "--data-dir", str(EXP / "datasets/youtube"), "--model-dir", str(model.parent), "--sample-size", str(args.sample_size)], log=EXP / "result/baselines/bayescard/build-logs/youtube.log")


def estimate_bayescard(args):
    build_bayescard(args)
    model_dir = EXP / "catalogs/youtube/bayescard/youtube"
    patterns = sorted((EXP / "patterns/factorjoin/youtube").glob("*.json"), key=natural_key)
    if args.limit:
        patterns = patterns[: args.limit]
    rows = []
    for p in patterns:
        start = time.perf_counter()
        proc = subprocess.run([PY_DEPS, str(EXP / "scripts/sql_baselines/run_bayescard.py"), "estimate", "--model-dir", str(model_dir), "--query-pattern", str(p)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        elapsed = time.perf_counter() - start
        if proc.returncode == 0 and proc.stdout.strip():
            parts = proc.stdout.strip().splitlines()[-1].split(",")
            rows.append({"query": p.stem, "estimate": parts[0], "latency_s": parts[1] if len(parts) > 1 else f"{elapsed:.6f}", "status": "ok", "error": ""})
        else:
            rows.append({"query": p.stem, "estimate": "", "latency_s": f"{elapsed:.6f}", "status": "failed", "error": (proc.stderr or proc.stdout).replace("\n", " ").replace(",", " ")[:500]})
    write_rows(output_csv("bayescard"), annotate_rows(rows))


def build_safebound(args):
    stats = EXP / "catalogs/youtube/safebound/youtube.pkl"
    if stats.exists() and not args.force:
        print(f"exists={stats}")
        return
    run([PY_DEPS, str(EXP / "scripts/safebound/run_safebound.py"), "build", "--workspace", str(EXP), "--dataset-dir", str(EXP / "datasets/youtube"), "--pattern-dir", str(EXP / "patterns/sql/youtube"), "--output-stats", str(stats), "--num-cores", str(args.threads)], log=EXP / "result/baselines/safebound/build-logs/youtube.log")


def estimate_safebound(args):
    build_safebound(args)
    raw = EXP / "result/baselines/safebound/estimate/youtube_raw.csv"
    run([PY_DEPS, str(EXP / "scripts/safebound/run_safebound.py"), "query", "--workspace", str(EXP), "--pattern-dir", str(EXP / "patterns/sql/youtube"), "--stats-path", str(EXP / "catalogs/youtube/safebound/youtube.pkl"), "--output-csv", str(raw)])
    rows = []
    with raw.open(newline="", encoding="utf-8") as f:
        for r in csv.DictReader(f):
            q = Path(r.get("query", "")).stem
            rows.append({"query": q, "estimate": r.get("estimate", ""), "latency_s": r.get("inference_time_s", ""), "status": "ok", "error": ""})
    if args.limit:
        rows = rows[: args.limit]
    write_rows(output_csv("safebound"), annotate_rows(rows))


def build_factorjoin(args):
    model = EXP / f"catalogs/youtube/factorjoin/model_youtube_{args.factorjoin_bucket_method}_{args.factorjoin_bins}.pkl"
    if model.exists() and not args.force:
        print(f"exists={model}")
        return
    run([PY_DEPS, str(EXP / "scripts/factorjoin/run_factorjoin_youtube.py"), "build", "--data-path", str(EXP / "datasets/youtube/{}.csv"), "--model-dir", str(model.parent), "--n-bins", str(args.factorjoin_bins), "--bucket-method", args.factorjoin_bucket_method], log=EXP / "result/baselines/factorjoin/build-logs/youtube.log")


def estimate_factorjoin(args):
    build_factorjoin(args)
    run([PY_DEPS, str(EXP / "scripts/factorjoin/run_factorjoin_youtube.py"), "query", "--pattern-dir", str(EXP / "patterns/factorjoin/youtube"), "--model-path", str(EXP / f"catalogs/youtube/factorjoin/model_youtube_{args.factorjoin_bucket_method}_{args.factorjoin_bins}.pkl"), "--output-csv", str(output_csv("factorjoin"))] + (["--limit", str(args.limit)] if args.limit else []))


def build_alley(args):
    run([str(EXP / "scripts/alley/build_catalog.sh"), "youtube", "alley", str(args.alley_ratio), str(args.seed)], log=EXP / "result/baselines/alley/build-logs/youtube_wrapper.log")


def estimate_alley(args):
    run([str(EXP / "scripts/alley/estimate_all.sh"), "youtube", "alley", str(args.alley_ratio), str(args.repeat), str(args.seed)], log=EXP / "result/baselines/alley/estimate/youtube_wrapper.log")
    src = EXP / f"result/baselines/alley/estimate/youtube_alley_p{args.alley_ratio}_s{args.seed}.csv"
    rows = []
    with src.open(newline="", encoding="utf-8") as f:
        for r in csv.DictReader(f):
            q = str(r.get("query", "")).split("/")[-1]
            rows.append({"query": q, "estimate": r.get("estimate", ""), "latency_s": r.get("latency_s", ""), "status": r.get("status", ""), "error": ""})
    if args.limit:
        rows = rows[: args.limit]
    write_rows(output_csv("alley"), annotate_rows(rows))


def build_gcard(args):
    db = EXP / "run_tmp/youtube_gcard_db"
    if args.force and db.exists():
        shutil.rmtree(db)
    db.mkdir(parents=True, exist_ok=True)
    stat = db / "youtube.statistic.bin"
    if stat.exists() and (db / "catalog.json").exists() and not args.force:
        print(f"exists={stat}")
        return
    gql = EXP / "run_tmp/youtube_gcard_setup.gql"
    gql.parent.mkdir(parents=True, exist_ok=True)
    gql.write_text(f'call load_ldbc("{EXP / "datasets/youtube_gcard"}", "youtube", {args.k})\n:quit\n', encoding="utf-8")
    run([str(REPO / "target/release/minigu"), "execute", str(gql), "--path", str(db)], cwd=REPO, log=EXP / "result/baselines/gcard/build-logs/youtube.log")


def parse_gcard_log(text: str):
    card_re = re.compile(r",\s*([^,\s]+),\s*cardinality:\s*([0-9.eE+-]+)")
    time_re = re.compile(r"Time:\s*([0-9.]+)s")
    prof_re = re.compile(r"total: (?P<total>\d+).*sample_time: (?P<sample>[0-9.]+), estimate_time: (?P<estimate>[0-9.]+), build_time: (?P<build>[0-9.]+).*cardinality: (?P<cardinality>[0-9.eE+-]+)")
    estimates = [float(m.group(2)) for m in card_re.finditer(text)]
    profiles = []
    cli_times = []
    current = None
    for line in text.splitlines():
        if line.startswith("[build-prof]"):
            current = {k: float(v) for k, v in re.findall(r"([a-zA-Z_+]+): ([0-9.]+)s", line)}
        elif line.startswith("total:"):
            m = prof_re.search(line)
            if m:
                if current is None:
                    current = {}
                current.update({"sample_time_s": float(m.group("sample")), "estimate_time_s": float(m.group("estimate")), "build_time_s": float(m.group("build"))})
                profiles.append(current)
                current = None
        elif line.startswith("Time:"):
            m = time_re.search(line)
            if m:
                cli_times.append(float(m.group(1)))
    return estimates, profiles, cli_times


def estimate_gcard(args):
    build_gcard(args)
    patterns = sorted((EXP / "patterns/gcard/youtube").glob("*.json"), key=natural_key)
    if args.limit:
        patterns = patterns[: args.limit]
    db = EXP / "run_tmp/youtube_gcard_db"
    gql = EXP / "run_tmp/youtube_gcard_query.gql"
    log = EXP / "result/baselines/gcard/estimate/youtube.log"
    lines = ["session set graph youtube", 'call load_catalog("youtube")', f"call set_gcard_star_config({args.star_len}, {args.star_degree})"]
    for p in patterns:
        lines.append(f':time call gcard_query("{p}", {args.k}, {args.sample_size}, 0, false, {args.tree_num})')
    lines.append(":quit")
    gql.write_text("\n".join(lines) + "\n", encoding="utf-8")
    run([str(REPO / "target/release/minigu"), "execute", str(gql), "--path", str(db)], cwd=REPO, log=log)
    estimates, profiles, cli_times = parse_gcard_log(log.read_text(encoding="utf-8", errors="replace"))
    rows = []
    for i, p in enumerate(patterns):
        if i < len(estimates):
            prof = profiles[i] if i < len(profiles) else {}
            latency = prof.get("build_time_s", 0.0) + prof.get("estimate_time_s", 0.0) + prof.get("sample_time_s", 0.0) if prof else (cli_times[i] if i < len(cli_times) else "")
            rows.append({"query": p.stem, "estimate": f"{estimates[i]:.17g}", "latency_s": f"{latency:.9g}" if isinstance(latency, float) else latency, "status": "ok", "error": ""})
        else:
            rows.append({"query": p.stem, "estimate": "", "latency_s": "", "status": "missing_parse", "error": "estimate not found in log"})
    write_rows(output_csv("gcard"), annotate_rows(rows))


BUILDERS = {
    "gcard": build_gcard,
    "safebound": build_safebound,
    "bayescard": build_bayescard,
    "factorjoin": build_factorjoin,
    "color": build_color,
    "pathce": build_pathce,
    "gcare": build_gcare,
    "alley": build_alley,
}
ESTIMATORS = {
    "gcard": estimate_gcard,
    "safebound": estimate_safebound,
    "bayescard": estimate_bayescard,
    "factorjoin": estimate_factorjoin,
    "color": estimate_color,
    "pathce": estimate_pathce,
    "gcare": estimate_gcare,
    "alley": estimate_alley,
}


def main():
    ap = argparse.ArgumentParser(description="Build/estimate the 8 YouTube baselines.")
    ap.add_argument("action", choices=["build", "estimate"])
    ap.add_argument("baseline", choices=BASELINES + ["all"])
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--limit", type=int)
    ap.add_argument("--threads", type=int, default=8)
    ap.add_argument("--k", type=int, default=2)
    ap.add_argument("--d", type=int, default=5)
    ap.add_argument("--m", type=int, default=200)
    ap.add_argument("--factorjoin-bins", type=int, default=100)
    ap.add_argument("--factorjoin-bucket-method", default="greedy")
    ap.add_argument("--timeout", type=float, default=300.0)
    ap.add_argument("--ratio", default="0.03")
    ap.add_argument("--repeat", default="30")
    ap.add_argument("--seed", default="0")
    ap.add_argument("--sample-size", type=int, default=500)
    ap.add_argument("--tree-num", type=int, default=10)
    ap.add_argument("--star-len", type=int, default=1)
    ap.add_argument("--star-degree", type=int, default=5)
    ap.add_argument("--alley-ratio", default="0.001")
    args = ap.parse_args()

    targets = BASELINES if args.baseline == "all" else [args.baseline]
    funcs = BUILDERS if args.action == "build" else ESTIMATORS
    for name in targets:
        print(f"== {args.action} {name} ==", flush=True)
        funcs[name](args)


if __name__ == "__main__":
    main()
