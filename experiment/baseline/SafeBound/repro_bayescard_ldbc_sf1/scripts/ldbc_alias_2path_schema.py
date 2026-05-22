import csv
import json
import os
import re
import sys

REPRO_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SAFEB0UND_BAYESCARD = os.path.abspath(os.path.join(REPRO_DIR, "..", "bayescard"))
if SAFEB0UND_BAYESCARD not in sys.path:
    sys.path.insert(0, SAFEB0UND_BAYESCARD)

from Schemas.graph_representation import SchemaGraph, Table
from ldbc_full_schema import DEFAULT_DATA_DIR, gen_ldbc_sf1_full_schema

DEFAULT_QUERY_DIR = "/home/zxz/miniGU_experiment_master/experiment/result/sql_baselines/lsqb_glogs_with_pred_10_unique/flat_sql"
BASE_RE = re.compile(r"^(.+)_\d+$")


def base_name(file_name):
    stem = os.path.splitext(os.path.basename(file_name))[0]
    match = BASE_RE.match(stem)
    return match.group(1) if match else stem


def split_and(expr):
    return [part.strip() for part in re.split(r"(?i)\s+and\s+", expr.strip()) if part.strip()]


def parse_sql_tables_and_joins(query_sql):
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
    alias_to_table = {}
    joins = []
    for table, alias, on_clause in table_pattern.findall(from_part):
        alias_to_table[alias] = table
        if on_clause:
            for condition in split_and(on_clause):
                join_match = re.match(r"^(.+?)\s*=\s*(.+)$", condition)
                if join_match:
                    joins.append((join_match.group(1).strip(), join_match.group(2).strip()))
    return alias_to_table, joins, where_part


def parse_attr(token):
    match = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\.([A-Za-z_][A-Za-z0-9_]*)$", token.strip())
    if not match:
        raise ValueError("Expected qualified attribute, got {}".format(token))
    return match.groups()


def clone_table(schema, physical_table, alias_table):
    src = schema.table_dictionary[physical_table]
    return Table(
        alias_table,
        primary_key=list(src.primary_key),
        table_size=src.table_size,
        csv_file_location=src.csv_file_location,
        attributes=list(src.attributes or []),
        irrelevant_attributes=list(getattr(src, "irrelevant_attributes", []) or []),
        keep_fk_attributes=list(getattr(src, "keep_fk_attributes", []) or []),
        sample_rate=src.sample_rate,
        fd_list=[],
        no_compression=list(getattr(src, "no_compression", []) or []),
    )


def copy_physical_table(dst_schema, src_schema, table):
    if table not in dst_schema.table_dictionary:
        dst_schema.add_table(clone_table(src_schema, table, table))


def find_fk_relationship(schema, table, attr):
    for relationship in schema.relationships:
        if relationship.start == table and relationship.start_attr == attr:
            return relationship
    return None


def add_relationship_once(schema, start, start_attr, end, end_attr):
    identifier = "{}.{} = {}.{}".format(start, start_attr, end, end_attr)
    reverse = "{}.{} = {}.{}".format(end, end_attr, start, start_attr)
    if identifier in schema.relationship_dictionary:
        return identifier
    if reverse in schema.relationship_dictionary:
        return reverse
    return schema.add_relationship(start, start_attr, end, end_attr)


def alias_table_name(pattern, alias):
    return "{}__{}".format(pattern, alias)


def implicit_table_name(pattern, physical_table):
    return "{}__implicit__{}".format(pattern, physical_table)


def resolve_alias_relationships(alias_schema, physical_schema, pattern, alias_to_table, joins):
    relationships = []
    for left, right in joins:
        left_alias, left_attr = parse_attr(left)
        right_alias, right_attr = parse_attr(right)
        left_physical = alias_to_table[left_alias]
        right_physical = alias_to_table[right_alias]
        left_table = alias_table_name(pattern, left_alias)
        right_table = alias_table_name(pattern, right_alias)

        direct = "{}.{} = {}.{}".format(left_physical, left_attr, right_physical, right_attr)
        reverse = "{}.{} = {}.{}".format(right_physical, right_attr, left_physical, left_attr)
        if direct in physical_schema.relationship_dictionary:
            relationships.append(add_relationship_once(alias_schema, left_table, left_attr, right_table, right_attr))
            continue
        if reverse in physical_schema.relationship_dictionary:
            relationships.append(add_relationship_once(alias_schema, right_table, right_attr, left_table, left_attr))
            continue

        left_fk = find_fk_relationship(physical_schema, left_physical, left_attr)
        right_fk = find_fk_relationship(physical_schema, right_physical, right_attr)
        if left_fk is not None and right_fk is not None and left_fk.end == right_fk.end and left_fk.end_attr == right_fk.end_attr:
            target_table = implicit_table_name(pattern, left_fk.end)
            if target_table not in alias_schema.table_dictionary:
                alias_schema.add_table(clone_table(physical_schema, left_fk.end, target_table))
            relationships.append(add_relationship_once(alias_schema, left_table, left_attr, target_table, left_fk.end_attr))
            relationships.append(add_relationship_once(alias_schema, right_table, right_attr, target_table, right_fk.end_attr))
            continue

        raise ValueError("No schema relationship for {} = {}".format(left, right))
    return relationships


def connected_2paths(schema, relationships):
    result = []
    rels = sorted(set(relationships))
    for i, left in enumerate(rels):
        left_obj = schema.relationship_dictionary[left]
        left_tables = {left_obj.start, left_obj.end}
        for right in rels[i + 1:]:
            right_obj = schema.relationship_dictionary[right]
            tables = left_tables | {right_obj.start, right_obj.end}
            if left_tables.intersection({right_obj.start, right_obj.end}) and len(tables) == 3:
                result.append((left, right))
    return result


def build_alias_2path_schema(query_dir=DEFAULT_QUERY_DIR, data_dir=DEFAULT_DATA_DIR):
    physical_schema = gen_ldbc_sf1_full_schema(data_dir)
    alias_schema = SchemaGraph()
    work_items = []
    parsed_patterns = {}

    for file_name in sorted(os.listdir(query_dir)):
        if not file_name.endswith(".sql"):
            continue
        pattern = base_name(file_name)
        if pattern in parsed_patterns:
            continue
        query_sql = open(os.path.join(query_dir, file_name), encoding="utf-8").read()
        alias_to_table, joins, _ = parse_sql_tables_and_joins(query_sql)
        parsed_patterns[pattern] = (alias_to_table, joins)

        for alias, physical in sorted(alias_to_table.items()):
            table_name = alias_table_name(pattern, alias)
            if table_name not in alias_schema.table_dictionary:
                alias_schema.add_table(clone_table(physical_schema, physical, table_name))

        relationships = resolve_alias_relationships(alias_schema, physical_schema, pattern, alias_to_table, joins)
        for left, right in connected_2paths(alias_schema, relationships):
            work_items.append({"pattern": pattern, "relationships": [left, right]})

    return alias_schema, work_items, parsed_patterns


def write_work_items(work_items, output_path):
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    with open(output_path, "w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(["index", "pattern", "relationship_1", "relationship_2"])
        for index, item in enumerate(work_items):
            writer.writerow([index, item["pattern"], item["relationships"][0], item["relationships"][1]])


def read_work_items(path):
    items = []
    with open(path, newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            items.append({
                "index": int(row["index"]),
                "pattern": row["pattern"],
                "relationships": [row["relationship_1"], row["relationship_2"]],
            })
    return items


if __name__ == "__main__":
    schema, work_items, patterns = build_alias_2path_schema()
    print("patterns={}".format(len(patterns)))
    print("tables={}".format(len(schema.tables)))
    print("relationships={}".format(len(schema.relationships)))
    print("2path_items={}".format(len(work_items)))
