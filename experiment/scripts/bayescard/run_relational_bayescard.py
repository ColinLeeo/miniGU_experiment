#!/usr/bin/env python
"""Run BayesCard on the original relational JOB-light and STATS schemas.

This intentionally bypasses the graph-pattern wrappers.  It reuses the
BayesCard code vendored under baseline/SafeBound/bayescard.
"""

from __future__ import print_function

import argparse
import csv
import json
import math
import os
import pickle
import shutil
import sys
import time

import numpy as np
import pandas as pd


SCRIPT_PATH = os.path.abspath(__file__)
EXPERIMENT_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SAFEBOUND_DIR = os.path.join(EXPERIMENT_DIR, "baseline", "SafeBound")
BAYESCARD_DIR = os.path.join(SAFEBOUND_DIR, "bayescard")
if BAYESCARD_DIR not in sys.path:
    sys.path.insert(0, BAYESCARD_DIR)
os.chdir(BAYESCARD_DIR)

import pgympy
sys.modules["Pgmpy"] = pgympy

from DataPrepare.join_data_preparation import JoinDataPreparator
from DataPrepare.prepare_single_tables import prepare_all_tables
import DataPrepare.prepare_single_tables as prepare_single_tables
from DataPrepare.query_prepare_BayesCard import factor_refine, generate_factors, prepare_single_query
from DeepDBUtils.ensemble_compilation.probabilistic_query import Expectation, IndicatorExpectation
from DeepDBUtils.evaluation.utils import parse_query, timestamp_transorform
from Models.BN_ensemble_model import BN_ensemble
from Models.Bayescard_BN import Bayescard_BN, build_meta_info
from Schemas.graph_representation import SchemaGraph, Table
from Schemas.imdb.schema import gen_job_light_imdb_schema
from Schemas.stats.schema import gen_stats_light_schema

Bayescard_BN.legitimacy_check = lambda self: None


DEFAULTS = {
    "job-light": {
        "csv_template": os.path.join(EXPERIMENT_DIR, "datasets", "imdb", "imdb_rel", "{}.csv"),
        "query_file": os.path.join(SAFEBOUND_DIR, "Workloads", "JOBLightQueriesBayes.sql"),
        "hdf_dir": os.path.join(EXPERIMENT_DIR, "catalogs", "imdb", "bayescard_relational_joblight_hdf"),
        "model_dir": os.path.join(EXPERIMENT_DIR, "catalogs", "imdb", "bayescard_relational_joblight_models"),
        "out_dir": os.path.join(EXPERIMENT_DIR, "result", "sql_baselines", "bayescard_relational", "job_light"),
    },
    "imdb-no-pred": {
        "csv_template": os.path.join(EXPERIMENT_DIR, "datasets", "imdb", "imdb_rel", "{}.csv"),
        "query_file": os.path.join(EXPERIMENT_DIR, "result", "sql_baselines", "bayescard_relational", "imdb_no_pred_q1_q28", "imdb_no_pred_q1_q28_bayes.sql"),
        "hdf_dir": os.path.join(EXPERIMENT_DIR, "catalogs", "imdb", "bayescard_relational_imdb_no_pred_q1_q28_hdf"),
        "model_dir": os.path.join(EXPERIMENT_DIR, "catalogs", "imdb", "bayescard_relational_imdb_no_pred_q1_q28_models"),
        "out_dir": os.path.join(EXPERIMENT_DIR, "result", "sql_baselines", "bayescard_relational", "imdb_no_pred_q1_q28"),
    },
    "stats": {
        "csv_template": os.path.join(SAFEBOUND_DIR, "Data", "Stats", "{}.csv"),
        "query_file": os.path.join(SAFEBOUND_DIR, "Workloads", "StatsQueriesBayes.sql"),
        "hdf_dir": os.path.join(EXPERIMENT_DIR, "catalogs", "stats_ceb", "bayescard_relational_hdf"),
        "model_dir": os.path.join(EXPERIMENT_DIR, "catalogs", "stats_ceb", "bayescard_relational_models"),
        "out_dir": os.path.join(EXPERIMENT_DIR, "result", "sql_baselines", "bayescard_relational", "stats"),
    },
}

STATS_DATE_COLUMNS = {
    "badges": ["Date"],
    "votes": ["CreationDate"],
    "postHistory": ["CreationDate"],
    "posts": ["CreationDate"],
    "users": ["CreationDate"],
    "comments": ["CreationDate"],
    "postLinks": ["CreationDate"],
}

IMDB_NO_PRED_PATTERN_DIR = os.path.join(EXPERIMENT_DIR, "patterns", "gcard", "imdb")
IMDB_NO_PRED_TRUTH_FILE = os.path.join(
    EXPERIMENT_DIR,
    "result",
    "sql_baselines",
    "imdb_q1_q28_7baselines",
    "imdb_q1_q28_7baselines_qerror_latency.csv",
)
IMDB_NO_PRED_QUERY_IDS = list(range(1, 29))


def attrs_without(all_attrs, keep_attrs):
    keep = set(keep_attrs)
    return [attr for attr in all_attrs if attr not in keep]


def add_imdb_table(schema, name, attrs, keep_attrs, csv_template, table_size):
    schema.add_table(
        Table(
            name,
            attributes=attrs,
            irrelevant_attributes=attrs_without(attrs, keep_attrs),
            csv_file_location=csv_template.format(name),
            table_size=table_size,
        )
    )


def gen_imdb_no_pred_schema(csv_template):
    """Full-IMDB subset needed by the q1-q28 no-predicate graph patterns."""
    schema = SchemaGraph()

    add_imdb_table(
        schema,
        "title",
        ["id", "title", "imdb_index", "kind_id", "production_year", "imdb_id", "phonetic_code", "episode_of_id", "season_nr", "episode_nr", "series_years", "md5sum"],
        ["id"],
        csv_template,
        2528312,
    )
    add_imdb_table(schema, "movie_info_idx", ["id", "movie_id", "info_type_id", "info", "note"], ["id", "movie_id"], csv_template, 1380035)
    add_imdb_table(schema, "movie_info", ["id", "movie_id", "info_type_id", "info", "note"], ["id", "movie_id"], csv_template, 14835720)
    add_imdb_table(schema, "cast_info", ["id", "person_id", "movie_id", "person_role_id", "note", "nr_order", "role_id"], ["id", "person_id", "movie_id", "person_role_id"], csv_template, 36244344)
    add_imdb_table(schema, "movie_keyword", ["id", "movie_id", "keyword_id"], ["id", "movie_id", "keyword_id"], csv_template, 4523930)
    add_imdb_table(schema, "movie_companies", ["id", "movie_id", "company_id", "company_type_id", "note"], ["id", "movie_id", "company_id"], csv_template, 2609129)
    add_imdb_table(schema, "company_name", ["id", "name", "country_code", "imdb_id", "name_pcode_nf", "name_pcode_sf", "md5sum"], ["id"], csv_template, 234997)
    add_imdb_table(schema, "keyword", ["id", "keyword", "phonetic_code"], ["id"], csv_template, 134170)
    add_imdb_table(schema, "name", ["id", "name", "imdb_index", "imdb_id", "gender", "name_pcode_cf", "name_pcode_nf", "surname_pcode", "md5sum"], ["id"], csv_template, 4167491)
    add_imdb_table(schema, "aka_name", ["id", "person_id", "name", "imdb_index", "name_pcode_cf", "name_pcode_nf", "surname_pcode", "md5sum"], ["id", "person_id"], csv_template, 901343)
    add_imdb_table(schema, "char_name", ["id", "name", "imdb_index", "imdb_id", "name_pcode_nf", "surname_pcode", "md5sum"], ["id"], csv_template, 3140339)
    add_imdb_table(schema, "person_info", ["id", "person_id", "info_type_id", "info", "note"], ["id", "person_id"], csv_template, 2963664)
    add_imdb_table(schema, "complete_cast", ["id", "movie_id", "subject_id", "status_id"], ["id", "movie_id"], csv_template, 135086)
    add_imdb_table(schema, "aka_title", ["id", "movie_id", "title", "imdb_index", "kind_id", "production_year", "phonetic_code", "episode_of_id", "season_nr", "episode_nr", "note", "md5sum"], ["id", "movie_id"], csv_template, 361472)
    add_imdb_table(schema, "movie_link", ["id", "movie_id", "linked_movie_id", "link_type_id"], ["id", "movie_id", "linked_movie_id"], csv_template, 29997)

    schema.add_relationship("movie_info_idx", "movie_id", "title", "id")
    schema.add_relationship("movie_info", "movie_id", "title", "id")
    schema.add_relationship("cast_info", "movie_id", "title", "id")
    schema.add_relationship("cast_info", "person_id", "name", "id")
    schema.add_relationship("cast_info", "person_role_id", "char_name", "id")
    schema.add_relationship("movie_keyword", "movie_id", "title", "id")
    schema.add_relationship("movie_keyword", "keyword_id", "keyword", "id")
    schema.add_relationship("movie_companies", "movie_id", "title", "id")
    schema.add_relationship("movie_companies", "company_id", "company_name", "id")
    schema.add_relationship("aka_name", "person_id", "name", "id")
    schema.add_relationship("person_info", "person_id", "name", "id")
    schema.add_relationship("complete_cast", "movie_id", "title", "id")
    schema.add_relationship("aka_title", "movie_id", "title", "id")
    schema.add_relationship("movie_link", "movie_id", "title", "id")
    schema.add_relationship("movie_link", "linked_movie_id", "title", "id")
    return schema


IMDB_VERTEX_TABLE = {
    "title": "title",
    "companyname": "company_name",
    "infoidxvertex": "movie_info_idx",
    "infovertex": "movie_info",
    "keyword": "keyword",
    "castinfovertex": "cast_info",
    "person": "name",
    "akaname": "aka_name",
    "character": "char_name",
    "personinfovertex": "person_info",
    "complcastinfovertex": "complete_cast",
    "akatitle": "aka_title",
}

IMDB_ALIAS_PREFIX = {
    "title": "t",
    "company_name": "cn",
    "movie_info_idx": "miidx",
    "movie_info": "mi",
    "keyword": "k",
    "cast_info": "ci",
    "name": "n",
    "aka_name": "an",
    "char_name": "chn",
    "person_info": "pi",
    "complete_cast": "cc",
    "aka_title": "at",
    "movie_companies": "mc",
    "movie_keyword": "mk",
    "movie_link": "ml",
}


def endpoint_alias(edge, vertices, aliases, label):
    candidates = []
    for key in ("src", "dst"):
        vertex_id = edge[key]
        if vertices[vertex_id] == label:
            candidates.append(aliases[vertex_id])
    if len(candidates) != 1:
        raise ValueError("Expected exactly one {} endpoint for edge {}".format(label, edge))
    return candidates[0]


def graph_pattern_to_imdb_sql(pattern_path):
    with open(pattern_path, encoding="utf-8") as handle:
        pattern = json.load(handle)
    if pattern.get("predicates"):
        raise ValueError("Expected no-predicate pattern: {}".format(pattern_path))

    vertices = {int(vertex["id"]): vertex["label"].lower() for vertex in pattern["vertices"]}
    aliases = {}
    from_items = []
    for vertex_id in sorted(vertices):
        table = IMDB_VERTEX_TABLE[vertices[vertex_id]]
        alias = "{}{}".format(IMDB_ALIAS_PREFIX[table], vertex_id)
        aliases[vertex_id] = alias
        from_items.append((table, alias))

    conditions = []
    for edge in sorted(pattern["edges"], key=lambda item: int(item["id"])):
        label = edge["label"].lower()
        edge_id = int(edge["id"])
        src_alias = aliases[int(edge["src"])]
        dst_alias = aliases[int(edge["dst"])]
        if label == "title_moviecompanies_companyname":
            edge_alias = "mc_e{}".format(edge_id)
            from_items.append(("movie_companies", edge_alias))
            title_alias = endpoint_alias(edge, vertices, aliases, "title")
            company_alias = endpoint_alias(edge, vertices, aliases, "companyname")
            conditions.extend(["{}.movie_id = {}.id".format(edge_alias, title_alias), "{}.company_id = {}.id".format(edge_alias, company_alias)])
        elif label == "title_keywordedge_keyword":
            edge_alias = "mk_e{}".format(edge_id)
            from_items.append(("movie_keyword", edge_alias))
            title_alias = endpoint_alias(edge, vertices, aliases, "title")
            keyword_alias = endpoint_alias(edge, vertices, aliases, "keyword")
            conditions.extend(["{}.movie_id = {}.id".format(edge_alias, title_alias), "{}.keyword_id = {}.id".format(edge_alias, keyword_alias)])
        elif label == "title_infoedge_infoidxvertex":
            title_alias = endpoint_alias(edge, vertices, aliases, "title")
            info_alias = endpoint_alias(edge, vertices, aliases, "infoidxvertex")
            conditions.append("{}.movie_id = {}.id".format(info_alias, title_alias))
        elif label == "title_infoedge_infovertex":
            title_alias = endpoint_alias(edge, vertices, aliases, "title")
            info_alias = endpoint_alias(edge, vertices, aliases, "infovertex")
            conditions.append("{}.movie_id = {}.id".format(info_alias, title_alias))
        elif label == "castinfovertex_castinfoedge_title":
            cast_alias = endpoint_alias(edge, vertices, aliases, "castinfovertex")
            title_alias = endpoint_alias(edge, vertices, aliases, "title")
            conditions.append("{}.movie_id = {}.id".format(cast_alias, title_alias))
        elif label == "castinfovertex_castinfoedge_person":
            cast_alias = endpoint_alias(edge, vertices, aliases, "castinfovertex")
            person_alias = endpoint_alias(edge, vertices, aliases, "person")
            conditions.append("{}.person_id = {}.id".format(cast_alias, person_alias))
        elif label == "castinfovertex_castinfoedge_character":
            cast_alias = endpoint_alias(edge, vertices, aliases, "castinfovertex")
            character_alias = endpoint_alias(edge, vertices, aliases, "character")
            conditions.append("{}.person_role_id = {}.id".format(cast_alias, character_alias))
        elif label == "person_akanameedge_akaname":
            person_alias = endpoint_alias(edge, vertices, aliases, "person")
            aka_alias = endpoint_alias(edge, vertices, aliases, "akaname")
            conditions.append("{}.person_id = {}.id".format(aka_alias, person_alias))
        elif label == "person_personinfoedge_personinfovertex":
            person_alias = endpoint_alias(edge, vertices, aliases, "person")
            info_alias = endpoint_alias(edge, vertices, aliases, "personinfovertex")
            conditions.append("{}.person_id = {}.id".format(info_alias, person_alias))
        elif label == "complcastinfovertex_complcastinfoedge_title":
            cast_alias = endpoint_alias(edge, vertices, aliases, "complcastinfovertex")
            title_alias = endpoint_alias(edge, vertices, aliases, "title")
            conditions.append("{}.movie_id = {}.id".format(cast_alias, title_alias))
        elif label == "title_akatitleedge_akatitle":
            title_alias = endpoint_alias(edge, vertices, aliases, "title")
            aka_title_alias = endpoint_alias(edge, vertices, aliases, "akatitle")
            conditions.append("{}.movie_id = {}.id".format(aka_title_alias, title_alias))
        elif label == "title_linktypeedge_title":
            edge_alias = "ml_e{}".format(edge_id)
            from_items.append(("movie_link", edge_alias))
            conditions.extend(["{}.movie_id = {}.id".format(edge_alias, src_alias), "{}.linked_movie_id = {}.id".format(edge_alias, dst_alias)])
        else:
            raise ValueError("Unsupported IMDB graph edge label {} in {}".format(label, pattern_path))

    from_sql = ", ".join("{} {}".format(table, alias) for table, alias in from_items)
    where_sql = " AND ".join(conditions)
    return "SELECT COUNT(*) FROM {} WHERE {}".format(from_sql, where_sql)


def read_imdb_no_pred_truths(path):
    truths = {}
    with open(path, newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            truths[row["query"]] = row["true"]
    return truths


def build_imdb_no_pred_workload(query_file):
    ensure_dir(os.path.dirname(query_file))
    truths = read_imdb_no_pred_truths(IMDB_NO_PRED_TRUTH_FILE)
    rows = []
    for query_id in IMDB_NO_PRED_QUERY_IDS:
        query_name = "q{}".format(query_id)
        pattern_path = os.path.join(IMDB_NO_PRED_PATTERN_DIR, query_name + ".json")
        rows.append((query_name, graph_pattern_to_imdb_sql(pattern_path), truths[query_name]))
    with open(query_file, "w", encoding="utf-8") as handle:
        for _query_name, sql, truth in rows:
            handle.write("{}||{}\n".format(sql, int(float(truth))))
    return rows


def ensure_dir(path):
    if not os.path.exists(path):
        os.makedirs(path)


def qerror(est, truth):
    if est is None or not math.isfinite(float(est)):
        return float("nan")
    est = float(est)
    truth = float(truth)
    if est <= 1:
        est = 1.0
    if truth <= 0:
        truth = 1.0
    return max(est / truth, truth / est)


def percentile(values, p):
    values = sorted(v for v in values if math.isfinite(v))
    if not values:
        return float("nan")
    if len(values) == 1:
        return values[0]
    pos = (len(values) - 1) * p
    lo = int(math.floor(pos))
    hi = int(math.ceil(pos))
    if lo == hi:
        return values[lo]
    frac = pos - lo
    return values[lo] * (1.0 - frac) + values[hi] * frac


def write_csv(path, rows, fieldnames):
    ensure_dir(os.path.dirname(path))
    with open(path, "w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        for row in rows:
            writer.writerow({field: row.get(field, "") for field in fieldnames})


def parse_true_and_sql(line):
    line = line.strip()
    parts = line.split("||")
    if len(parts) == 2:
        left, right = parts
        try:
            return int(left), right.strip()
        except ValueError:
            return int(right), left.strip()
    if len(parts) == 3:
        return int(parts[-1]), parts[1].strip()
    raise ValueError("Unsupported workload line: {}".format(line[:120]))


def schema_for(dataset, csv_template):
    if dataset == "job-light":
        return gen_job_light_imdb_schema(csv_template)
    if dataset == "imdb-no-pred":
        return gen_imdb_no_pred_schema(csv_template)
    if dataset == "stats":
        return gen_stats_light_schema(csv_template)
    raise ValueError("Unsupported dataset {}".format(dataset))


def patch_stats_csv_reader():
    original = prepare_single_tables.read_table_csv

    def read_table_csv_stats(table_obj, csv_seperator=",", stats=False):
        frame = pd.read_csv(table_obj.csv_file_location)
        for column in STATS_DATE_COLUMNS.get(table_obj.table_name, []):
            if column in frame.columns:
                frame[column] = frame[column].apply(
                    lambda value: timestamp_transorform("'" + str(value) + "'")
                    if pd.notna(value) and str(value) != ""
                    else value
                )
        frame.columns = [table_obj.table_name + "." + attr for attr in table_obj.attributes]
        for attribute in table_obj.irrelevant_attributes:
            col = table_obj.table_name + "." + attribute
            if col in frame.columns:
                frame = frame.drop(col, axis=1)
        return frame.apply(pd.to_numeric, errors="ignore")

    prepare_single_tables.read_table_csv = read_table_csv_stats
    return original


def prepare_hdf(args):
    if args.dataset == "stats":
        patch_stats_csv_reader()
    schema = schema_for(args.dataset, args.csv_template)
    if args.force and os.path.exists(args.hdf_dir):
        shutil.rmtree(args.hdf_dir)
    ensure_dir(args.hdf_dir)
    prepare_all_tables(
        schema,
        args.hdf_dir,
        csv_seperator=args.csv_separator,
        max_table_data=args.max_rows_per_hdf_file,
    )
    print("HDF written to {}".format(args.hdf_dir))


def train(args):
    schema = schema_for(args.dataset, args.csv_template)
    ensure_dir(args.model_dir)
    prep = JoinDataPreparator(os.path.join(args.hdf_dir, "meta_data.pkl"), schema, max_table_data=args.max_rows_per_hdf_file)
    relationships = list(schema.relationships)
    start = args.start_index
    end = args.end_index if args.end_index is not None else len(relationships)
    print("Training relationships [{}:{}) out of {}".format(start, end, len(relationships)))
    for index, relationship_obj in enumerate(relationships[start:end], start=start):
        relation = [relationship_obj.identifier]
        model_path = os.path.join(args.model_dir, "{}_{}_{}.pkl".format(index, args.learning_algo, args.max_parents))
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
            algorithm=args.learning_algo,
            max_parents=args.max_parents,
            ignore_cols=["id"],
            sample_size=args.sample_size,
        )
        with open(model_path, "wb") as handle:
            pickle.dump(bn, handle, pickle.HIGHEST_PROTOCOL)
        print("Saved {}".format(model_path))


def load_ensemble(schema, model_dir):
    ensemble = BN_ensemble(schema)
    files = [name for name in os.listdir(model_dir) if name.endswith(".pkl")]
    for name in sorted(files):
        with open(os.path.join(model_dir, name), "rb") as handle:
            bn = pickle.load(handle)
        bn.infer_algo = "exact-jit"
        bn.init_inference_method()
        ensemble.bns.append(bn)
    if not ensemble.bns:
        raise RuntimeError("No .pkl BayesCard models found in {}".format(model_dir))
    return ensemble


def compile_query(ensemble, schema, query_sql):
    query = parse_query(query_sql.strip(), schema)
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
        exploit_incoming_multipliers=True,
        prefer_disjunct=False,
    )
    factors = factor_refine(factors)
    compiled = []
    for factor in factors:
        if isinstance(factor, IndicatorExpectation):
            ranges = factor.spn._parse_conditions(factor.conditions, group_by_columns=None, group_by_tuples=None)
            actual_query, fanout = prepare_single_query(ranges, factor)
            compiled.append(
                {
                    "bn_index": ensemble.bns.index(factor.spn),
                    "inverse": factor.inverse,
                    "query": actual_query,
                    "expectation": fanout,
                }
            )
        elif isinstance(factor, Expectation):
            raise NotImplementedError("Expectation factors are not implemented by this BayesCard adapter")
        else:
            compiled.append(factor)
    return compiled


def evaluate(args):
    if args.dataset == "imdb-no-pred":
        build_imdb_no_pred_workload(args.query_file)
    schema = schema_for(args.dataset, args.csv_template)
    ensemble = load_ensemble(schema, args.model_dir)
    rows = []
    with open(args.query_file) as handle:
        workload = [line for line in handle if line.strip()]

    for query_no, line in enumerate(workload):
        truth, query_sql = parse_true_and_sql(line)
        if args.limit is not None and query_no >= args.limit:
            break
        print("Evaluating {} query {}/{}".format(args.dataset, query_no + 1, len(workload)))
        start = time.time()
        try:
            compiled = compile_query(ensemble, schema, query_sql)
            decoded = ensemble.parse_query_all([compiled])[0]
            prediction = ensemble.cardinality(decoded)
            if isinstance(prediction, np.ndarray):
                prediction = prediction[0]
            latency_s = time.time() - start
            qe = qerror(prediction, truth)
            status = "ok"
        except Exception as exc:
            prediction = 1.0
            latency_s = time.time() - start
            qe = qerror(prediction, truth)
            status = "failed: {}: {}".format(type(exc).__name__, exc)
        query_name = "q{}".format(query_no + 1) if args.dataset == "imdb-no-pred" else str(query_no)
        rows.append(
            {
                "dataset": args.dataset,
                "query_no": query_no,
                "query": query_name,
                "true_cardinality": truth,
                "prediction": prediction,
                "latency_s": latency_s,
                "qerror": qe,
                "status": status,
                "query_sql": query_sql,
            }
        )

    ensure_dir(args.out_dir)
    detail_path = os.path.join(args.out_dir, "{}_bayescard_relational_detail.csv".format(args.dataset.replace("-", "_")))
    write_csv(
        detail_path,
        rows,
        ["dataset", "query_no", "query", "true_cardinality", "prediction", "latency_s", "qerror", "status", "query_sql"],
    )

    qvals = [float(row["qerror"]) for row in rows if row["qerror"] not in ("", None) and math.isfinite(float(row["qerror"]))]
    lvals = [float(row["latency_s"]) for row in rows if row["latency_s"] not in ("", None) and math.isfinite(float(row["latency_s"]))]
    summary = [
        {
            "dataset": args.dataset,
            "count_rows": len(rows),
            "ok_status": sum(1 for row in rows if row["status"] == "ok"),
            "failed_or_missing": sum(1 for row in rows if row["status"] != "ok"),
            "valid_qerror": len(qvals),
            "mean_qerror": sum(qvals) / len(qvals) if qvals else "",
            "median_qerror": percentile(qvals, 0.50),
            "p90_qerror": percentile(qvals, 0.90),
            "p95_qerror": percentile(qvals, 0.95),
            "p99_qerror": percentile(qvals, 0.99),
            "max_qerror": max(qvals) if qvals else "",
            "latency_n": len(lvals),
            "mean_latency_s": sum(lvals) / len(lvals) if lvals else "",
            "median_latency_s": percentile(lvals, 0.50),
            "p90_latency_s": percentile(lvals, 0.90),
            "total_latency_s": sum(lvals) if lvals else "",
            "model_dir": args.model_dir,
            "query_file": args.query_file,
        }
    ]
    summary_path = os.path.join(args.out_dir, "{}_bayescard_relational_summary.csv".format(args.dataset.replace("-", "_")))
    write_csv(summary_path, summary, list(summary[0].keys()))
    print("Detail written to {}".format(detail_path))
    print("Summary written to {}".format(summary_path))
    print(summary[0])


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("stage", choices=["prepare-hdf", "train", "evaluate", "all"])
    parser.add_argument("--dataset", choices=["job-light", "imdb-no-pred", "stats"], required=True)
    parser.add_argument("--csv-template")
    parser.add_argument("--query-file")
    parser.add_argument("--hdf-dir")
    parser.add_argument("--model-dir")
    parser.add_argument("--out-dir")
    parser.add_argument("--csv-separator", default=",")
    parser.add_argument("--max-rows-per-hdf-file", type=int, default=20000000)
    parser.add_argument("--learning-algo", default="chow-liu")
    parser.add_argument("--max-parents", type=int, default=1)
    parser.add_argument("--sample-size", type=int, default=200000)
    parser.add_argument("--df-sample-size", type=int, default=10000000)
    parser.add_argument("--post-sampling-factor", type=int, default=10)
    parser.add_argument("--start-index", type=int, default=0)
    parser.add_argument("--end-index", type=int, default=None)
    parser.add_argument("--limit", type=int, default=None)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    defaults = DEFAULTS[args.dataset]
    for key, value in defaults.items():
        attr = key
        if getattr(args, attr) is None:
            setattr(args, attr, value)
    return args


def main():
    args = parse_args()
    if args.dataset == "imdb-no-pred" and os.environ.get("PYTHONHASHSEED") != "1":
        env = os.environ.copy()
        env["PYTHONHASHSEED"] = "1"
        os.execvpe(sys.executable, [sys.executable, SCRIPT_PATH] + sys.argv[1:], env)
    if args.dataset == "imdb-no-pred":
        build_imdb_no_pred_workload(args.query_file)
    if args.stage in ("prepare-hdf", "all"):
        prepare_hdf(args)
    if args.stage in ("train", "all"):
        train(args)
    if args.stage in ("evaluate", "all"):
        evaluate(args)


if __name__ == "__main__":
    main()
