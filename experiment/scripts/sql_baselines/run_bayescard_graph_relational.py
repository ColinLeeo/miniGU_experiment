#!/usr/bin/env python3
"""Run BayesCard on graph workloads as explicit relational joins.

This replaces the previous independence wrappers. The
workload is a single SQL representation with the same exact truth used by the
other SQL baselines, and repeated graph vertices/edges are represented as SQL
aliases over the physical vertex/edge tables.
"""

from __future__ import print_function

import argparse
import copy
import csv
import itertools
import json
import math
import os
import pickle
import re
import shutil
import sys
import time
import traceback
from pathlib import Path

import numpy as np
import pandas as pd


SCRIPT_PATH = Path(__file__).resolve()
INVOCATION_CWD = Path.cwd()
EXPERIMENT_DIR = SCRIPT_PATH.parents[2]
SAFEBOUND_DIR = EXPERIMENT_DIR / "baseline" / "SafeBound"
BAYESCARD_DIR = SAFEBOUND_DIR / "bayescard"
if str(BAYESCARD_DIR) not in sys.path:
    sys.path.insert(0, str(BAYESCARD_DIR))
os.chdir(str(BAYESCARD_DIR))

import pgympy
sys.modules["Pgmpy"] = pgympy

from DataPrepare.join_data_preparation import JoinDataPreparator
from DataPrepare.prepare_single_tables import prepare_all_tables
import DataPrepare.prepare_single_tables as prepare_single_tables
from DataPrepare.query_prepare_BayesCard import factor_refine, generate_factors, prepare_single_query
from DeepDBUtils.ensemble_compilation.graph_representation import Query, SchemaGraph, Table
from DeepDBUtils.ensemble_compilation.probabilistic_query import Expectation, IndicatorExpectation
from Models.BN_ensemble_model import BN_ensemble
from Models.Bayescard_BN import Bayescard_BN, build_meta_info

DEFAULTS = {
    "aids_merged": {
        "data_dir": EXPERIMENT_DIR / "datasets" / "aids_merged",
        "pattern_dir": EXPERIMENT_DIR / "patterns" / "gcard" / "aids_merged",
        "truth_file": EXPERIMENT_DIR / "result" / "truecard" / "aids_merged_current" / "true_card.csv",
        "hdf_dir": EXPERIMENT_DIR / "catalogs" / "aids_merged" / "bayescard_graph_relational_hdf",
        "model_dir": EXPERIMENT_DIR / "catalogs" / "aids_merged" / "bayescard_graph_relational_models",
        "out_dir": EXPERIMENT_DIR / "result" / "sql_baselines" / "bayescard_graph_relational" / "aids_merged",
    },
    "dblp": {
        "data_dir": EXPERIMENT_DIR / "datasets" / "dblp",
        "pattern_dir": EXPERIMENT_DIR / "patterns" / "gcard" / "dblp",
        "truth_file": EXPERIMENT_DIR / "result" / "truecard" / "dblp" / "true_card.csv",
        "hdf_dir": EXPERIMENT_DIR / "catalogs" / "dblp" / "bayescard_graph_relational_hdf",
        "model_dir": EXPERIMENT_DIR / "catalogs" / "dblp" / "bayescard_graph_relational_models",
        "out_dir": EXPERIMENT_DIR / "result" / "sql_baselines" / "bayescard_graph_relational" / "dblp",
    },
    "ldbc": {
        "data_dir": SAFEBOUND_DIR / "repro_bayescard_ldbc_sf1" / "prepared_csv",
        "pattern_dir": EXPERIMENT_DIR / "patterns" / "gcard" / "lsqb_glogs_with_pred_10",
        "truth_file": EXPERIMENT_DIR / "result" / "truecard" / "ldbc_sf1_unique_with_pred_10" / "truth.csv",
        "hdf_dir": EXPERIMENT_DIR / "catalogs" / "ldbc" / "bayescard_graph_schema_hdf",
        "model_dir": EXPERIMENT_DIR / "catalogs" / "ldbc" / "bayescard_graph_schema_models_s10k_j50k",
        "out_dir": EXPERIMENT_DIR / "result" / "sql_baselines" / "bayescard_graph_relational" / "ldbc_with_pred",
    },
}

LDBC_MANIFEST = EXPERIMENT_DIR / "datasets" / "ldbc" / "sf1" / "manifest.json"
LDBC_DROP_ATTRIBUTES = {
    "city": {"url"},
    "comment": {"locationip", "content"},
    "company": {"url"},
    "continent": {"url"},
    "country": {"url"},
    "forum": {"title"},
    "person": {"firstname", "lastname", "locationip", "email"},
    "post": {"imagefile", "locationip", "content"},
    "tag": {"url"},
    "tagclass": {"url"},
    "university": {"url"},
}


def ensure_dir(path):
    Path(path).mkdir(parents=True, exist_ok=True)


def csv_header(path):
    with open(path, newline="", encoding="utf-8") as handle:
        row = next(csv.reader(handle), None)
    if not row:
        raise ValueError("Empty CSV: {}".format(path))
    return row


def count_csv_rows(path):
    count = 0
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            count += block.count(b"\n")
    return max(0, count - 1)


def load_row_count_cache(path):
    cache = {}
    if not path.exists():
        return cache
    with open(path, newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            cache[row["path"]] = int(row["rows"])
    return cache


def write_row_count_cache(path, cache):
    ensure_dir(path.parent)
    with open(path, "w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=["path", "rows"])
        writer.writeheader()
        for key in sorted(cache):
            writer.writerow({"path": key, "rows": cache[key]})


def row_count(path, cache):
    key = str(Path(path).resolve())
    if key not in cache:
        cache[key] = count_csv_rows(path)
    return cache[key]


def attrs_without(all_attrs, keep_attrs):
    keep = set(keep_attrs)
    return [attr for attr in all_attrs if attr not in keep]


def infer_attr_types(dataset, threshold=3000):
    attr_types = {}
    for column in dataset.columns:
        n_unique = dataset[column].nunique()
        if n_unique == 2:
            attr_types[column] = "boolean"
        elif pd.api.types.is_numeric_dtype(dataset[column]) and (
            n_unique >= len(dataset) / 30 or n_unique > threshold
        ):
            attr_types[column] = "continuous"
        else:
            attr_types[column] = "categorical"
    return attr_types


def add_csv_table(schema, table_name, csv_path, primary_key, keep_attrs, row_cache):
    attrs = csv_header(csv_path)
    schema.add_table(
        Table(
            table_name,
            primary_key=list(primary_key),
            attributes=attrs,
            irrelevant_attributes=attrs_without(attrs, keep_attrs),
            keep_fk_attributes=[attr for attr in ("src", "dst") if attr in attrs],
            csv_file_location=str(csv_path),
            table_size=row_count(csv_path, row_cache),
        )
    )


def role_table_name(edge_table, endpoint, vertex_table):
    return "role__{}__{}__{}".format(edge_table, endpoint, vertex_table)


def base_table_name(table_name):
    if table_name.startswith("role__"):
        parts = table_name.split("__", 3)
        if len(parts) == 4:
            return parts[3]
    return table_name


def build_aids_schema(data_dir, row_cache, role_schema=False):
    schema = SchemaGraph()
    vertex_path = Path(data_dir) / "vertex.csv"
    if not role_schema:
        add_csv_table(schema, "vertex", vertex_path, ["id"], ["id"], row_cache)
    for label in ("0", "1", "2", "3"):
        table = "edge_{}".format(label)
        add_csv_table(schema, table, Path(data_dir) / "{}.csv".format(label), ["src", "dst"], ["src", "dst"], row_cache)
        if role_schema:
            src_role = role_table_name(table, "src", "vertex")
            dst_role = role_table_name(table, "dst", "vertex")
            add_csv_table(schema, src_role, vertex_path, ["id"], ["id"], row_cache)
            add_csv_table(schema, dst_role, vertex_path, ["id"], ["id"], row_cache)
            schema.add_relationship(table, "src", src_role, "id")
            schema.add_relationship(table, "dst", dst_role, "id")
        else:
            schema.add_relationship(table, "src", "vertex", "id")
            schema.add_relationship(table, "dst", "vertex", "id")
    return schema


def build_dblp_schema(data_dir, row_cache, role_schema=False):
    schema = SchemaGraph()
    data_dir = Path(data_dir)
    vertex_paths = {
        vertex_path.stem: vertex_path
        for vertex_path in sorted(data_dir.glob("v*.csv"), key=lambda p: int(p.stem[1:]))
    }
    if not role_schema:
        for vertex_table, vertex_path in sorted(vertex_paths.items(), key=lambda item: int(item[0][1:])):
            add_csv_table(schema, vertex_table, vertex_path, ["id"], ["id"], row_cache)

    edge_re = re.compile(r"pe0_(\d+)_(\d+)\.csv$")
    edge_paths = []
    for edge_path in data_dir.glob("pe0_*_*.csv"):
        match = edge_re.match(edge_path.name)
        if match:
            edge_paths.append((int(match.group(1)), int(match.group(2)), edge_path))
    for src_label, dst_label, edge_path in sorted(edge_paths):
        table = edge_path.stem
        src_vertex = "v{}".format(src_label)
        dst_vertex = "v{}".format(dst_label)
        add_csv_table(schema, table, edge_path, ["src", "dst"], ["src", "dst"], row_cache)
        if role_schema:
            src_role = role_table_name(table, "src", src_vertex)
            dst_role = role_table_name(table, "dst", dst_vertex)
            add_csv_table(schema, src_role, vertex_paths[src_vertex], ["id"], ["id"], row_cache)
            add_csv_table(schema, dst_role, vertex_paths[dst_vertex], ["id"], ["id"], row_cache)
            schema.add_relationship(table, "src", src_role, "id")
            schema.add_relationship(table, "dst", dst_role, "id")
        else:
            schema.add_relationship(table, "src", src_vertex, "id")
            schema.add_relationship(table, "dst", dst_vertex, "id")
    return schema


def build_ldbc_schema(data_dir, row_cache, role_schema=False):
    with open(LDBC_MANIFEST, encoding="utf-8") as handle:
        manifest = json.load(handle)

    schema = SchemaGraph()
    data_dir = Path(data_dir)
    vertex_paths = {
        item["label"]: data_dir / item["file"]["path"]
        for item in manifest["vertices"]
    }
    if not role_schema:
        for table, csv_path in sorted(vertex_paths.items()):
            attrs = csv_header(csv_path)
            keep = [attr for attr in attrs if attr not in LDBC_DROP_ATTRIBUTES.get(table, set())]
            add_csv_table(schema, table, csv_path, ["id"], keep, row_cache)

    for edge in sorted(manifest["edges"], key=lambda item: item["label"]):
        table = edge["label"]
        csv_path = data_dir / edge["file"]["path"]
        add_csv_table(
            schema,
            table,
            csv_path,
            ["id"],
            csv_header(csv_path),
            row_cache,
        )
        src_vertex = edge["src_label"]
        dst_vertex = edge["dst_label"]
        if role_schema:
            src_role = role_table_name(table, "src", src_vertex)
            dst_role = role_table_name(table, "dst", dst_vertex)
            for role, vertex in ((src_role, src_vertex), (dst_role, dst_vertex)):
                vertex_path = vertex_paths[vertex]
                attrs = csv_header(vertex_path)
                keep = [
                    attr
                    for attr in attrs
                    if attr not in LDBC_DROP_ATTRIBUTES.get(vertex, set())
                ]
                add_csv_table(schema, role, vertex_path, ["id"], keep, row_cache)
            schema.add_relationship(table, "src", src_role, "id")
            schema.add_relationship(table, "dst", dst_role, "id")
        else:
            schema.add_relationship(table, "src", src_vertex, "id")
            schema.add_relationship(table, "dst", dst_vertex, "id")
    return schema


def schema_for(args, role_schema=False):
    cache_path = Path(args.out_dir) / "row_counts.csv"
    cache = load_row_count_cache(cache_path)
    if args.dataset == "aids_merged":
        schema = build_aids_schema(args.data_dir, cache, role_schema=role_schema)
    elif args.dataset == "dblp":
        schema = build_dblp_schema(args.data_dir, cache, role_schema=role_schema)
    elif args.dataset == "ldbc":
        schema = build_ldbc_schema(args.data_dir, cache, role_schema=role_schema)
    else:
        raise ValueError("Unsupported dataset {}".format(args.dataset))
    write_row_count_cache(cache_path, cache)
    return schema


def patch_header_csv_reader(csv_separator):
    def read_table_csv_header(table_obj, csv_seperator=",", stats=False):
        frame = pd.read_csv(table_obj.csv_file_location, sep=csv_separator)
        expected = list(table_obj.attributes)
        if list(frame.columns) != expected:
            if len(frame.columns) != len(expected):
                raise ValueError(
                    "CSV header mismatch for {}: got {}, expected {}".format(
                        table_obj.csv_file_location, list(frame.columns), expected
                    )
                )
            frame.columns = expected
        frame.columns = [table_obj.table_name + "." + attr for attr in expected]
        for attribute in table_obj.irrelevant_attributes:
            column = table_obj.table_name + "." + attribute
            if column in frame.columns:
                frame = frame.drop(column, axis=1)
        return frame.apply(pd.to_numeric, errors="ignore")

    prepare_single_tables.read_table_csv = read_table_csv_header


def unwrap_literal(value):
    if isinstance(value, dict):
        return next(iter(value.values()))
    return value


def sql_literal(value):
    value = unwrap_literal(value)
    if isinstance(value, bool):
        return "1" if value else "0"
    if isinstance(value, (int, float)):
        return str(value)
    return "'{}'".format(str(value).replace("'", "''"))


def graph_pattern_to_sql(dataset, pattern_path):
    with open(pattern_path, encoding="utf-8") as handle:
        pattern = json.load(handle)
    if dataset != "ldbc" and pattern.get("predicates"):
        raise ValueError("Expected no-predicate pattern: {}".format(pattern_path))

    vertices = {int(vertex["id"]): vertex["label"] for vertex in pattern["vertices"]}
    vertex_aliases = {vertex_id: "n{}".format(vertex_id) for vertex_id in vertices}
    from_items = []
    for vertex_id in sorted(vertices):
        from_items.append((vertices[vertex_id], vertex_aliases[vertex_id]))

    conditions = []
    for edge in sorted(pattern["edges"], key=lambda item: int(item["id"])):
        edge_id = int(edge["id"])
        src = int(edge["src"])
        dst = int(edge["dst"])
        if dataset == "aids_merged":
            edge_table = "edge_{}".format(edge["label"])
        elif dataset == "dblp":
            src_label = vertices[src]
            dst_label = vertices[dst]
            if not (src_label.startswith("v") and dst_label.startswith("v")):
                raise ValueError("Unexpected DBLP vertex labels in {}".format(pattern_path))
            edge_table = "pe0_{}_{}".format(src_label[1:], dst_label[1:])
        elif dataset == "ldbc":
            edge_table = edge["label"]
        else:
            raise ValueError(dataset)
        edge_alias = "r{}".format(edge_id)
        from_items.append((edge_table, edge_alias))
        conditions.append("{}.src = {}.id".format(edge_alias, vertex_aliases[src]))
        conditions.append("{}.dst = {}.id".format(edge_alias, vertex_aliases[dst]))

    if dataset == "ldbc":
        operator = {
            "eq": "=",
            "ne": "!=",
            "gt": ">",
            "ge": ">=",
            "lt": "<",
            "le": "<=",
        }
        for predicate in pattern.get("predicates", []):
            if predicate["target"] == "vertex":
                alias = vertex_aliases[int(predicate["id"])]
            else:
                alias = "r{}".format(int(predicate["id"]))
            conditions.append(
                "{}.{}{}{}".format(
                    alias,
                    predicate["property"],
                    operator[predicate["op"].lower()],
                    sql_literal(predicate["value"]),
                )
            )

    from_sql = ", ".join("{} {}".format(table, alias) for table, alias in from_items)
    where_sql = " AND ".join(conditions)
    return "SELECT COUNT(*) FROM {} WHERE {};".format(from_sql, where_sql)


def workload_path(args):
    return Path(args.out_dir) / "workload.csv"


def build_workload(args):
    ensure_dir(args.out_dir)
    rows = []
    with open(args.truth_file, newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        for index, row in enumerate(reader):
            if args.limit is not None and len(rows) >= args.limit:
                break
            if row.get("status") and row.get("status") != "ok":
                continue
            if args.dataset == "aids_merged":
                pattern = row["pattern"]
                query_name = pattern.replace("/", "_")
                pattern_path = Path(args.pattern_dir) / (pattern + ".json")
            elif args.dataset == "dblp":
                query_name = row["query"]
                pattern = query_name
                pattern_path = Path(args.pattern_dir) / (query_name + ".json")
            elif args.dataset == "ldbc":
                query_name = row["variant"] if row.get("variant") else row["query"]
                pattern = row["base"] if row.get("base") else row["query"]
                pattern_path = (
                    Path(args.pattern_dir) / row["group"] / (query_name + ".json")
                )
            else:
                raise ValueError(args.dataset)
            if not pattern_path.exists():
                raise FileNotFoundError(pattern_path)
            sql = graph_pattern_to_sql(args.dataset, pattern_path)
            rows.append(
                {
                    "dataset": args.dataset,
                    "query_no": len(rows),
                    "query": query_name,
                    "pattern": pattern,
                    "pattern_path": str(pattern_path),
                    "true_cardinality": int(float(row["true_cardinality"])),
                    "sql": sql,
                }
            )

    out = workload_path(args)
    with open(out, "w", newline="", encoding="utf-8") as handle:
        fieldnames = ["dataset", "query_no", "query", "pattern", "pattern_path", "true_cardinality", "sql"]
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        for row in rows:
            writer.writerow(row)

    sql_path = Path(args.out_dir) / "workload.sql"
    with open(sql_path, "w", encoding="utf-8") as handle:
        for row in rows:
            handle.write("-- {} true={}\n{}\n".format(row["query"], row["true_cardinality"], row["sql"]))
    print("Wrote {} workload rows to {}".format(len(rows), out))
    print("SQL written to {}".format(sql_path))
    return rows


def split_and(expr):
    return [part.strip() for part in re.split(r"(?i)\s+and\s+", expr.strip()) if part.strip()]


def find_fk_relationship(schema, table, attr):
    for relationship in schema.relationships:
        if relationship.start == table and relationship.start_attr == attr:
            return relationship
    return None


def clone_schema_with_aliases(schema, alias_dict):
    alias_schema = copy.deepcopy(schema)
    for table in alias_schema.tables:
        if not hasattr(table, "table_nn_attribute"):
            table.table_nn_attribute = table.table_name + "_nn"
    for alias, physical in alias_dict.items():
        if alias in alias_schema.table_dictionary:
            raise ValueError("Alias {} conflicts with a physical table name".format(alias))
        physical_table = schema.table_dictionary[physical]
        alias_schema.add_table(
            Table(
                alias,
                primary_key=list(physical_table.primary_key),
                table_size=physical_table.table_size,
                csv_file_location=physical_table.csv_file_location,
                attributes=list(physical_table.attributes or []),
                irrelevant_attributes=list(getattr(physical_table, "irrelevant_attributes", []) or []),
                keep_fk_attributes=list(getattr(physical_table, "keep_fk_attributes", []) or []),
                drop_id_attributes=list(getattr(physical_table, "drop_id_attributes", []) or []),
                sample_rate=physical_table.sample_rate,
                fd_list=[],
                no_compression=list(getattr(physical_table, "no_compression", []) or []),
            )
        )
        alias_schema.table_dictionary[alias].table_nn_attribute = alias + "_nn"
    return alias_schema


def add_alias_relationship(schema, start, start_attr, end, end_attr):
    identifier = "{}.{} = {}.{}".format(start, start_attr, end, end_attr)
    reverse = "{}.{} = {}.{}".format(end, end_attr, start, start_attr)
    if identifier in schema.relationship_dictionary:
        return identifier
    if reverse in schema.relationship_dictionary:
        return reverse
    return schema.add_relationship(start, start_attr, end, end_attr)


def add_alias_join(query, alias_schema, physical_schema, left, right, alias_dict):
    left_alias, left_attr = left.split(".", 1)
    right_alias, right_attr = right.split(".", 1)
    left_table = alias_dict[left_alias]
    right_table = alias_dict[right_alias]

    physical_rel = "{}.{} = {}.{}".format(left_table, left_attr, right_table, right_attr)
    physical_rev = "{}.{} = {}.{}".format(right_table, right_attr, left_table, left_attr)
    if physical_rel in physical_schema.relationship_dictionary:
        query.add_join_condition(add_alias_relationship(alias_schema, left_alias, left_attr, right_alias, right_attr))
        return
    if physical_rev in physical_schema.relationship_dictionary:
        query.add_join_condition(add_alias_relationship(alias_schema, right_alias, right_attr, left_alias, left_attr))
        return

    left_fk = find_fk_relationship(physical_schema, left_table, left_attr)
    right_fk = find_fk_relationship(physical_schema, right_table, right_attr)
    if left_fk is not None and right_fk is not None and left_fk.end == right_fk.end and left_fk.end_attr == right_fk.end_attr:
        query.add_join_condition(add_alias_relationship(alias_schema, left_alias, left_attr, left_fk.end, left_fk.end_attr))
        query.add_join_condition(add_alias_relationship(alias_schema, right_alias, right_attr, right_fk.end, right_fk.end_attr))
        return

    raise ValueError("No alias schema relationship for {} = {}".format(left, right))


def parse_count_sql_alias_safe(query_sql, schema):
    sql = " ".join(query_sql.strip().rstrip(";").split())
    match = re.match(r"(?is)^select\s+count\(\*\)\s+from\s+(.+?)(?:\s+where\s+(.+))?$", sql)
    if not match:
        raise ValueError("Only SELECT COUNT(*) FROM ... [WHERE ...] queries are supported")
    from_part, where_part = match.groups()

    alias_dict = {}
    for item in [part.strip() for part in from_part.split(",") if part.strip()]:
        item_match = re.match(r"(?is)^([A-Za-z_][A-Za-z0-9_]*)\s+(?:as\s+)?([A-Za-z_][A-Za-z0-9_]*)$", item)
        if not item_match:
            raise ValueError("Unsupported FROM item: {}".format(item))
        table, alias = item_match.groups()
        if table not in schema.table_dictionary:
            raise KeyError("Unknown table {}".format(table))
        if alias in alias_dict:
            raise ValueError("Duplicate alias {}".format(alias))
        alias_dict[alias] = table

    alias_schema = clone_schema_with_aliases(schema, alias_dict)
    query = Query(alias_schema)
    for alias in alias_dict:
        query.table_set.add(alias)

    if where_part:
        for condition in split_and(where_part):
            condition = condition.strip().strip("()")
            pred_match = re.match(r"^(.+?)\s*(=|>=|<=|>|<)\s*(.+)$", condition)
            if not pred_match:
                raise ValueError("Unsupported WHERE condition: {}".format(condition))
            left, operator, right = pred_match.groups()
            left = left.strip()
            right = right.strip()
            if operator == "=" and re.match(r"^[A-Za-z_][A-Za-z0-9_]*\.[A-Za-z_][A-Za-z0-9_]*$", right):
                add_alias_join(query, alias_schema, schema, left, right, alias_dict)
            else:
                alias, attr = left.split(".", 1)
                query.add_where_condition(alias, "{}{}{}".format(attr, operator, right))

    return query, alias_dict


def physical_condition(alias, condition, alias_to_physical):
    return alias_to_physical[alias], condition


class AliasBN(object):
    def __init__(self, physical_bn, alias_to_physical, relationship_map, alias_schema):
        self._bn = physical_bn
        self.alias_to_physical = dict(alias_to_physical)
        self.physical_to_aliases = {}
        for alias, physical in self.alias_to_physical.items():
            self.physical_to_aliases.setdefault(physical, set()).add(alias)
        self.schema_graph = alias_schema
        self.table_set = set(self.alias_to_physical.keys()) | (set(physical_bn.table_set) - set(self.physical_to_aliases.keys()))
        self.relationship_set = set()
        for rel in physical_bn.relationship_set:
            mapped = relationship_map.get(rel)
            if mapped is not None:
                self.relationship_set.add(mapped)
            elif rel in alias_schema.relationship_dictionary:
                self.relationship_set.add(rel)
        for rel in list(self.relationship_set):
            obj = alias_schema.relationship_dictionary[rel]
            self.table_set.add(obj.start)
            self.table_set.add(obj.end)
        self.column_names = physical_bn.column_names
        self.full_join_size = physical_bn.full_join_size

    def __getattr__(self, name):
        return getattr(self._bn, name)

    def _physical_condition(self, table, condition):
        physical = self.alias_to_physical.get(table, table)
        if table != physical:
            alias_obj = self.schema_graph.table_dictionary[table]
            physical_obj = self._bn.schema_graph.table_dictionary[physical]
            condition = condition.replace(alias_obj.table_nn_attribute, physical_obj.table_nn_attribute, 1)
        return physical, condition

    def _parse_conditions(self, conditions, group_by_columns=None, group_by_tuples=None):
        physical_conditions = [self._physical_condition(table, condition) for table, condition in conditions]
        physical_group_by = None
        if group_by_columns:
            physical_group_by = [
                (self.alias_to_physical.get(table, table), attr)
                for table, attr in group_by_columns
            ]
        return self._bn._parse_conditions(
            physical_conditions,
            group_by_columns=physical_group_by,
            group_by_tuples=group_by_tuples,
        )

    def compute_mergeable_relationships(self, query, start_table):
        relationships = []
        queue = [start_table]
        while queue:
            table = queue.pop(0)
            table_obj = self.schema_graph.table_dictionary[table]
            for relationship in table_obj.incoming_relationships + table_obj.outgoing_relationships:
                if relationship.identifier in self.relationship_set and relationship.identifier in query.relationship_set and relationship.identifier not in relationships:
                    relationships.append(relationship.identifier)
                    queue.append(relationship.start if relationship.end == table else relationship.end)
        return relationships

    def relevant_conditions(self, query, merged_tables=None):
        if merged_tables is None:
            merged_tables = query.table_set.intersection(self.table_set)
        conditions = []
        for table, conds in query.table_where_condition_dict.items():
            if table in merged_tables:
                physical = self.alias_to_physical.get(table, table)
                for condition in conds:
                    conditions.append((physical, condition))
        for table in merged_tables:
            physical = self.alias_to_physical.get(table, table)
            table_obj = self._bn.schema_graph.table_dictionary[physical]
            conditions.append((physical, getattr(table_obj, "table_nn_attribute", physical + "_nn") + " IS NOT NULL"))
        return conditions

    def compute_multipliers(self, query):
        return []


def alias_relationship_candidates(alias_schema, alias_dict, physical_rel, query_relationships):
    candidates = []
    for alias_rel in sorted(query_relationships):
        obj = alias_schema.relationship_dictionary[alias_rel]
        start_physical = alias_dict.get(obj.start, obj.start)
        end_physical = alias_dict.get(obj.end, obj.end)
        if (
            start_physical == base_table_name(physical_rel.start)
            and end_physical == base_table_name(physical_rel.end)
            and obj.start_attr == physical_rel.start_attr
            and obj.end_attr == physical_rel.end_attr
        ):
            candidates.append(alias_rel)
    return candidates


def is_edge_like_table(table_obj):
    attrs = set(table_obj.attributes or [])
    return "src" in attrs and "dst" in attrs


def alias_combo_is_consistent(combo, physical_rels, alias_schema, alias_dict, physical_schema):
    edge_alias_by_physical = {}
    alias_to_physical = {}
    relationship_map = {}
    for physical_rel_id, alias_rel_id in zip(physical_rels, combo):
        physical_rel = physical_schema.relationship_dictionary[physical_rel_id]
        alias_rel = alias_schema.relationship_dictionary[alias_rel_id]
        relationship_map[physical_rel_id] = alias_rel_id
        alias_to_physical[alias_rel.start] = physical_rel.start
        alias_to_physical[alias_rel.end] = physical_rel.end
        if is_edge_like_table(physical_schema.table_dictionary[physical_rel.start]):
            previous = edge_alias_by_physical.setdefault(physical_rel.start, alias_rel.start)
            if previous != alias_rel.start:
                return None, None
    return alias_to_physical, relationship_map


def build_alias_ensemble(ensemble, query, alias_dict):
    alias_schema = query.schema_graph
    alias_ensemble = copy.copy(ensemble)
    alias_ensemble.schema_graph = alias_schema
    alias_ensemble.bns = []

    for bn in ensemble.bns:
        physical_rels = sorted(rel for rel in bn.relationship_set if rel in ensemble.schema_graph.relationship_dictionary)
        if not physical_rels:
            alias_ensemble.bns.append(bn)
            continue

        candidate_lists = []
        for rel in physical_rels:
            candidates = alias_relationship_candidates(
                alias_schema,
                alias_dict,
                ensemble.schema_graph.relationship_dictionary[rel],
                query.relationship_set,
            )
            if not candidates:
                candidate_lists = []
                break
            candidate_lists.append(candidates)
        if not candidate_lists:
            continue

        for combo in itertools.product(*candidate_lists):
            if len(set(combo)) != len(combo):
                continue
            alias_to_physical, relationship_map = alias_combo_is_consistent(
                combo,
                physical_rels,
                alias_schema,
                alias_dict,
                ensemble.schema_graph,
            )
            if alias_to_physical is None:
                continue
            alias_ensemble.bns.append(AliasBN(bn, alias_to_physical, relationship_map, alias_schema))
    if not alias_ensemble.bns:
        raise ValueError("No alias BNs matched the query relationships")
    return alias_ensemble


def build_single_parsed_query_from_query(ensemble, query):
    first_bn, next_mergeable_relationships, next_mergeable_tables = ensemble._greedily_select_first_cardinality_bn(
        query,
        rdc_spn_selection=True,
        rdc_attribute_dict={},
    )
    if first_bn is None:
        raise ValueError("No mergeable BN found")

    factors = generate_factors(
        ensemble,
        query,
        first_bn,
        next_mergeable_relationships,
        next_mergeable_tables,
        rdc_bn_selection=True,
        rdc_attribute_dict={},
        merge_indicator_exp=True,
        exploit_incoming_multipliers=False,
        prefer_disjunct=False,
    )
    factors = factor_refine(factors)

    parsed = []
    for factor in factors:
        if isinstance(factor, IndicatorExpectation):
            ranges = factor.spn._parse_conditions(factor.conditions, group_by_columns=None, group_by_tuples=None)
            actual_query, fanout = prepare_single_query(ranges, factor)
            parsed.append(
                {
                    "bn_index": ensemble.bns.index(factor.spn),
                    "inverse": factor.inverse,
                    "query": actual_query,
                    "expectation": fanout,
                }
            )
        elif isinstance(factor, Expectation):
            raise NotImplementedError("Expectation factors are not implemented by this adapter")
        else:
            parsed.append(factor)
    return parsed


def decode_and_cardinality(ensemble, parsed_query):
    decoded_query = ensemble.parse_query_all([parsed_query])[0]
    has_none = any(isinstance(part, dict) and part.get("query") is None for part in decoded_query[1:])
    if has_none:
        raise ValueError("Decoded query contains None")
    prediction = ensemble.cardinality(decoded_query)
    if isinstance(prediction, np.ndarray):
        prediction = prediction[0]
    if prediction is None or not math.isfinite(float(prediction)):
        raise ValueError("Non-finite prediction {}".format(prediction))
    return max(1.0, float(prediction))


def qerror(est, truth):
    est = max(1.0, float(est))
    truth = max(1.0, float(truth))
    return max(est / truth, truth / est)


def percentile(values, p):
    values = sorted(float(v) for v in values if math.isfinite(float(v)))
    if not values:
        return ""
    if len(values) == 1:
        return values[0]
    pos = (len(values) - 1) * p
    lo = int(math.floor(pos))
    hi = int(math.ceil(pos))
    if lo == hi:
        return values[lo]
    frac = pos - lo
    return values[lo] * (1.0 - frac) + values[hi] * frac


def prepare_hdf(args):
    patch_header_csv_reader(args.csv_separator)
    schema = schema_for(args, role_schema=args.bn_scope == "edge-incidence")
    if args.force and Path(args.hdf_dir).exists():
        shutil.rmtree(args.hdf_dir)
    ensure_dir(args.hdf_dir)
    prepare_all_tables(
        schema,
        args.hdf_dir,
        csv_seperator=args.csv_separator,
        max_table_data=args.max_rows_per_hdf_file,
    )
    print("HDF written to {}".format(args.hdf_dir))


def relationship_groups_for_training(schema, scope):
    if scope == "single-relationship":
        return [[relationship.identifier] for relationship in schema.relationships]
    if scope != "edge-incidence":
        raise ValueError("Unsupported BN scope {}".format(scope))
    groups = []
    for table in schema.tables:
        outgoing = sorted(
            [relationship.identifier for relationship in table.outgoing_relationships],
            key=lambda rel: schema.relationship_dictionary[rel].start_attr,
        )
        if outgoing:
            groups.append(outgoing)
    return groups


def train(args):
    schema = schema_for(args, role_schema=args.bn_scope == "edge-incidence")
    ensure_dir(args.model_dir)
    prep = JoinDataPreparator(os.path.join(args.hdf_dir, "meta_data.pkl"), schema, max_table_data=args.max_rows_per_hdf_file)
    relationship_groups = relationship_groups_for_training(schema, args.bn_scope)
    start = args.start_index
    end = args.end_index if args.end_index is not None else len(relationship_groups)
    if args.limit_relationships is not None:
        end = min(end, start + args.limit_relationships)
    print("Training {} BN groups [{}:{}) out of {}".format(args.bn_scope, start, end, len(relationship_groups)))
    for index, relation in enumerate(relationship_groups[start:end], start=start):
        model_path = os.path.join(args.model_dir, "{}_{}_{}_{}.pkl".format(args.model_prefix, index, args.learning_algo, args.max_parents))
        if os.path.exists(model_path) and not args.force:
            print("Skipping existing {}".format(model_path))
            continue
        print("Training {}: {}".format(index, relation))
        df, meta_types, null_values, full_join_est = prep.generate_n_samples(
            args.df_sample_size,
            relationship_list=relation,
            post_sampling_factor=args.post_sampling_factor,
        )
        columns = list(df.columns)
        meta_info = build_meta_info(columns, null_values)
        bn = Bayescard_BN(
            schema,
            relation,
            column_names=columns,
            full_join_size=full_join_est,
            table_meta_data=prep.table_meta_data,
            meta_types=meta_types,
            null_values=null_values,
            meta_info=meta_info,
        )
        bn.build_from_data(
            df,
            attr_type=infer_attr_types(df),
            algorithm=args.learning_algo,
            max_parents=args.max_parents,
            ignore_cols=["id"],
            sample_size=args.sample_size,
        )
        with open(model_path, "wb") as handle:
            pickle.dump(bn, handle, pickle.HIGHEST_PROTOCOL)
        print("Saved {}".format(model_path))


def model_file_index(name, model_prefix):
    match = re.match(
        r"^{}_(\d+)_".format(re.escape(model_prefix)),
        name,
    )
    if not match:
        raise ValueError("Unexpected model filename {}".format(name))
    return int(match.group(1))


def load_ensemble(schema, model_dir, model_prefix, expected_models=None):
    ensemble = BN_ensemble(schema)
    files = [name for name in os.listdir(model_dir) if name.endswith(".pkl") and (not model_prefix or name.startswith(model_prefix + "_"))]
    files = sorted(files, key=lambda name: model_file_index(name, model_prefix))
    if expected_models is not None:
        actual = [model_file_index(name, model_prefix) for name in files]
        expected = list(range(expected_models))
        if actual != expected:
            raise RuntimeError(
                "Incomplete model set in {}: got {}, expected {}".format(
                    model_dir, actual, expected
                )
            )
    for name in files:
        with open(os.path.join(model_dir, name), "rb") as handle:
            bn = pickle.load(handle)
        bn.infer_algo = "exact-jit"
        bn.init_inference_method()
        ensemble.bns.append(bn)
    if not ensemble.bns:
        raise RuntimeError("No .pkl BayesCard models found in {}".format(model_dir))
    return ensemble


def write_csv(path, rows, fieldnames):
    ensure_dir(Path(path).parent)
    with open(path, "w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        for row in rows:
            writer.writerow({field: row.get(field, "") for field in fieldnames})


def evaluate(args):
    if args.rebuild_workload or not workload_path(args).exists():
        build_workload(args)
    parse_schema = schema_for(args, role_schema=False)
    model_schema = schema_for(args, role_schema=args.bn_scope == "edge-incidence")
    expected_models = len(
        relationship_groups_for_training(model_schema, args.bn_scope)
    )
    ensemble = load_ensemble(
        model_schema,
        args.model_dir,
        args.model_prefix,
        expected_models=expected_models,
    )

    rows = []
    with open(workload_path(args), newline="", encoding="utf-8") as handle:
        workload = list(csv.DictReader(handle))
    for i, item in enumerate(workload):
        if args.limit is not None and i >= args.limit:
            break
        tic = time.perf_counter()
        prediction = ""
        qe = ""
        relationships = ""
        mode = "exact_relationships"
        try:
            query, alias_dict = parse_count_sql_alias_safe(item["sql"], parse_schema)
            relationships = ";".join(sorted(query.relationship_set))
            alias_ensemble = build_alias_ensemble(ensemble, query, alias_dict)
            parsed = build_single_parsed_query_from_query(alias_ensemble, query)
            prediction = decode_and_cardinality(alias_ensemble, parsed)
            qe = qerror(prediction, item["true_cardinality"])
            status = "ok"
        except Exception as exc:
            status = "failed: {}: {}".format(type(exc).__name__, exc)
            prediction = 1.0
            qe = qerror(prediction, item["true_cardinality"])
            if args.verbose:
                traceback.print_exc()
        latency_s = time.perf_counter() - tic
        row = {
            "dataset": args.dataset,
            "query_no": item["query_no"],
            "query": item["query"],
            "pattern": item["pattern"],
            "true_cardinality": item["true_cardinality"],
            "prediction": prediction,
            "qerror": qe,
            "latency_s": latency_s,
            "status": status,
            "mode": mode,
            "relationships": relationships,
            "sql": item["sql"],
        }
        rows.append(row)
        print("{} pred={} qerror={} latency_s={:.6f} status={}".format(item["query"], prediction, qe, latency_s, status))

    out_prefix = args.dataset.replace("-", "_")
    detail_path = Path(args.out_dir) / "{}_bayescard_graph_relational_detail.csv".format(out_prefix)
    fieldnames = [
        "dataset",
        "query_no",
        "query",
        "pattern",
        "true_cardinality",
        "prediction",
        "qerror",
        "latency_s",
        "status",
        "mode",
        "relationships",
        "sql",
    ]
    write_csv(detail_path, rows, fieldnames)

    qvals = [float(row["qerror"]) for row in rows if row.get("qerror") not in ("", None)]
    lvals = [float(row["latency_s"]) for row in rows if row.get("latency_s") not in ("", None)]
    summary = {
        "dataset": args.dataset,
        "rows": len(rows),
        "ok": sum(1 for row in rows if row["status"] == "ok"),
        "failed": sum(1 for row in rows if row["status"] != "ok"),
        "valid_qerror": len(qvals),
        "median_qerror": percentile(qvals, 0.50),
        "p90_qerror": percentile(qvals, 0.90),
        "p95_qerror": percentile(qvals, 0.95),
        "p99_qerror": percentile(qvals, 0.99),
        "max_qerror": max(qvals) if qvals else "",
        "median_latency_s": percentile(lvals, 0.50),
        "p90_latency_s": percentile(lvals, 0.90),
        "total_latency_s": sum(lvals) if lvals else "",
        "model_dir": args.model_dir,
        "workload": str(workload_path(args)),
    }
    summary_path = Path(args.out_dir) / "{}_bayescard_graph_relational_summary.csv".format(out_prefix)
    write_csv(summary_path, [summary], list(summary.keys()))
    print("Detail written to {}".format(detail_path))
    print("Summary written to {}".format(summary_path))
    print(summary)


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("stage", choices=["prepare-workload", "prepare-hdf", "train", "evaluate", "all"])
    parser.add_argument("--dataset", choices=["aids_merged", "dblp", "ldbc"], required=True)
    parser.add_argument("--data-dir")
    parser.add_argument("--pattern-dir")
    parser.add_argument("--truth-file")
    parser.add_argument("--hdf-dir")
    parser.add_argument("--model-dir")
    parser.add_argument("--out-dir")
    parser.add_argument("--csv-separator", default=",")
    parser.add_argument("--max-rows-per-hdf-file", type=int, default=20000000)
    parser.add_argument("--learning-algo", default="chow-liu")
    parser.add_argument("--max-parents", type=int, default=1)
    parser.add_argument("--sample-size", type=int)
    parser.add_argument("--df-sample-size", type=int)
    parser.add_argument("--post-sampling-factor", type=int, default=10)
    parser.add_argument("--start-index", type=int, default=0)
    parser.add_argument("--end-index", type=int, default=None)
    parser.add_argument("--limit-relationships", type=int, default=None)
    parser.add_argument("--bn-scope", choices=["edge-incidence", "single-relationship"], default="edge-incidence")
    parser.add_argument("--model-prefix", default="edgeinc")
    parser.add_argument("--limit", type=int, default=None)
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--rebuild-workload", action="store_true")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    defaults = DEFAULTS[args.dataset]
    for key, value in defaults.items():
        if getattr(args, key) is None:
            setattr(args, key, str(value))
        else:
            path = Path(getattr(args, key))
            if not path.is_absolute():
                setattr(args, key, str((INVOCATION_CWD / path).resolve()))
    if args.sample_size is None:
        args.sample_size = 10000 if args.dataset == "ldbc" else 200000
    if args.df_sample_size is None:
        args.df_sample_size = 50000 if args.dataset == "ldbc" else 10000000
    return args


def main():
    args = parse_args()
    if args.stage == "prepare-workload":
        build_workload(args)
    elif args.stage == "prepare-hdf":
        prepare_hdf(args)
    elif args.stage == "train":
        train(args)
    elif args.stage == "evaluate":
        evaluate(args)
    elif args.stage == "all":
        build_workload(args)
        prepare_hdf(args)
        train(args)
        evaluate(args)
    else:
        raise ValueError(args.stage)


if __name__ == "__main__":
    main()
