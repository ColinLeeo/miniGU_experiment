#!/usr/bin/env python
import argparse
import copy
import csv
import itertools
import os
import pickle
import re
import sys
import time
import traceback


REPRO_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SAFEB0UND_BAYESCARD = os.path.abspath(os.path.join(REPRO_DIR, "..", "bayescard"))
if SAFEB0UND_BAYESCARD not in sys.path:
    sys.path.insert(0, SAFEB0UND_BAYESCARD)
if os.path.dirname(__file__) not in sys.path:
    sys.path.insert(0, os.path.dirname(__file__))

import numpy as np

from DeepDBUtils.ensemble_compilation.graph_representation import Query, Table
from Models.BN_ensemble_model import BN_ensemble
from DeepDBUtils.ensemble_compilation.probabilistic_query import (
    Expectation,
    IndicatorExpectation,
)
from DataPrepare.query_prepare_BayesCard import (
    factor_refine,
    generate_factors,
    prepare_single_query,
)
from ldbc_full_schema import DEFAULT_DATA_DIR, gen_ldbc_sf1_full_schema
from ldbc_alias_2path_schema import (
    DEFAULT_QUERY_DIR as DEFAULT_2PATH_QUERY_DIR,
    alias_table_name,
    base_name,
    build_alias_2path_schema,
    parse_attr,
    parse_sql_tables_and_joins,
    resolve_alias_relationships,
)


def load_ensemble(schema, model_dir):
    ensemble = BN_ensemble(schema)
    for file_name in sorted(os.listdir(model_dir)):
        if not file_name.endswith(".pkl"):
            continue
        path = os.path.join(model_dir, file_name)
        with open(path, "rb") as handle:
            bn = pickle.load(handle)
        bn.infer_algo = "exact-jit"
        bn.init_inference_method()
        ensemble.bns.append(bn)
        print("loaded {}".format(path))
    if not ensemble.bns:
        raise RuntimeError("No BN models loaded from {}".format(model_dir))
    return ensemble


def split_and(expr):
    return [
        part.strip()
        for part in re.split(r"(?i)\s+and\s+", expr.strip())
        if part.strip()
    ]


def resolve_attr(token, alias_dict):
    token = token.strip()
    match = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\.([A-Za-z_][A-Za-z0-9_]*)$", token)
    if not match:
        raise ValueError("Expected qualified attribute, got {}".format(token))
    alias, attr = match.groups()
    if alias not in alias_dict:
        raise KeyError(alias)
    return alias_dict[alias], attr


def add_join(query, schema, left, right, alias_dict):
    left_table, left_attr = resolve_attr(left, alias_dict)
    right_table, right_attr = resolve_attr(right, alias_dict)
    relationship = "{}.{} = {}.{}".format(left_table, left_attr, right_table, right_attr)
    reverse = "{}.{} = {}.{}".format(right_table, right_attr, left_table, left_attr)
    if relationship in schema.relationship_dictionary:
        query.add_join_condition(relationship)
    elif reverse in schema.relationship_dictionary:
        query.add_join_condition(reverse)
    else:
        left_fk = find_fk_relationship(schema, left_table, left_attr)
        right_fk = find_fk_relationship(schema, right_table, right_attr)
        if (
            left_fk is not None
            and right_fk is not None
            and left_fk.end == right_fk.end
            and left_fk.end_attr == right_fk.end_attr
        ):
            query.add_join_condition(left_fk.identifier)
            query.add_join_condition(right_fk.identifier)
            return
        raise ValueError("No schema relationship for {}".format(relationship))


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
            continue
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
    if physical_rel in physical_schema.relationship_dictionary or physical_rev in physical_schema.relationship_dictionary:
        query.add_join_condition(add_alias_relationship(alias_schema, left_alias, left_attr, right_alias, right_attr))
        return

    left_fk = find_fk_relationship(physical_schema, left_table, left_attr)
    right_fk = find_fk_relationship(physical_schema, right_table, right_attr)
    if left_fk is not None and right_fk is not None and left_fk.end == right_fk.end and left_fk.end_attr == right_fk.end_attr:
        query.add_join_condition(add_alias_relationship(alias_schema, left_alias, left_attr, left_fk.end, left_fk.end_attr))
        query.add_join_condition(add_alias_relationship(alias_schema, right_alias, right_attr, right_fk.end, right_fk.end_attr))
        return

    raise ValueError("No alias schema relationship for {} = {}".format(left, right))


def parse_ldbc_join_sql_alias_safe(query_sql, schema):
    sql = " ".join(query_sql.strip().rstrip(";").split())
    match = re.match(r"(?is)^select\s+count\(\*\)\s+from\s+(.+?)(?:\s+where\s+(.+))?$", sql)
    if not match:
        raise ValueError("Only SELECT COUNT(*) FROM ... [WHERE ...] queries are supported")
    from_part, where_part = match.groups()
    table_pattern = re.compile(
        r"(?is)(?:^|\s+(?:cross\s+join|join)\s+)"
        r"([A-Za-z_][A-Za-z0-9_]*)\s+([A-Za-z_][A-Za-z0-9_]*)"
        r"(?:\s+on\s+(.+?))?"
        r"(?=\s+(?:cross\s+join|join)\s+[A-Za-z_]|$)"
    )
    alias_dict = {}
    on_clauses = []
    for table, alias, on_clause in table_pattern.findall(from_part):
        alias_dict[alias] = table
        if on_clause:
            on_clauses.extend(split_and(on_clause))
    if not alias_dict:
        raise ValueError("No FROM/JOIN table aliases found")

    alias_schema = clone_schema_with_aliases(schema, alias_dict)
    query = Query(alias_schema)
    for alias in alias_dict:
        query.table_set.add(alias)

    for condition in on_clauses:
        join_match = re.match(r"^(.+?)\s*=\s*(.+)$", condition)
        if not join_match:
            raise ValueError("Unsupported ON condition: {}".format(condition))
        add_alias_join(query, alias_schema, schema, join_match.group(1).strip(), join_match.group(2).strip(), alias_dict)

    if where_part:
        for condition in split_and(where_part):
            pred_match = re.match(r"^(.+?)\s*(=|>=|<=|>|<)\s*(.+)$", condition)
            if not pred_match:
                raise ValueError("Unsupported WHERE condition: {}".format(condition))
            left, operator, value = pred_match.groups()
            alias, attr = left.strip().split(".", 1)
            query.add_where_condition(alias, "{}{}{}".format(attr, operator, value.strip()))

    return query, alias_dict


def physical_condition(alias, condition, alias_to_physical):
    return alias_to_physical[alias], condition


class AliasBN(object):
    def __init__(self, physical_bn, table_map, relationship_map, alias_schema):
        self._bn = physical_bn
        self.table_map = table_map
        self.alias_to_physical = {alias: physical for physical, alias in table_map.items()}
        self.schema_graph = alias_schema
        self.table_set = set(table_map.values()) | (set(physical_bn.table_set) - set(table_map.keys()))
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


def build_alias_ensemble(ensemble, query, alias_dict):
    alias_schema = query.schema_graph
    by_physical = {}
    for alias, physical in alias_dict.items():
        by_physical.setdefault(physical, []).append(alias)

    alias_ensemble = copy.copy(ensemble)
    alias_ensemble.schema_graph = alias_schema
    alias_ensemble.bns = []

    for bn in ensemble.bns:
        physical_choices = []
        physical_tables = []
        for table in sorted(bn.table_set):
            if table in by_physical:
                physical_tables.append(table)
                physical_choices.append(by_physical[table] + [table])
        if not physical_choices:
            alias_ensemble.bns.append(bn)
            continue
        for combo in itertools.product(*physical_choices):
            table_map = {physical: alias for physical, alias in zip(physical_tables, combo) if alias != physical}
            relationship_map = {}
            for rel in bn.relationship_set:
                if rel not in ensemble.schema_graph.relationship_dictionary:
                    continue
                obj = ensemble.schema_graph.relationship_dictionary[rel]
                start = table_map.get(obj.start, obj.start)
                end = table_map.get(obj.end, obj.end)
                mapped = "{}.{} = {}.{}".format(start, obj.start_attr, end, obj.end_attr)
                if mapped in alias_schema.relationship_dictionary:
                    relationship_map[rel] = mapped
            alias_ensemble.bns.append(AliasBN(bn, table_map, relationship_map, alias_schema))
    return alias_ensemble



def parse_ldbc_join_sql_2path_alias(query_sql, schema, pattern):
    alias_to_table, joins, where_part = parse_sql_tables_and_joins(query_sql)
    query = Query(schema)
    # Reuse the same resolver used by the training schema so relationship IDs match
    # the 2-path BN relationship lists exactly.
    physical_schema = gen_ldbc_sf1_full_schema(DEFAULT_DATA_DIR)
    relationships = resolve_alias_relationships(schema, physical_schema, pattern, alias_to_table, joins)
    for relationship in relationships:
        query.add_join_condition(relationship)
    for alias in alias_to_table:
        table = alias_table_name(pattern, alias)
        if table in schema.table_dictionary:
            query.table_set.add(table)

    if where_part:
        for condition in split_and(where_part):
            pred_match = re.match(r"^(.+?)\s*(=|>=|<=|>|<)\s*(.+)$", condition)
            if not pred_match:
                raise ValueError("Unsupported WHERE condition: {}".format(condition))
            left, operator, value = pred_match.groups()
            alias, attr = parse_attr(left)
            query.add_where_condition(alias_table_name(pattern, alias), "{}{}{}".format(attr, operator, value.strip()))
    return query, alias_to_table

def parse_ldbc_join_sql(query_sql, schema):
    sql = " ".join(query_sql.strip().rstrip(";").split())
    match = re.match(r"(?is)^select\s+count\(\*\)\s+from\s+(.+?)(?:\s+where\s+(.+))?$", sql)
    if not match:
        raise ValueError("Only SELECT COUNT(*) FROM ... [WHERE ...] queries are supported")

    from_part, where_part = match.groups()
    table_pattern = re.compile(
        r"(?is)(?:^|\s+(?:cross\s+join|join)\s+)"
        r"([A-Za-z_][A-Za-z0-9_]*)\s+([A-Za-z_][A-Za-z0-9_]*)"
        r"(?:\s+on\s+(.+?))?"
        r"(?=\s+(?:cross\s+join|join)\s+[A-Za-z_]|$)"
    )

    alias_dict = {}
    on_clauses = []
    for table, alias, on_clause in table_pattern.findall(from_part):
        alias_dict[alias] = table
        if on_clause:
            on_clauses.extend(split_and(on_clause))
    if not alias_dict:
        raise ValueError("No FROM/JOIN table aliases found")

    query = Query(schema)
    for table in alias_dict.values():
        query.table_set.add(table)

    for condition in on_clauses:
        join_match = re.match(r"^(.+?)\s*=\s*(.+)$", condition)
        if not join_match:
            raise ValueError("Unsupported ON condition: {}".format(condition))
        add_join(query, schema, join_match.group(1), join_match.group(2), alias_dict)

    if where_part:
        for condition in split_and(where_part):
            pred_match = re.match(r"^(.+?)\s*(=|>=|<=|>|<)\s*(.+)$", condition)
            if not pred_match:
                raise ValueError("Unsupported WHERE condition: {}".format(condition))
            left, operator, value = pred_match.groups()
            table, attr = resolve_attr(left, alias_dict)
            query.add_where_condition(table, "{}{}{}".format(attr, operator, value.strip()))

    return query, alias_dict


def build_single_parsed_query_from_query(ensemble, query):
    first_bn, next_mergeable_relationships, next_mergeable_tables = (
        ensemble._greedily_select_first_cardinality_bn(
            query,
            rdc_spn_selection=True,
            rdc_attribute_dict={},
        )
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
        exploit_incoming_multipliers=True,
        prefer_disjunct=False,
    )
    factors = factor_refine(factors)

    parse_result = []
    for factor in factors:
        if isinstance(factor, IndicatorExpectation):
            range_conditions = factor.spn._parse_conditions(
                factor.conditions,
                group_by_columns=None,
                group_by_tuples=None,
            )
            actual_query, fanout = prepare_single_query(range_conditions, factor)
            parse_result.append(
                {
                    "bn_index": ensemble.bns.index(factor.spn),
                    "inverse": factor.inverse,
                    "query": actual_query,
                    "expectation": fanout,
                }
            )
        elif isinstance(factor, Expectation):
            raise NotImplementedError("Expectation factors are not used here")
        else:
            parse_result.append(factor)
    return parse_result


def alias_caveat(alias_dict):
    repeated = sorted(
        table
        for table in set(alias_dict.values())
        if list(alias_dict.values()).count(table) > 1
    )
    if repeated:
        return "repeated_alias_tables={}".format("|".join(repeated))
    return ""


def simplify_parallel_relationships(query):
    simplified = Query(query.schema_graph)
    simplified.table_set = set(query.table_set)
    simplified.table_where_condition_dict = {
        table: list(conditions)
        for table, conditions in query.table_where_condition_dict.items()
    }
    simplified.conditions = list(query.conditions)

    seen_pairs = set()
    for relationship_id in sorted(query.relationship_set):
        relationship = query.schema_graph.relationship_dictionary[relationship_id]
        pair = (relationship.start, relationship.end)
        if pair in seen_pairs:
            continue
        seen_pairs.add(pair)
        simplified.add_join_condition(relationship_id)
    return simplified


def simplify_spanning_relationships(query):
    simplified = Query(query.schema_graph)
    simplified.table_set = set(query.table_set)
    simplified.table_where_condition_dict = {
        table: list(conditions)
        for table, conditions in query.table_where_condition_dict.items()
    }
    simplified.conditions = list(query.conditions)

    parent = {table: table for table in query.table_set}

    def find(table):
        while parent[table] != table:
            parent[table] = parent[parent[table]]
            table = parent[table]
        return table

    def union(left, right):
        left_root = find(left)
        right_root = find(right)
        if left_root == right_root:
            return False
        parent[right_root] = left_root
        return True

    for relationship_id in sorted(query.relationship_set):
        relationship = query.schema_graph.relationship_dictionary[relationship_id]
        if union(relationship.start, relationship.end):
            simplified.add_join_condition(relationship_id)
    return simplified


def supported_condition_columns(ensemble):
    columns = set()
    for bn in ensemble.bns:
        columns.update(getattr(bn, "column_names", []))
    return columns


def drop_unsupported_conditions(query, supported_columns):
    cleaned = Query(query.schema_graph)
    cleaned.table_set = set(query.table_set)
    cleaned.relationship_set = set(query.relationship_set)
    ignored = []
    for table, conditions in query.table_where_condition_dict.items():
        for condition in conditions:
            attr_match = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)", condition)
            if attr_match and "{}.{}".format(table, attr_match.group(1)) in supported_columns:
                cleaned.add_where_condition(table, condition)
            else:
                ignored.append("{}:{}".format(table, condition))
    return cleaned, ignored


def decode_and_cardinality(ensemble, parsed_query, none_as_unconditional=False):
    decoded_query = ensemble.parse_query_all([parsed_query])[0]
    has_none = any(
        isinstance(part, dict) and part.get("query") is None
        for part in decoded_query[1:]
    )
    if has_none and not none_as_unconditional:
        raise ValueError("Decoded query contains None")
    if has_none:
        for part in decoded_query[1:]:
            if isinstance(part, dict) and part.get("query") is None:
                part["query"] = {}
                part["n_distinct"] = None
    return ensemble.cardinality(decoded_query)


def estimate_query_with_fallbacks(ensemble, query, supported_columns):
    attempts = [
        ("exact_relationships", query, [], False),
        (
            "simplified_parallel_relationships",
            simplify_parallel_relationships(query),
            [],
            False,
        ),
        (
            "spanning_tree_relationships",
            simplify_spanning_relationships(query),
            [],
            False,
        ),
    ]
    cleaned, ignored = drop_unsupported_conditions(query, supported_columns)
    attempts.append(
        (
            "simplified_parallel_relationships_without_unsupported_predicates",
            simplify_parallel_relationships(cleaned),
            ignored,
            False,
        )
    )
    attempts.append(
        (
            "spanning_tree_relationships_without_unsupported_predicates",
            simplify_spanning_relationships(cleaned),
            ignored,
            False,
        )
    )
    attempts.append(
        (
            "spanning_tree_without_unsupported_predicates_none_as_unconditional",
            simplify_spanning_relationships(cleaned),
            ignored,
            True,
        )
    )

    failures = []
    for mode, attempt_query, ignored_conditions, none_as_unconditional in attempts:
        try:
            parsed_query = build_single_parsed_query_from_query(ensemble, attempt_query)
            prediction = decode_and_cardinality(
                ensemble,
                parsed_query,
                none_as_unconditional=none_as_unconditional,
            )
            if prediction is None or prediction <= 1:
                prediction = 1
            return prediction, mode, ignored_conditions, failures
        except Exception as exc:
            failures.append("{} failed: {}: {}".format(mode, type(exc).__name__, exc))
    raise RuntimeError(" | ".join(failures))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-dir", default=DEFAULT_DATA_DIR)
    parser.add_argument("--model-dir", default=os.path.join(REPRO_DIR, "models_2path_alias_local"))
    parser.add_argument(
        "--query-dir",
        default="/home/zxz/miniGU/experiment/pattern_sql/ldbc_with_pred",
    )
    parser.add_argument(
        "--alias-2path",
        action="store_true",
        help="Use alias-aware 2-path schema and parse aliases as logical table instances.",
    )
    parser.add_argument(
        "--output",
        default=os.path.join(REPRO_DIR, "results", "ldbc_with_pred_estimates.csv"),
    )
    args = parser.parse_args()

    os.makedirs(os.path.dirname(args.output), exist_ok=True)

    if args.alias_2path:
        schema, _, _ = build_alias_2path_schema(args.query_dir, args.data_dir)
    else:
        schema = gen_ldbc_sf1_full_schema(args.data_dir)
    ensemble = load_ensemble(schema, args.model_dir)
    supported_columns = supported_condition_columns(ensemble)

    latencies = []
    with open(args.output, "w", newline="") as out:
        writer = csv.writer(out)
        writer.writerow(
            [
                "query_file",
                "prediction",
                "latency_ms",
                "relationships",
                "where_conditions",
                "status",
                "notes",
                "mode",
                "ignored_conditions",
            ]
        )
        for file_name in sorted(os.listdir(args.query_dir)):
            if not file_name.endswith(".sql"):
                continue
            path = os.path.join(args.query_dir, file_name)
            query_sql = open(path, encoding="utf-8").read()
            tic = time.time()
            relationships = ""
            where_conditions = ""
            notes = ""
            mode = ""
            ignored_conditions = []
            try:
                if args.alias_2path:
                    base_query, aliases = parse_ldbc_join_sql_2path_alias(query_sql, schema, base_name(file_name))
                else:
                    base_query, aliases = parse_ldbc_join_sql(query_sql, schema)
                notes = alias_caveat(aliases)
                query = base_query
                query_ensemble = ensemble
                query_supported_columns = supported_columns
                relationships = ";".join(sorted(query.relationship_set))
                where_conditions = ";".join(
                    "{}:{}".format(table, "|".join(conds))
                    for table, conds in sorted(query.table_where_condition_dict.items())
                )
                prediction, mode, ignored_conditions, failures = estimate_query_with_fallbacks(
                    query_ensemble, query, query_supported_columns
                )
                if mode == "exact_relationships":
                    status = "ok"
                else:
                    status = "ok_approx"
                    if failures:
                        notes = ";".join([note for note in [notes] + failures if note])
            except Exception as exc:
                prediction = ""
                status = "failed: {}: {}".format(type(exc).__name__, exc)
                print(traceback.format_exc())
            latency_ms = (time.time() - tic) * 1000.0
            if status == "ok":
                latencies.append(latency_ms)
            writer.writerow(
                [
                    file_name,
                    prediction,
                    latency_ms,
                    relationships,
                    where_conditions,
                    status,
                    notes,
                    mode,
                    "|".join(ignored_conditions),
                ]
            )
            print(
                "{} pred={} latency_ms={} status={} mode={} ignored={} notes={}".format(
                    file_name,
                    prediction,
                    latency_ms,
                    status,
                    mode,
                    "|".join(ignored_conditions),
                    notes,
                )
            )

    if latencies:
        print("latency p50={} ms".format(np.percentile(latencies, 50)))
    print("results written to {}".format(args.output))


if __name__ == "__main__":
    main()
