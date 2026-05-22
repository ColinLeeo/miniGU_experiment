import pandas as pd

import DataPrepare.prepare_single_tables as prepare_single_tables


def read_header_csv(table_obj, csv_seperator=",", stats=False):
    table = pd.read_csv(
        table_obj.csv_file_location,
        escapechar="\\",
        encoding="utf-8",
        quotechar='"',
        sep=csv_seperator,
    )
    expected = list(table_obj.attributes)
    actual = list(table.columns)
    if actual != expected:
        raise ValueError(
            "Unexpected header for {}. Expected {}, got {}".format(
                table_obj.table_name, expected, actual
            )
        )
    table.columns = [
        "{}.{}".format(table_obj.table_name, attribute)
        for attribute in table_obj.attributes
    ]
    drop_columns = [
        "{}.{}".format(table_obj.table_name, attribute)
        for attribute in table_obj.irrelevant_attributes
    ]
    table.drop(columns=drop_columns, inplace=True)
    return table.apply(pd.to_numeric, errors="ignore")


def patch_header_csv_reader():
    prepare_single_tables.read_table_csv = read_header_csv
