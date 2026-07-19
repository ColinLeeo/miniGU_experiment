#!/usr/bin/env python3
"""Run XDB/WanderJoin on STATS-CEB with-predicate workload."""

from xdb_wanderjoin_common import DatasetConfig, ROOT, build_arg_parser, run_dataset


CFG = DatasetConfig(
    name="stats",
    schema="xdb_stats",
    data_dir=ROOT / "datasets" / "stats_ceb",
    query_dir=ROOT / "patterns" / "sql" / "stats_ceb",
    truth_csv=ROOT / "patterns" / "gcard" / "stats_ceb" / "truth.csv",
    truth_query_col="query",
    truth_card_col="truth_cardinality",
)


def main() -> None:
    parser = build_arg_parser(__doc__ or "")
    args = parser.parse_args()
    run_dataset(CFG, args)


if __name__ == "__main__":
    main()
