#!/usr/bin/env python
import argparse
import os
import pickle
import sys

REPRO_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SAFEB0UND_BAYESCARD = os.path.abspath(os.path.join(REPRO_DIR, "..", "bayescard"))
if SAFEB0UND_BAYESCARD not in sys.path:
    sys.path.insert(0, SAFEB0UND_BAYESCARD)
if os.path.dirname(__file__) not in sys.path:
    sys.path.insert(0, os.path.dirname(__file__))

import pgmpy
sys.modules.setdefault("Pgmpy", pgmpy)

from DataPrepare.join_data_preparation import JoinDataPreparator
from Models.Bayescard_BN import Bayescard_BN, build_meta_info
from ldbc_full_schema import DEFAULT_DATA_DIR
from ldbc_alias_2path_schema import DEFAULT_QUERY_DIR, build_alias_2path_schema, read_work_items, write_work_items
from train_utils import infer_attr_types


def train_2path_range(schema, work_items, hdf_path, model_folder, algorithm, max_parents, sample_size, join_sample_size, start_index, end_index, skip_existing=True):
    os.makedirs(model_folder, exist_ok=True)
    prep = JoinDataPreparator(os.path.join(hdf_path, "meta_data.pkl"), schema, max_table_data=20000000)
    if end_index is None:
        end_index = len(work_items)
    print("training 2-path items [{}:{}) out of {}".format(start_index, end_index, len(work_items)))
    for item in work_items[start_index:end_index]:
        i = item["index"]
        relation = item["relationships"]
        model_path = os.path.join(model_folder, "{:04d}_2path_{}_{}.pkl".format(i, algorithm, max_parents))
        if skip_existing and os.path.exists(model_path):
            print("skip existing {}".format(model_path))
            continue
        print("training {} {}: {}".format(i, item["pattern"], " ; ".join(relation)))
        df, meta_types, null_values, full_join_est = prep.generate_n_samples(
            join_sample_size,
            relationship_list=relation,
            post_sampling_factor=10,
        )
        columns = list(df.columns)
        assert len(columns) == len(meta_types) == len(null_values)
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
            algorithm=algorithm,
            max_parents=max_parents,
            ignore_cols=["id"],
            sample_size=sample_size,
        )
        with open(model_path, "wb") as handle:
            pickle.dump(bn, handle, pickle.HIGHEST_PROTOCOL)
        print("model saved at {}".format(model_path))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-dir", default=DEFAULT_DATA_DIR)
    parser.add_argument("--query-dir", default=DEFAULT_QUERY_DIR)
    parser.add_argument("--hdf-dir", default=os.path.join(REPRO_DIR, "hdf_2path_alias_local"))
    parser.add_argument("--model-dir", default=os.path.join(REPRO_DIR, "models_2path_alias_local"))
    parser.add_argument("--worklist", default=os.path.join(REPRO_DIR, "results", "ldbc_2path_alias_local_worklist.csv"))
    parser.add_argument("--algorithm", default="chow-liu")
    parser.add_argument("--max-parents", type=int, default=1)
    parser.add_argument("--sample-size", type=int, default=10000)
    parser.add_argument("--join-sample-size", type=int, default=50000)
    parser.add_argument("--start-index", type=int, default=0)
    parser.add_argument("--end-index", type=int, default=None)
    parser.add_argument("--overwrite", action="store_true")
    args = parser.parse_args()

    schema, generated_work_items, patterns = build_alias_2path_schema(args.query_dir, args.data_dir)
    if os.path.exists(args.worklist):
        work_items = read_work_items(args.worklist)
    else:
        work_items = generated_work_items
        write_work_items(work_items, args.worklist)
    print("schema has {} tables, {} relationships from {} patterns; worklist has {} 2-path items".format(
        len(schema.tables), len(schema.relationships), len(patterns), len(work_items)
    ))
    train_2path_range(
        schema,
        work_items,
        args.hdf_dir,
        args.model_dir,
        args.algorithm,
        args.max_parents,
        args.sample_size,
        args.join_sample_size,
        args.start_index,
        args.end_index,
        skip_existing=not args.overwrite,
    )


if __name__ == "__main__":
    main()
