#!/usr/bin/env python3
"""Run XDB/WanderJoin rel_ci-threshold reports for run_imdb_xdb_wanderjoin."""

from run_imdb_xdb_wanderjoin import CFG
from xdb_wanderjoin_relci_common import build_relci_arg_parser, run_relci_dataset


def main() -> None:
    parser = build_relci_arg_parser(__doc__ or "")
    args = parser.parse_args()
    run_relci_dataset(CFG, args)


if __name__ == "__main__":
    main()
