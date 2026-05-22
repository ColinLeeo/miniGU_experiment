#!/usr/bin/env python
import argparse
import csv
import os


REPRO_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))


def normalize_bool(value):
    lower = value.lower()
    if lower == "true":
        return "1"
    if lower == "false":
        return "0"
    return value


def prepare_file(src_path, dst_path, force=False):
    if os.path.exists(dst_path) and not force:
        print("skip existing {}".format(dst_path))
        return

    with open(src_path, newline="", encoding="utf-8") as src:
        reader = csv.reader(src)
        header = next(reader)
        is_edge_table = len(header) >= 2 and header[0] == "src" and header[1] == "dst"
        output_header = ["id"] + header if is_edge_table else header
        bool_indexes = [
            i
            for i, column in enumerate(header)
            if column == "explicitlydeleted"
        ]

        tmp_path = dst_path + ".tmp"
        with open(tmp_path, "w", newline="", encoding="utf-8") as dst:
            writer = csv.writer(dst)
            writer.writerow(output_header)
            for row_id, row in enumerate(reader):
                for index in bool_indexes:
                    row[index] = normalize_bool(row[index])
                if is_edge_table:
                    writer.writerow([row_id] + row)
                else:
                    writer.writerow(row)
        os.replace(tmp_path, dst_path)
    print("wrote {}".format(dst_path))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--source-dir",
        default="/home/zxz/miniGU/experiment/datasets/ldbc/sf1",
    )
    parser.add_argument(
        "--output-dir",
        default=os.path.join(REPRO_DIR, "prepared_csv"),
    )
    parser.add_argument("--force", action="store_true")
    parser.add_argument(
        "--tables",
        nargs="*",
        help="Optional table names without .csv. Defaults to all CSV files.",
    )
    args = parser.parse_args()

    os.makedirs(args.output_dir, exist_ok=True)
    if args.tables:
        csv_files = ["{}.csv".format(table) for table in args.tables]
    else:
        csv_files = sorted(
            file_name
            for file_name in os.listdir(args.source_dir)
            if file_name.endswith(".csv")
        )

    for file_name in csv_files:
        prepare_file(
            os.path.join(args.source_dir, file_name),
            os.path.join(args.output_dir, file_name),
            force=args.force,
        )


if __name__ == "__main__":
    main()
