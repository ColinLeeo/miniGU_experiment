#!/usr/bin/env python3
"""
Convert pathce pattern JSON files to miniGU gcard_query pattern JSON.

Supports:
- no-predicate patterns
- patterns with predicates (when present in input)

Usage examples:
  # single file
  python3 pattern2minigu.py \
    --schema experiment/schemas/imdb/imdb_pathce_schema.json \
    --input experiment/patterns/pathce/imdb/q1.json \
    --output experiment/patterns/gcard/imdb/q1.json

  # recursive directory conversion (keep predicates if present)
  python3 pattern2minigu.py \
    --schema experiment/schemas/ldbc/ldbc_pathce_schema.json \
    --input experiment/patterns/pathce/lsqb \
    --output experiment/patterns/gcard/lsqb

  # dataset shortcut + drop predicates
  python3 pattern2minigu.py \
    --dataset aids_merged \
    --input experiment/patterns/pathce/aids_merged \
    --output experiment/patterns/gcard/aids_merged_nopred \
    --predicates drop
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any, Dict, List, Tuple


DATASET_SCHEMAS = {
    "ldbc": "schemas/ldbc/ldbc_pathce_schema.json",
    "aids_merged": "schemas/aids_merged/aids_merged_pathce_schema.json",
    "imdb": "schemas/imdb/imdb_pathce_schema.json",
}

OP_MAP = {
    "eq": "eq",
    "=": "eq",
    "ne": "ne",
    "!=": "ne",
    "<>": "ne",
    "gt": "gt",
    ">": "gt",
    "ge": "ge",
    ">=": "ge",
    "lt": "lt",
    "<": "lt",
    "le": "le",
    "<=": "le",
}


def load_json(path: Path) -> Dict[str, Any]:
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def build_label_maps(schema: Dict[str, Any]) -> Tuple[Dict[int, str], Dict[int, str]]:
    vertex_map = {int(v): k.lower() for k, v in schema["vertex_labels"].items()}
    edge_map = {int(v): k.lower() for k, v in schema["edge_labels"].items()}
    return vertex_map, edge_map


def normalize_scalar_value(value: Any) -> Dict[str, Any]:
    # Already in miniGU ScalarValue shape
    if isinstance(value, dict) and len(value) == 1:
        t = next(iter(value.keys()))
        if t in {"Int64", "Float64", "String", "Boolean"}:
            return value

    if isinstance(value, bool):
        return {"Boolean": value}
    if isinstance(value, int):
        return {"Int64": value}
    if isinstance(value, float):
        return {"Float64": value}
    return {"String": str(value)}


def normalize_op(op: Any) -> str:
    key = str(op).strip().lower()
    if key not in OP_MAP:
        raise ValueError(f"Unsupported predicate op: {op}")
    return OP_MAP[key]


def remap_predicate_id(raw_id: Any, valid_ids: set[int]) -> int:
    # Many pathce-like sources use 0-based tag IDs; miniGU pattern uses 1-based IDs.
    if not isinstance(raw_id, int):
        raise ValueError(f"Predicate target id must be int, got: {raw_id}")
    if raw_id in valid_ids:
        return raw_id
    if (raw_id + 1) in valid_ids:
        return raw_id + 1
    raise ValueError(f"Predicate references unknown target id: {raw_id}")


def convert_predicates(
    raw_predicates: List[Dict[str, Any]],
    vertex_ids: set[int],
    edge_ids: set[int],
) -> List[Dict[str, Any]]:
    converted: List[Dict[str, Any]] = []
    next_pid = 1
    for p in raw_predicates:
        target = str(p.get("target", "vertex")).lower()
        if target not in {"vertex", "edge"}:
            raise ValueError(f"Invalid predicate target: {target}")

        raw_target_id = p.get("id", p.get("target_id", p.get("tag_id")))
        if raw_target_id is None:
            raise ValueError("Predicate missing target id (id/target_id/tag_id)")

        valid_ids = vertex_ids if target == "vertex" else edge_ids
        target_id = remap_predicate_id(raw_target_id, valid_ids)

        prop = p.get("property", p.get("prop", p.get("key")))
        if not prop:
            raise ValueError("Predicate missing property")

        op = normalize_op(p.get("op", p.get("operator", "eq")))
        value = normalize_scalar_value(p.get("value"))

        predicate_id = p.get("predicate_id")
        if predicate_id is None:
            predicate_id = next_pid
            next_pid += 1

        converted.append(
            {
                "predicate_id": int(predicate_id),
                "target": target,
                "id": target_id,
                "property": str(prop),
                "op": op,
                "value": value,
            }
        )
    return converted


def convert_pattern(
    pathce_pattern: Dict[str, Any],
    vertex_map: Dict[int, str],
    edge_map: Dict[int, str],
    predicates_mode: str,
) -> Dict[str, Any]:
    vertices: List[Dict[str, Any]] = []
    for v in pathce_pattern["vertices"]:
        tag_id = int(v["tag_id"])
        label_id = int(v["label_id"])
        if label_id not in vertex_map:
            raise ValueError(f"Unknown vertex label_id: {label_id}")
        vertices.append({"id": tag_id + 1, "label": vertex_map[label_id]})

    edges: List[Dict[str, Any]] = []
    for e in pathce_pattern["edges"]:
        tag_id = int(e["tag_id"])
        label_id = int(e["label_id"])
        if label_id not in edge_map:
            raise ValueError(f"Unknown edge label_id: {label_id}")
        edges.append(
            {
                "id": tag_id + 1,
                "label": edge_map[label_id],
                "src": int(e["src"]) + 1,
                "dst": int(e["dst"]) + 1,
            }
        )

    raw_preds = pathce_pattern.get("predicates", [])
    if predicates_mode == "drop":
        preds: List[Dict[str, Any]] = []
    elif predicates_mode == "only":
        preds = convert_predicates(
            raw_preds,
            vertex_ids={v["id"] for v in vertices},
            edge_ids={e["id"] for e in edges},
        )
        if not preds:
            raise ValueError("predicates_mode=only but input has no predicates")
    else:
        preds = convert_predicates(
            raw_preds,
            vertex_ids={v["id"] for v in vertices},
            edge_ids={e["id"] for e in edges},
        )

    return {"vertices": vertices, "edges": edges, "predicates": preds}


def convert_one_file(
    schema: Dict[str, Any],
    input_path: Path,
    output_path: Path,
    predicates_mode: str,
) -> None:
    vertex_map, edge_map = build_label_maps(schema)
    src = load_json(input_path)
    out = convert_pattern(src, vertex_map, edge_map, predicates_mode)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8") as f:
        json.dump(out, f, indent=2)


def resolve_schema_path(args: argparse.Namespace, experiment_dir: Path) -> Path:
    if args.schema:
        return Path(args.schema).resolve()
    if args.dataset:
        rel = DATASET_SCHEMAS[args.dataset]
        return (experiment_dir / rel).resolve()
    raise ValueError("Either --schema or --dataset must be provided")


def main() -> None:
    parser = argparse.ArgumentParser(description="Convert pathce patterns to miniGU patterns")
    parser.add_argument("--schema", help="Path to pathce schema JSON")
    parser.add_argument(
        "--dataset",
        choices=sorted(DATASET_SCHEMAS.keys()),
        help="Use built-in schema path for dataset",
    )
    parser.add_argument("--input", required=True, help="Input pattern file or directory")
    parser.add_argument("--output", required=True, help="Output pattern file or directory")
    parser.add_argument(
        "--predicates",
        choices=["keep", "drop", "only"],
        default="keep",
        help="Predicate handling: keep/drop/only (default: keep)",
    )
    args = parser.parse_args()

    tool_dir = Path(__file__).resolve().parent
    experiment_dir = tool_dir.parent
    schema_path = resolve_schema_path(args, experiment_dir)
    schema = load_json(schema_path)

    input_path = Path(args.input).resolve()
    output_path = Path(args.output).resolve()

    if input_path.is_file():
        if output_path.exists() and output_path.is_dir():
            output_file = output_path / input_path.name
        else:
            output_file = output_path
        convert_one_file(schema, input_path, output_file, args.predicates)
        print(f"{input_path} -> {output_file}")
        return

    if not input_path.is_dir():
        raise FileNotFoundError(f"Input path not found: {input_path}")

    files = sorted(input_path.rglob("*.json"))
    if not files:
        print(f"No JSON files found under {input_path}")
        return

    converted = 0
    for src in files:
        rel = src.relative_to(input_path)
        dst = output_path / rel
        try:
            convert_one_file(schema, src, dst, args.predicates)
            converted += 1
        except Exception as e:
            print(f"SKIP {src}: {e}")

    print(f"Converted {converted}/{len(files)} files into {output_path}")


if __name__ == "__main__":
    main()
