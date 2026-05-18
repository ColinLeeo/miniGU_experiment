#!/usr/bin/env python3
import copy
import csv
import json
import subprocess
from pathlib import Path


WORKSPACE = Path(__file__).resolve().parents[1]
DATA_DIR = WORKSPACE / "datasets" / "ldbc" / "sf1"
GCARD_IN = WORKSPACE / "patterns/gcard" / "ldbc_with_pred"
GCARD_OUT = WORKSPACE / "patterns/gcard" / "ldbc_with_pred_30"
SQL_OUT = WORKSPACE / "patterns/sql" / "ldbc_with_pred_30"
SUMMARY_OUT = WORKSPACE / "patterns/sql" / "ldbc_with_pred_30_summary.csv"


def read_column(table, column, cast=None):
    values = []
    with open(DATA_DIR / f"{table}.csv", newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        for row in reader:
            value = row[column]
            if value == "":
                continue
            values.append(cast(value) if cast else value)
    return values


def quantiles(values, qs):
    values = sorted(values)
    return [values[round((len(values) - 1) * q)] for q in qs]


def top_values(values, limit=8):
    counts = {}
    for value in values:
        counts[value] = counts.get(value, 0) + 1
    return sorted(counts.items(), key=lambda item: (-item[1], item[0]))[:limit]


def val(value):
    if isinstance(value, str):
        return {"String": value}
    return {"Int64": int(value)}


def pred(pid, vertex_id, prop, op, value):
    return {
        "predicate_id": pid,
        "target": "vertex",
        "id": vertex_id,
        "property": prop,
        "op": op,
        "value": val(value),
    }


def p_person(pid, vertex_id, i, stats, shift=0):
    recipes = [
        ("gender", "eq", stats["person_gender"][(i + shift) % len(stats["person_gender"])]),
        ("birthday", "ge", stats["person_birthday"][(i + shift) % len(stats["person_birthday"])]),
        ("creationdate", "ge", stats["person_creationdate"][(i + shift) % len(stats["person_creationdate"])]),
        ("browserused", "eq", stats["person_browserused"][(i + shift) % len(stats["person_browserused"])]),
        ("deletiondate", "eq", 1577664000000),
    ]
    prop, op, value = recipes[(i + shift) % len(recipes)]
    return pred(pid, vertex_id, prop, op, value)


def p_post(pid, vertex_id, i, stats, shift=0):
    recipes = [
        ("length", "ge", stats["post_length"][(i + shift) % len(stats["post_length"])]),
        ("creationdate", "ge", stats["post_creationdate"][(i + shift) % len(stats["post_creationdate"])]),
        ("browserused", "eq", stats["post_browserused"][(i + shift) % len(stats["post_browserused"])]),
        ("deletiondate", "ge", stats["post_deletiondate"][(i + shift) % len(stats["post_deletiondate"])]),
    ]
    prop, op, value = recipes[(i + shift) % len(recipes)]
    return pred(pid, vertex_id, prop, op, value)


def p_comment(pid, vertex_id, i, stats, shift=0):
    recipes = [
        ("length", "ge", stats["comment_length"][(i + shift) % len(stats["comment_length"])]),
        ("creationdate", "ge", stats["comment_creationdate"][(i + shift) % len(stats["comment_creationdate"])]),
        ("browserused", "eq", stats["comment_browserused"][(i + shift) % len(stats["comment_browserused"])]),
        ("deletiondate", "ge", stats["comment_deletiondate"][(i + shift) % len(stats["comment_deletiondate"])]),
    ]
    prop, op, value = recipes[(i + shift) % len(recipes)]
    return pred(pid, vertex_id, prop, op, value)


def assign_predicates(pattern_name, i, stats):
    # IDs are the vertex ids in patterns/gcard/ldbc_with_pred/*.json.
    if pattern_name == "L1_PA":
        choices = [
            [p_person(1, 1, i, stats, 0), p_person(2, 2, i, stats, 1)],
            [p_person(1, 1, i, stats, 2), p_person(2, 3, i, stats, 3)],
            [p_person(1, 1, i, stats, 0), p_person(2, 2, i, stats, 2), p_person(3, 3, i, stats, 4)],
        ]
    elif pattern_name == "L2_PB":
        choices = [
            [p_person(1, 3, i, stats, 0), p_post(2, 5, i, stats, 1), p_comment(3, 6, i, stats, 2)],
            [p_person(1, 3, i, stats, 3), p_post(2, 5, i, stats, 0), p_comment(3, 6, i, stats, 1)],
            [p_person(1, 3, i, stats, 1), p_person(2, 3, i, stats, 4), p_post(3, 5, i, stats, 2), p_comment(4, 6, i, stats, 0)],
        ]
    elif pattern_name == "L3_PB":
        choices = [
            [p_comment(1, 1, i, stats, 0), p_person(2, 2, i, stats, 1), p_person(3, 3, i, stats, 2)],
            [p_comment(1, 1, i, stats, 2), p_post(2, 4, i, stats, 0), p_person(3, 3, i, stats, 3)],
            [p_comment(1, 1, i, stats, 1), p_person(2, 2, i, stats, 0), p_person(3, 3, i, stats, 4), p_post(4, 4, i, stats, 2)],
        ]
    elif pattern_name == "L4_PA":
        choices = [
            [p_comment(1, 1, i, stats, 0), p_person(2, 2, i, stats, 1), p_person(3, 3, i, stats, 2)],
            [p_person(1, 2, i, stats, 0), p_person(2, 3, i, stats, 1), p_post(3, 4, i, stats, 2)],
            [p_comment(1, 1, i, stats, 3), p_post(2, 4, i, stats, 0), p_person(3, 2, i, stats, 4)],
        ]
    elif pattern_name == "L5_PB":
        choices = [
            [p_person(1, 5, i, stats, 0), p_person(2, 6, i, stats, 1), p_person(3, 7, i, stats, 2)],
            [p_person(1, 5, i, stats, 3), p_person(2, 6, i, stats, 4), p_person(3, 7, i, stats, 0)],
            [p_person(1, 5, i, stats, 1), p_person(2, 6, i, stats, 2), p_person(3, 7, i, stats, 4)],
        ]
    elif pattern_name == "L6_PA":
        choices = [
            [p_comment(1, 1, i, stats, 0), p_person(2, 3, i, stats, 1), p_person(3, 4, i, stats, 2)],
            [p_comment(1, 1, i, stats, 2), p_comment(2, 2, i, stats, 3), p_person(3, 3, i, stats, 0)],
            [p_comment(1, 1, i, stats, 1), p_person(2, 3, i, stats, 3), p_person(3, 4, i, stats, 4), p_comment(4, 2, i, stats, 0)],
        ]
    else:
        raise KeyError(pattern_name)
    return choices[i % len(choices)]


def load_stats():
    qs = [0.00, 0.10, 0.25, 0.40, 0.50, 0.60, 0.75, 0.90]
    return {
        "person_gender": [x for x, _ in top_values(read_column("person", "gender"), 2)],
        "person_browserused": [x for x, _ in top_values(read_column("person", "browserused"), 5)],
        "person_birthday": quantiles(read_column("person", "birthday", int), qs),
        "person_creationdate": quantiles(read_column("person", "creationdate", int), qs),
        "post_browserused": [x for x, _ in top_values(read_column("post", "browserused"), 5)],
        "post_length": sorted(set([0, 1, 10, 50, 80, 100, 108, 150, 200])),
        "post_creationdate": quantiles(read_column("post", "creationdate", int), qs),
        "post_deletiondate": quantiles(read_column("post", "deletiondate", int), [0.00, 0.25, 0.50, 0.75]),
        "comment_browserused": [x for x, _ in top_values(read_column("comment", "browserused"), 5)],
        "comment_length": sorted(set([2, 3, 4, 5, 20, 50, 79, 88, 100, 120])),
        "comment_creationdate": quantiles(read_column("comment", "creationdate", int), qs),
        "comment_deletiondate": quantiles(read_column("comment", "deletiondate", int), [0.00, 0.25, 0.50, 0.75]),
    }


def write_summary(stats, generated):
    with open(SUMMARY_OUT, "w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(["section", "key", "value"])
        for key in sorted(stats):
            writer.writerow(["distribution", key, stats[key]])
        for pattern_name, count in sorted(generated.items()):
            writer.writerow(["generated", pattern_name, count])


def main():
    stats = load_stats()
    GCARD_OUT.mkdir(parents=True, exist_ok=True)
    SQL_OUT.mkdir(parents=True, exist_ok=True)

    for path in GCARD_OUT.glob("*.json"):
        path.unlink()
    for path in SQL_OUT.glob("*.sql"):
        path.unlink()

    generated = {}
    for template_path in sorted(GCARD_IN.glob("*.json")):
        pattern_name = template_path.stem
        template = json.loads(template_path.read_text(encoding="utf-8"))
        generated[pattern_name] = 0
        for i in range(30):
            query = copy.deepcopy(template)
            query["predicates"] = assign_predicates(pattern_name, i, stats)
            out_path = GCARD_OUT / f"{pattern_name}_{i + 1:02d}.json"
            out_path.write_text(json.dumps(query, indent=2) + "\n", encoding="utf-8")
            generated[pattern_name] += 1

    subprocess.run(
        [
            "python3",
            str(WORKSPACE / "tools" / "minigu2sql.py"),
            "-d",
            str(GCARD_OUT),
            "-o",
            str(SQL_OUT),
        ],
        check=True,
    )
    write_summary(stats, generated)
    print(f"generated gcard patterns: {GCARD_OUT}")
    print(f"generated sql patterns: {SQL_OUT}")
    print(f"summary: {SUMMARY_OUT}")
    print(f"total: {sum(generated.values())}")


if __name__ == "__main__":
    main()
