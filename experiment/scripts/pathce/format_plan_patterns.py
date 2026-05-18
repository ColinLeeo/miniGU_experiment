#!/usr/bin/env python3
import argparse
import json
import re
from pathlib import Path


HEADER_RE = re.compile(r"^===== (?P<name>.+) =====$")
PLAN_EDGE_RE = re.compile(r"^\[pathce-plan\]\s+ce(?P<tag>\d+)\s+(?P<kind>\w+)\s+label=(?P<label>\d+).*")
COVERS_RE = re.compile(r"^\[pathce-plan\]\s+covers=(?P<covers>.+)$")


def load_schema(path):
    with open(path) as f:
        schema = json.load(f)
    vertex_names = {v: k for k, v in schema["vertex_labels"].items()}
    edge_names = {v: k for k, v in schema["edge_labels"].items()}
    return vertex_names, edge_names


def load_pattern(path):
    with open(path) as f:
        pattern = json.load(f)
    vertices = {v["tag_id"]: v["label_id"] for v in pattern["vertices"]}
    edges = {e["tag_id"]: e for e in pattern["edges"]}
    return vertices, edges


def relation_tokens(edge_name):
    parts = edge_name.split("_")
    if len(parts) >= 3:
        return parts[1:-1]
    return parts


def edge_name_from_orientation(edge, current, nxt, vertex_names, edge_names):
    src_label = vertex_names[edge["src_label"]]
    dst_label = vertex_names[edge["dst_label"]]
    edge_name = edge_names[edge["label_id"]]
    rel = relation_tokens(edge_name)
    if edge["src"] == current and edge["dst"] == nxt:
        return [src_label, *rel, dst_label], True
    if edge["dst"] == current and edge["src"] == nxt:
        return [dst_label, *rel, src_label], False
    raise ValueError("edge is not incident to the requested vertices")


def trail_orders(edge_ids, edges):
    chosen = [edges[eid] for eid in edge_ids]
    adj = {}
    for edge in chosen:
        adj.setdefault(edge["src"], []).append((edge["dst"], edge["tag_id"]))
        adj.setdefault(edge["dst"], []).append((edge["src"], edge["tag_id"]))

    starts = [v for v, inc in adj.items() if len(inc) == 1] or sorted(adj)
    orders = []

    def dfs(vertex, used, order):
        if len(used) == len(chosen):
            orders.append(order[:])
            return
        for nxt, eid in adj.get(vertex, []):
            if eid in used:
                continue
            used.add(eid)
            order.append((eid, vertex, nxt))
            dfs(nxt, used, order)
            order.pop()
            used.remove(eid)

    for start in starts:
        dfs(start, set(), [])
    return orders


def format_connected_pattern(edge_ids, vertices, edges, vertex_names, edge_names):
    if len(edge_ids) == 1:
        edge = edges[edge_ids[0]]
        return edge_names[edge["label_id"]]

    enriched_edges = {}
    for eid in edge_ids:
        edge = dict(edges[eid])
        edge["src_label"] = vertices[edge["src"]]
        edge["dst_label"] = vertices[edge["dst"]]
        enriched_edges[eid] = edge

    candidates = []
    for order in trail_orders(edge_ids, enriched_edges):
        if len(order) != len(edge_ids):
            continue
        tokens = []
        direction_score = 0
        for i, (eid, current, nxt) in enumerate(order):
            edge_tokens, forward = edge_name_from_orientation(
                enriched_edges[eid], current, nxt, vertex_names, edge_names
            )
            edge = enriched_edges[eid]
            # Reversing an edge whose endpoint labels are the same, such as
            # Person_knows_Person, does not make the printed pattern less natural.
            if forward or edge["src_label"] == edge["dst_label"]:
                direction_score += 1
            if i == 0:
                tokens.extend(edge_tokens)
            else:
                tokens.extend(edge_tokens[1:])
        candidates.append((-direction_score, "_".join(tokens)))

    plus_pattern = "+".join(edge_names[edges[eid]["label_id"]] for eid in edge_ids)
    if not candidates:
        return plus_pattern
    candidates.sort()
    best_score, best_pattern = candidates[0]
    if -best_score != len(edge_ids):
        return plus_pattern
    return best_pattern


def format_cover(cover, vertices, edges, vertex_names, edge_names):
    cover = cover.strip()
    if cover == "<unresolved>":
        return cover
    alternatives = []
    for alt in cover.split("|"):
        edge_ids = [int(tok.strip()[1:]) for tok in alt.strip().split(",") if tok.strip()]
        alternatives.append(
            "[" + format_connected_pattern(edge_ids, vertices, edges, vertex_names, edge_names) + "]"
        )
    return " | ".join(alternatives)


def convert_plan(plan_log, pattern_dir, schema_path, output):
    vertex_names, edge_names = load_schema(schema_path)
    current_pattern = None
    current_edge = None
    vertices = {}
    edges = {}

    with open(plan_log) as src, open(output, "w") as dst:
        for line in src:
            line = line.rstrip("\n")
            header = HEADER_RE.match(line)
            if header:
                current_pattern = header.group("name")
                vertices, edges = load_pattern(pattern_dir / current_pattern)
                current_edge = None
                print(line, file=dst)
                continue

            edge_match = PLAN_EDGE_RE.match(line)
            if edge_match:
                current_edge = edge_match.groupdict()
                print(
                    "[pathce-plan]   ce{tag} {kind} label={label}".format(**current_edge),
                    file=dst,
                )
                continue

            covers_match = COVERS_RE.match(line)
            if covers_match and current_pattern is not None:
                cover = format_cover(covers_match.group("covers"), vertices, edges, vertex_names, edge_names)
                print(f"[pathce-plan]     covers={cover}", file=dst)
                continue

            if line.startswith("[pathce-plan] vertices:") or line.startswith("[pathce-plan]   v"):
                continue
            if " catalog=" in line:
                continue
            print(line, file=dst)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan-log", required=True, type=Path)
    parser.add_argument("--pattern-dir", required=True, type=Path)
    parser.add_argument("--schema", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    convert_plan(args.plan_log, args.pattern_dir, args.schema, args.output)


if __name__ == "__main__":
    main()
