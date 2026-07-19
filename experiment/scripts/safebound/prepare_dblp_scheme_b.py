#!/usr/bin/env python3
import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PATTERN_JSON_DIR = ROOT / "run_tmp/dblp_gcard_partitioned_patterns"
DEFAULT_FILTER_SQL_DIR = ROOT / "patterns/sql/dblp_partitioned"
DEFAULT_OUTPUT_DIR = ROOT / "patterns/sql/dblp_partitioned_scheme_b"


def qident(name):
    return str(name)


def pattern_to_sql(pattern):
    vertices = sorted(pattern["vertices"], key=lambda x: x["id"])
    edges = sorted(pattern["edges"], key=lambda x: x["id"])
    from_terms = []
    where = []

    for edge in edges:
        alias = f"e{edge['id']}"
        from_terms.append(f"{qident(edge['label'])} {alias}")
        where.append(f"{alias}.src = v{edge['src']}.id")
        where.append(f"{alias}.dst = v{edge['dst']}.id")

    for vertex in vertices:
        from_terms.append(f"{qident(vertex['label'])} v{vertex['id']}")

    if not from_terms:
        raise ValueError("empty pattern")
    sql = "SELECT COUNT(*) FROM " + ", ".join(from_terms)
    if where:
        sql += " WHERE " + " AND ".join(where)
    return sql + "\n"


def main():
    ap = argparse.ArgumentParser(description="Generate DBLP SafeBound scheme-B SQL with partitioned edge tables plus vertex FK/PK joins.")
    ap.add_argument("--pattern-json-dir", type=Path, default=DEFAULT_PATTERN_JSON_DIR)
    ap.add_argument("--filter-sql-dir", type=Path, default=DEFAULT_FILTER_SQL_DIR, help="Only generate names present in this SQL dir; pass an empty/nonexistent dir to use all JSON patterns.")
    ap.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    ap.add_argument("--force", action="store_true")
    args = ap.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    if args.filter_sql_dir.exists():
        wanted = {p.stem for p in args.filter_sql_dir.glob("*.sql")}
    else:
        wanted = None

    count = 0
    for path in sorted(args.pattern_json_dir.glob("*.json")):
        if wanted is not None and path.stem not in wanted:
            continue
        out = args.output_dir / f"{path.stem}.sql"
        if out.exists() and not args.force:
            count += 1
            continue
        pattern = json.loads(path.read_text(encoding="utf-8"))
        out.write_text(pattern_to_sql(pattern), encoding="utf-8")
        count += 1

    print(f"output={args.output_dir}")
    print(f"queries={count}")


if __name__ == "__main__":
    main()
