#!/usr/bin/env python3
"""Shared helpers for XDB/WanderJoin workload runners."""

from __future__ import annotations

import argparse
import csv
import math
import os
import re
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PSQL = Path(os.environ.get("XDB_PSQL", "psql"))
DEFAULT_HOST = os.environ.get("XDB_HOST", "/tmp/xdb-run")
DEFAULT_PORT = int(os.environ.get("XDB_PORT", "55432"))
DEFAULT_USER = os.environ.get("XDB_USER", os.environ.get("USER", "postgres"))
DEFAULT_DB = os.environ.get("XDB_DB", "xdb_bench")
DEFAULT_OUT = ROOT / "result" / "xdb_wanderjoin"

BOOLEAN_VALUES = {"true", "false"}
INTEGER_RE = re.compile(r"^[+-]?\d+$")
FLOAT_RE = re.compile(r"^[+-]?(?:\d+\.\d*|\d*\.\d+)(?:[eE][+-]?\d+)?$")
DATE_LITERAL_RE = re.compile(
    r"'(\d{4}-\d{2}-\d{2}(?:\s+\d{2}:\d{2}:\d{2})?)'(?:\s*::\s*timestamp)?",
    re.IGNORECASE,
)
JOIN_INDEX_COLUMNS = {
    "id",
    "src",
    "dst",
    "userid",
    "postid",
    "owneruserid",
    "relatedpostid",
    "excerptpostid",
    "lasteditoruserid",
}


@dataclass(frozen=True)
class DatasetConfig:
    name: str
    schema: str
    data_dir: Path
    truth_csv: Path
    truth_query_col: str
    truth_card_col: str
    query_dir: Path | None = None
    ldbc_truth_paths: bool = False


def qident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def sql_literal(value: str | Path) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def table_name_from_csv(path: Path) -> str:
    return path.stem.lower()


def sample_rows(path: Path, limit: int = 200) -> tuple[list[str], list[list[str]]]:
    with path.open(newline="", encoding="utf-8") as handle:
        reader = csv.reader(handle)
        header = next(reader)
        rows = []
        for row in reader:
            rows.append(row)
            if len(rows) >= limit:
                break
    return header, rows


def infer_type(values: Iterable[str]) -> str:
    seen = [v.strip() for v in values if v.strip() and v.strip().upper() != "NA"]
    if not seen:
        return "BIGINT"
    lowered = {v.lower() for v in seen}
    if lowered <= BOOLEAN_VALUES:
        return "TEXT"
    if all(INTEGER_RE.match(v) for v in seen):
        return "BIGINT"
    if all(INTEGER_RE.match(v) or FLOAT_RE.match(v) for v in seen):
        return "DOUBLE PRECISION"
    return "TEXT"


def run_psql(args: argparse.Namespace, sql: str, *, tuples_only: bool = False) -> str:
    cmd = [
        str(args.psql),
        "-h",
        args.host,
        "-p",
        str(args.port),
        "-U",
        args.user,
        "-d",
        args.db,
        "-v",
        "ON_ERROR_STOP=1",
        "-P",
        "pager=off",
    ]
    if tuples_only:
        cmd.extend(["-X", "-q", "-t", "-A", "-F", "\t"])
    proc = subprocess.run(cmd, input=sql, text=True, capture_output=True)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())
    return proc.stdout


def list_csvs(cfg: DatasetConfig) -> list[Path]:
    return sorted(path for path in cfg.data_dir.glob("*.csv") if path.is_file())


def build_schema_sql(cfg: DatasetConfig) -> str:
    statements = [
        "SET synchronous_commit = off;",
        "SET maintenance_work_mem = '512MB';",
        f"DROP SCHEMA IF EXISTS {qident(cfg.schema)} CASCADE;",
        f"CREATE SCHEMA {qident(cfg.schema)};",
    ]
    for csv_path in list_csvs(cfg):
        table = table_name_from_csv(csv_path)
        header, rows = sample_rows(csv_path)
        columns: list[tuple[str, str]] = []
        for idx, col in enumerate(header):
            values = [row[idx] for row in rows if idx < len(row)]
            columns.append((col.lower(), infer_type(values)))

        fq_table = f"{qident(cfg.schema)}.{qident(table)}"
        defs = [f"{qident('__rowid')} BIGSERIAL"]
        defs.extend(f"{qident(col)} {typ}" for col, typ in columns)
        statements.append(f"CREATE UNLOGGED TABLE {fq_table} ({', '.join(defs)});")

        copy_cols = ", ".join(qident(col) for col, _ in columns)
        statements.append(
            f"COPY {fq_table} ({copy_cols}) FROM {sql_literal(csv_path)} "
            "WITH (FORMAT csv, HEADER true);"
        )
        statements.append(f"ALTER TABLE {fq_table} ADD PRIMARY KEY ({qident('__rowid')});")

        for col, _typ in columns:
            if col not in JOIN_INDEX_COLUMNS:
                continue
            index_name = re.sub(r"[^A-Za-z0-9_]", "_", f"{cfg.schema}_{table}_{col}_idx")[:60]
            statements.append(f"CREATE INDEX {qident(index_name)} ON {fq_table} ({qident(col)});")
    statements.append("ANALYZE;")
    return "\n".join(statements) + "\n"


def normalize_sql(sql: str) -> str:
    sql = re.sub(r"--.*", "", sql)
    sql = re.sub(r"\s+", " ", sql).strip()
    return sql[:-1].strip() if sql.endswith(";") else sql


def lower_quoted_identifiers(sql: str) -> str:
    return re.sub(r'"([A-Za-z_][A-Za-z0-9_]*)"', lambda m: qident(m.group(1).lower()), sql)



def split_conjuncts(expr: str) -> list[str]:
    return [part.strip().strip("() ") for part in re.split(r"\s+AND\s+", expr, flags=re.IGNORECASE) if part.strip().strip("() ")]

def parse_ldbc_relation(term: str) -> tuple[str, str, list[str]]:
    term = term.strip()
    sub = re.match(
        r"^\(\s*SELECT\s+\*\s+FROM\s+([A-Za-z_][A-Za-z0-9_]*)\s+([A-Za-z_][A-Za-z0-9_]*)\s+WHERE\s+(.+)\s*\)\s+([A-Za-z_][A-Za-z0-9_]*)$",
        term,
        re.IGNORECASE,
    )
    if sub:
        table, inner_alias, predicate, outer_alias = sub.groups()
        if inner_alias != outer_alias:
            predicate = re.sub(rf"\b{re.escape(inner_alias)}\.", outer_alias + ".", predicate)
        return table, outer_alias, split_conjuncts(predicate)

    plain = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s+(?:AS\s+)?([A-Za-z_][A-Za-z0-9_]*)$", term, re.IGNORECASE)
    if not plain:
        raise ValueError(f"cannot parse LDBC relation term: {term}")
    return plain.group(1), plain.group(2), []


def flatten_ldbc_sql(sql: str) -> str:
    sql = normalize_sql(sql)
    body = re.sub(r"^SELECT\s+COUNT\s*\(\s*\*\s*\)\s+FROM\s+", "", sql, flags=re.IGNORECASE)
    parts = re.split(r"\s+JOIN\s+", body, flags=re.IGNORECASE)
    tables: list[str] = []
    predicates: list[str] = []

    table, alias, preds = parse_ldbc_relation(parts[0])
    tables.append(f"{table.lower()} {alias}")
    predicates.extend(preds)

    for part in parts[1:]:
        split = re.split(r"\s+ON\s+", part, maxsplit=1, flags=re.IGNORECASE)
        if len(split) != 2:
            raise ValueError(f"cannot split JOIN term: {part}")
        rel_term, join_cond = split
        table, alias, preds = parse_ldbc_relation(rel_term)
        tables.append(f"{table.lower()} {alias}")
        predicates.extend(preds)
        predicates.extend(split_conjuncts(join_cond))

    where = " AND ".join(predicates)
    return f"SELECT COUNT(*) FROM {', '.join(tables)} WHERE {where}"


def timestamp_literal_to_epoch(match: re.Match[str]) -> str:
    text = match.group(1)
    for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d"):
        try:
            return str(int(time.mktime(time.strptime(text, fmt))))
        except ValueError:
            pass
    return match.group(0)


def transform_stats_sql(sql: str) -> str:
    return DATE_LITERAL_RE.sub(timestamp_literal_to_epoch, normalize_sql(sql))



def add_join_self_filters(sql: str) -> str:
    terms: list[str] = []
    seen: set[str] = set()
    for left, right in re.findall(r"\b([A-Za-z_][A-Za-z0-9_]*\.[A-Za-z_][A-Za-z0-9_]*)\s*=\s*([A-Za-z_][A-Za-z0-9_]*\.[A-Za-z_][A-Za-z0-9_]*)\b", sql):
        for term in (left, right):
            key = term.lower()
            if key not in seen:
                seen.add(key)
                terms.append(f"{term} = {term}")
    if not terms:
        return sql
    extra = " AND ".join(terms)
    if re.search(r"\bWHERE\b", sql, flags=re.IGNORECASE):
        return sql + " AND " + extra
    return sql + " WHERE " + extra

def to_online_sql(sql: str, cfg: DatasetConfig, withtime_ms: int, confidence: int) -> str:
    if cfg.name == "ldbc":
        sql = flatten_ldbc_sql(sql)
        sql = re.sub(r"=\s*(true|false)\b", lambda m: "= '" + m.group(1).lower() + "'", sql, flags=re.IGNORECASE)
    elif cfg.name == "stats":
        sql = add_join_self_filters(transform_stats_sql(sql))
    else:
        sql = lower_quoted_identifiers(normalize_sql(sql))

    sql = re.sub(r"^SELECT\s+COUNT\s*\(\s*\*\s*\)", "SELECT ONLINE COUNT(*)", sql, flags=re.IGNORECASE)
    return (
        f"SET search_path TO {qident(cfg.schema)};\n"
        f"{sql} WITHTIME {withtime_ms} CONFIDENCE {confidence} REPORTINTERVAL {withtime_ms};\n"
    )


def dedupe_rows(rows: list[dict[str, str]], query_col: str) -> list[dict[str, str]]:
    seen: set[str] = set()
    out = []
    for row in rows:
        query = row[query_col]
        if query in seen:
            continue
        seen.add(query)
        out.append(row)
    return out


def load_workload(cfg: DatasetConfig) -> list[tuple[str, Path, float]]:
    with cfg.truth_csv.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle))
    if cfg.ldbc_truth_paths:
        return [
            (row[cfg.truth_query_col], Path(row["sql_path"]), float(row[cfg.truth_card_col]))
            for row in rows
        ]
    if cfg.query_dir is None:
        raise ValueError(f"{cfg.name} needs query_dir")
    return [
        (row[cfg.truth_query_col], cfg.query_dir / f"{row[cfg.truth_query_col]}.sql", float(row[cfg.truth_card_col]))
        for row in dedupe_rows(rows, cfg.truth_query_col)
    ]


def parse_online_row(output: str) -> tuple[float, int, int, float, float]:
    rows = [line.strip() for line in output.splitlines() if line.strip()]
    if not rows:
        raise ValueError("empty ONLINE output")
    fields = rows[-1].split("\t")
    if len(fields) < 5:
        raise ValueError(f"unexpected ONLINE output row: {rows[-1]!r}")
    estimate = math.nan if fields[3].lower() == "nan" else float(fields[3])
    rel_ci = math.nan if fields[4].lower() == "nan" else float(fields[4])
    return float(fields[0]), int(fields[1]), int(fields[2]), estimate, rel_ci


def qerror(true_card: float, estimate: float) -> float:
    if math.isnan(estimate):
        return math.nan
    return max(true_card, estimate) / max(min(true_card, estimate), 1.0)


def run_dataset(cfg: DatasetConfig, args: argparse.Namespace) -> Path:
    args.out_dir.mkdir(parents=True, exist_ok=True)
    online_dir = args.out_dir / "online_sql" / cfg.name
    online_dir.mkdir(parents=True, exist_ok=True)

    if not args.skip_prepare:
        ddl = build_schema_sql(cfg)
        (args.out_dir / f"prepare_{cfg.name}.sql").write_text(ddl, encoding="utf-8")
        if args.rebuild:
            print(f"preparing {cfg.name} in schema {cfg.schema}", flush=True)
            run_psql(args, ddl)

    if args.prepare_only:
        return args.out_dir / f"prepare_{cfg.name}.sql"

    result_path = args.out_dir / f"{cfg.name}_with_pred_xdb_wanderjoin_{args.withtime_ms}ms_qerror_latency_long.csv"
    fields = [
        "dataset",
        "query",
        "method",
        "true_cardinality",
        "estimate",
        "qerror",
        "latency_s",
        "wall_latency_s",
        "nsamples",
        "nrejected",
        "rel_ci",
        "withtime_ms",
        "status",
        "source_sql",
        "notes",
    ]
    workload = load_workload(cfg)
    with result_path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for idx, (query, sql_path, true_card) in enumerate(workload, 1):
            online_sql = to_online_sql(sql_path.read_text(encoding="utf-8"), cfg, args.withtime_ms, args.confidence)
            (online_dir / f"{query}.sql").write_text(online_sql, encoding="utf-8")
            started = time.perf_counter()
            status = "ok"
            notes = ""
            estimate = math.nan
            latency_s = math.nan
            nsamples = 0
            nrejected = 0
            rel_ci = math.nan
            try:
                output = run_psql(args, online_sql, tuples_only=True)
                time_ms, nsamples, nrejected, estimate, rel_ci = parse_online_row(output)
                latency_s = time_ms / 1000.0
            except Exception as exc:  # noqa: BLE001
                status = "error"
                notes = str(exc).replace("\n", " ")[:500]
            writer.writerow(
                {
                    "dataset": cfg.name,
                    "query": query,
                    "method": "XDB-WanderJoin",
                    "true_cardinality": true_card,
                    "estimate": estimate,
                    "qerror": qerror(true_card, estimate),
                    "latency_s": latency_s,
                    "wall_latency_s": time.perf_counter() - started,
                    "nsamples": nsamples,
                    "nrejected": nrejected,
                    "rel_ci": rel_ci,
                    "withtime_ms": args.withtime_ms,
                    "status": status,
                    "source_sql": str(sql_path),
                    "notes": notes,
                }
            )
            handle.flush()
            if idx % 10 == 0 or idx == len(workload):
                print(f"{cfg.name}: {idx}/{len(workload)} -> {result_path}", flush=True)
    write_summary(result_path)
    return result_path


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return math.nan
    if len(values) == 1:
        return values[0]
    values = sorted(values)
    pos = (len(values) - 1) * pct / 100.0
    lo = math.floor(pos)
    hi = math.ceil(pos)
    if lo == hi:
        return values[lo]
    return values[lo] * (hi - pos) + values[hi] * (pos - lo)


def write_summary(result_path: Path) -> Path:
    with result_path.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle))
    ok = [row for row in rows if row["status"] == "ok"]
    qerrs = [float(row["qerror"]) for row in ok if row["qerror"] and row["qerror"] != "nan"]
    lats = [float(row["latency_s"]) for row in ok if row["latency_s"] and row["latency_s"] != "nan"]
    walls = [float(row["wall_latency_s"]) for row in ok if row["wall_latency_s"] and row["wall_latency_s"] != "nan"]
    out = result_path.with_name(result_path.stem.replace("_qerror_latency_long", "_summary") + ".csv")
    with out.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(
            handle,
            fieldnames=[
                "dataset",
                "n",
                "ok",
                "errors",
                "qerror_median",
                "qerror_p95",
                "latency_median_s",
                "wall_latency_median_s",
            ],
        )
        writer.writeheader()
        writer.writerow(
            {
                "dataset": rows[0]["dataset"] if rows else "",
                "n": len(rows),
                "ok": len(ok),
                "errors": len(rows) - len(ok),
                "qerror_median": percentile(qerrs, 50),
                "qerror_p95": percentile(qerrs, 95),
                "latency_median_s": percentile(lats, 50),
                "wall_latency_median_s": percentile(walls, 50),
            }
        )
    print(f"summary -> {out}", flush=True)
    return out


def build_arg_parser(description: str) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--psql", type=Path, default=DEFAULT_PSQL)
    parser.add_argument("--host", default=DEFAULT_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--user", default=DEFAULT_USER)
    parser.add_argument("--db", default=DEFAULT_DB)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--withtime-ms", type=int, default=1000)
    parser.add_argument("--confidence", type=int, default=95)
    parser.add_argument("--rebuild", action="store_true", help="drop/recreate/load this dataset schema")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--skip-prepare", action="store_true", help="do not generate or run prepare SQL")
    return parser
