#!/usr/bin/env python3
"""Run XDB/WanderJoin on the 16 LDBC no-predicate SQL queries."""

from xdb_wanderjoin_common import DatasetConfig, ROOT
from xdb_wanderjoin_relci_common import build_relci_arg_parser, run_relci_dataset


CFG = DatasetConfig(
    name="ldbc_no_pred_16",
    schema="xdb_ldbc",
    data_dir=ROOT / "datasets" / "ldbc" / "sf1",
    truth_csv=ROOT / "result" / "xdb_wanderjoin" / "ldbc_no_pred_16_truth.csv",
    truth_query_col="query",
    truth_card_col="true_cardinality",
    ldbc_truth_paths=True,
)


def main() -> None:
    parser = build_relci_arg_parser(__doc__ or "")
    args = parser.parse_args()
    run_relci_dataset(CFG, args)


if __name__ == "__main__":
    main()
