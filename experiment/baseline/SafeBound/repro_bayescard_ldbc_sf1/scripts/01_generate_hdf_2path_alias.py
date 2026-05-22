#!/usr/bin/env python
import argparse
import os
import sys

REPRO_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SAFEB0UND_BAYESCARD = os.path.abspath(os.path.join(REPRO_DIR, "..", "bayescard"))
if SAFEB0UND_BAYESCARD not in sys.path:
    sys.path.insert(0, SAFEB0UND_BAYESCARD)
if os.path.dirname(__file__) not in sys.path:
    sys.path.insert(0, os.path.dirname(__file__))

from DataPrepare.prepare_single_tables import prepare_all_tables
from ldbc_full_schema import DEFAULT_DATA_DIR
from ldbc_alias_2path_schema import DEFAULT_QUERY_DIR, build_alias_2path_schema, write_work_items
from reader_patch import patch_header_csv_reader


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-dir", default=DEFAULT_DATA_DIR)
    parser.add_argument("--query-dir", default=DEFAULT_QUERY_DIR)
    parser.add_argument("--hdf-dir", default=os.path.join(REPRO_DIR, "hdf_2path_alias_local"))
    parser.add_argument("--worklist", default=os.path.join(REPRO_DIR, "results", "ldbc_2path_alias_local_worklist.csv"))
    parser.add_argument("--csv-separator", default=",")
    parser.add_argument("--max-table-data", type=int, default=20000000)
    args = parser.parse_args()

    os.makedirs(args.hdf_dir, exist_ok=True)
    patch_header_csv_reader()
    schema, work_items, patterns = build_alias_2path_schema(args.query_dir, args.data_dir)
    write_work_items(work_items, args.worklist)
    print("generating alias HDF for {} tables, {} relationships, {} 2-path BNs from {} patterns".format(
        len(schema.tables), len(schema.relationships), len(work_items), len(patterns)
    ))
    print("worklist written to {}".format(args.worklist))
    prepare_all_tables(schema, args.hdf_dir, csv_seperator=args.csv_separator, max_table_data=args.max_table_data)


if __name__ == "__main__":
    main()
