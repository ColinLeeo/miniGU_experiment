#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any


OP_MAP = {
    "=": "eq",
    "==": "eq",
    "!=": "ne",
    "<>": "ne",
    ">": "gt",
    ">=": "ge",
    "<": "lt",
    "<=": "le",
}


def split_comma(s: str) -> list[str]:
    parts: list[str] = []
    cur: list[str] = []
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


def split_conditions(where_clause: str) -> list[str]:
    return [x.strip() for x in re.split(r"\s+and\s+", where_clause, flags=re.IGNORECASE) if x.strip()]


def parse_value(raw: str) -> Any:
    value = raw.strip().rstrip(";")
    if value.startswith("'") and value.endswith("'") and len(value) >= 2:
        return value[1:-1]
    try:
        return int(value)
    except ValueError:
        pass
    try:
        return float(value)
    except ValueError:
        return value


def scalar_value(value: Any) -> dict[str, Any]:
    if isinstance(value, bool):
        return {"Boolean": value}
    if isinstance(value, int):
        return {"Int64": value}
    if isinstance(value, float):
        return {"Float64": value}
    return {"String": str(value)}


def normalize_label(value: str) -> str:
    return value.strip().lower()


def parse_sql(sql: str) -> tuple[dict[str, str], list[tuple[str, str, str, str]], list[tuple[str, str, str, Any]]]:
    sql = sql.strip().rstrip(";")
    m = re.match(
        r"^select\s+count\s*\(\s*\*\s*\)\s+from\s+(?P<from>.+?)\s+where\s+(?P<where>.+)$",
        sql,
        flags=re.IGNORECASE | re.DOTALL,
    )
    if not m:
        raise ValueError(f"Unsupported job-light SQL: {sql}")

    aliases: dict[str, str] = {}
    for term in split_comma(m.group("from")):
        toks = term.split()
        if len(toks) == 2:
            table, alias = toks
        elif len(toks) == 3 and toks[1].lower() == "as":
            table, alias = toks[0], toks[2]
        else:
            raise ValueError(f"Unsupported FROM term: {term}")
        aliases[alias] = table

    joins: list[tuple[str, str, str, str]] = []
    predicates: list[tuple[str, str, str, Any]] = []
    for cond in split_conditions(m.group("where")):
        join = re.match(
            r"^([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*=\s*([A-Za-z_]\w*)\.([A-Za-z_]\w*)$",
            cond,
        )
        if join:
            joins.append(join.groups())
            continue

        pred = re.match(r"^([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*(=|==|!=|<>|<=|>=|<|>)\s*(.+)$", cond)
        if not pred:
            raise ValueError(f"Unsupported WHERE condition: {cond}")
        alias, col, op, rhs = pred.groups()
        predicates.append((alias, col, op, parse_value(rhs)))

    return aliases, joins, predicates


def to_gcard_pattern(sql: str) -> dict[str, Any]:
    aliases, joins, predicates = parse_sql(sql)

    alias_order = list(aliases.keys())
    alias_to_id = {alias: i + 1 for i, alias in enumerate(alias_order)}
    vertices = [
        {"id": alias_to_id[alias], "label": normalize_label(table)}
        for alias, table in aliases.items()
    ]

    edges = []
    for i, (l_alias, l_col, r_alias, r_col) in enumerate(joins, start=1):
        l_table = normalize_label(aliases[l_alias])
        r_table = normalize_label(aliases[r_alias])
        edge_label = f"{l_table}_{l_col.lower()}_{r_table}_{r_col.lower()}"
        edges.append(
            {
                "id": i,
                "label": edge_label,
                "src": alias_to_id[l_alias],
                "dst": alias_to_id[r_alias],
            }
        )

    gcard_predicates = []
    for i, (alias, col, op, value) in enumerate(predicates, start=1):
        gcard_predicates.append(
            {
                "predicate_id": i,
                "target": "vertex",
                "id": alias_to_id[alias],
                "property": col,
                "op": OP_MAP[op],
                "value": scalar_value(value),
            }
        )

    return {"vertices": vertices, "edges": edges, "predicates": gcard_predicates}


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description="Prepare BayesCard JOB-light workload for miniGU experiments.")
    parser.add_argument("--input", required=True, help="BayesCard Benchmark/IMDB/job-light.sql")
    parser.add_argument("--sql-output-dir", required=True)
    parser.add_argument("--gcard-output-dir", required=True)
    parser.add_argument("--true-card-output-dir", required=True)
    parser.add_argument("--manifest-output", default=None)
    args = parser.parse_args()

    input_path = Path(args.input)
    sql_output_dir = Path(args.sql_output_dir)
    gcard_output_dir = Path(args.gcard_output_dir)
    true_card_output_dir = Path(args.true_card_output_dir)

    manifest_rows = ["query,sql_file,gcard_file,true_card"]
    count = 0
    for count, line in enumerate(input_path.read_text(encoding="utf-8").splitlines(), start=1):
        line = line.strip()
        if not line:
            continue
        if "||" not in line:
            raise ValueError(f"Missing true-card separator || in line {count}: {line}")
        sql, true_card = line.rsplit("||", 1)
        true_card = true_card.strip()
        if not true_card.isdigit():
            raise ValueError(f"Invalid true cardinality in line {count}: {true_card}")

        qname = f"q{count}"
        sql_file = sql_output_dir / f"{qname}.sql"
        gcard_file = gcard_output_dir / f"{qname}.json"
        true_file = true_card_output_dir / f"{qname}.txt"

        write_text(sql_file, sql.strip() + "\n")
        write_text(true_file, true_card + "\n")
        gcard_file.parent.mkdir(parents=True, exist_ok=True)
        gcard_file.write_text(json.dumps(to_gcard_pattern(sql), indent=2) + "\n", encoding="utf-8")
        manifest_rows.append(f"{qname},{sql_file.as_posix()},{gcard_file.as_posix()},{true_card}")

    if args.manifest_output:
        write_text(Path(args.manifest_output), "\n".join(manifest_rows) + "\n")
    print(f"Prepared {count} JOB-light queries")


if __name__ == "__main__":
    main()
