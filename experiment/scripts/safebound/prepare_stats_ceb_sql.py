#!/usr/bin/env python3
import argparse
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description="Split stats_CEB.sql into SQL patterns and true-cardinality files.")
    parser.add_argument("--input", required=True)
    parser.add_argument("--sql-output-dir", required=True)
    parser.add_argument("--true-card-dir", required=True)
    args = parser.parse_args()

    input_path = Path(args.input)
    sql_output_dir = Path(args.sql_output_dir)
    true_card_dir = Path(args.true_card_dir)
    sql_output_dir.mkdir(parents=True, exist_ok=True)
    true_card_dir.mkdir(parents=True, exist_ok=True)

    count = 0
    for line_no, raw_line in enumerate(input_path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw_line.strip()
        if not line:
            continue
        if "||" not in line:
            raise ValueError(f"line {line_no}: missing truecard separator '||'")
        true_card, sql = line.split("||", 1)
        true_card = true_card.strip()
        sql = sql.strip()
        if not true_card.isdigit():
            raise ValueError(f"line {line_no}: invalid truecard {true_card!r}")
        if not sql:
            raise ValueError(f"line {line_no}: empty SQL")
        count += 1
        stem = f"q{count:03d}"
        (sql_output_dir / f"{stem}.sql").write_text(sql.rstrip(";") + ";\n", encoding="utf-8")
        (true_card_dir / f"{stem}.txt").write_text(true_card + "\n", encoding="utf-8")

    print(f"wrote {count} SQL files to {sql_output_dir}")
    print(f"wrote {count} true-cardinality files to {true_card_dir}")


if __name__ == "__main__":
    main()
