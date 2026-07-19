#!/usr/bin/env python3
"""Run XDB/WanderJoin on IMDB JOB-light unique with-predicate workload."""

from xdb_wanderjoin_common import DatasetConfig, ROOT, build_arg_parser, run_dataset


CFG = DatasetConfig(
    name="imdb",
    schema="xdb_imdb",
    data_dir=ROOT / "datasets" / "imdb" / "job_light_unique",
    query_dir=ROOT / "patterns" / "sql" / "job_light_imdb_unique",
    truth_csv=ROOT
    / "result"
    / "sql_baselines"
    / "job_light_imdb_unique_k2d5"
    / "job_light_imdb_unique_gcare_ratio003_indepfb_qerror_latency_long.csv",
    truth_query_col="query",
    truth_card_col="true_cardinality",
)


def main() -> None:
    parser = build_arg_parser(__doc__ or "")
    args = parser.parse_args()
    run_dataset(CFG, args)


if __name__ == "__main__":
    main()
