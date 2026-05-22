# BayesCard LDBC 2path BN

This directory keeps the Python code needed by the LDBC alias-aware 2path BN flow.
Only the alias-aware 2path BN flow is kept here.

## Kept Python files

```text
scripts/00_prepare_csv.py
scripts/01_generate_hdf_2path_alias.py
scripts/02_train_bn_2path_alias.py
scripts/06_eval_ldbc_with_pred.py
scripts/ldbc_alias_2path_schema.py
scripts/ldbc_full_schema.py
scripts/reader_patch.py
scripts/train_utils.py
```

## Bash entrypoints

Use the wrappers in:

```text
experiment/scripts/bayescard/
```

They activate the BayesCard virtualenv and set the default input/output paths.

```bash
experiment/scripts/bayescard/prepare_ldbc_sf1_csv.sh
experiment/scripts/bayescard/train_lsqb_glogs_2path_bn.sh
experiment/scripts/bayescard/query_lsqb_glogs_no_pred.sh
experiment/scripts/bayescard/query_lsqb_glogs_with_pred.sh
```

Main overrides:

```text
VENV=/path/to/bayescard/env
RAW_DATA_DIR=/path/to/ldbc/sf1
PREPARED_CSV_DIR=/path/to/prepared_csv
QUERY_DIR=/path/to/flat_sql
MODEL_DIR=/path/to/models_2path_alias_local
OUTPUT=/path/to/estimate.csv
```

Generated CSV, HDF, model and result files are ignored by git.
