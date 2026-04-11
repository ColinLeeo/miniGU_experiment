#!/usr/bin/env python
import sys
import json
import argparse
import networkx as nx
import matplotlib.pyplot as plt

colors = [
    "#d33fc2",
    "#8bed2f",
    "#4e90bf",
    "#5402d1",
    "#e8c435",
    "#fff3cc",
    "#16cafc",
    "#3cd8a7",
    "#d3d3d3",
    "#c42d1f",
    "#057f11",
    "#520de8",
    "#065de0",
]

parser = argparse.ArgumentParser(prog=sys.argv[0],
                                 description="Visualize the input pattern")
parser.add_argument("-p",
                    "--pattern",
                    help="Specify the pattern path",
                    required=True)
parser.add_argument("-s",
                    "--schema",
                    help="Specify the schema path",
                    required=True)
parser.add_argument("-o",
                    "--output",
                    help="Specify the output path (.pdf or .png)",
                    required=True)
parser.add_argument("-w", 
                    "--with-label-id",
                    action="store_true",
                    help="Specify whether to show label id for vertices and edges")
args = parser.parse_args()

with open(args.pattern) as f:
    pattern = json.load(f)

with open(args.schema) as f:
    schema = json.load(f)

color_idx = 0
vlabel_color_map = {}
vlabel_map = {}
vlabel_vertices_map = {}
for vertex_label, vertex_label_id in sorted(schema["vertex_labels"].items(),
                                            key=lambda v: v[1]):
    vlabel_map[vertex_label_id] = vertex_label
    vlabel_color_map[vertex_label_id] = colors[color_idx]
    vlabel_vertices_map[vertex_label_id] = set()
    color_idx += 1

elabel_id_to_name = {}
for edge_label, edge_label_id in schema["edge_labels"].items():
    elabel_id_to_name[edge_label_id] = edge_label

g = nx.DiGraph()
for vertex in pattern["vertices"]:
    tag_id = vertex["tag_id"]
    label_id = vertex["label_id"]
    vlabel_vertices_map[label_id].add(tag_id)
    vlabel = vlabel_map[label_id]
    g.add_node(tag_id)

elabel_map = {}
for edge in pattern["edges"]:
    src_tag_id = edge["src"]
    dst_tag_id = edge["dst"]
    label_id = edge["label_id"]
    g.add_edge(src_tag_id, dst_tag_id)
    edge_label = elabel_id_to_name[label_id].split("_")[1]
    if args.with_label_id:
        label = f"{edge_label} ({label_id})"
    else:
        label = f"{edge_label}"
    elabel_map[(src_tag_id, dst_tag_id)] = label

# pos = nx.layout.spring_layout(g)
pos = nx.layout.planar_layout(g)
fig = plt.figure()
for vertex_label_id, vertices in vlabel_vertices_map.items():
    if len(vertices) == 0:
        continue
    vertex_label = vlabel_map[vertex_label_id]
    if args.with_label_id:
        label = f"{vertex_label} (label_id: {vertex_label_id})"
    else:
        label = f"{vertex_label}"
    nx.draw_networkx_nodes(g,
                           pos=pos,
                           node_color=vlabel_color_map[vertex_label_id],
                           nodelist=vertices,
                           label=label)
    nx.draw_networkx_labels(g, pos=pos)
nx.draw_networkx_edge_labels(g, pos=pos, edge_labels=elabel_map)
nx.draw_networkx_edges(g, pos=pos)
fig.legend()
fig.savefig(args.output)
