#!/usr/bin/env python3
"""Run XDB/WanderJoin on LDBC SF1 with-predicate SQL workload."""

from xdb_wanderjoin_common import DatasetConfig, ROOT, build_arg_parser, run_dataset


CFG = DatasetConfig(
    name="ldbc",
    schema="xdb_ldbc",
    data_dir=ROOT / "datasets" / "ldbc" / "sf1",
    truth_csv=ROOT / "result" / "truecard" / "ldbc_sf1_unique_with_pred_10" / "truth.csv",
    truth_query_col="variant",
    truth_card_col="true_cardinality",
    ldbc_truth_paths=True,
)


def main() -> None:
    parser = build_arg_parser(__doc__ or "")
    args = parser.parse_args()
    run_dataset(CFG, args)


if __name__ == "__main__":
    main()
