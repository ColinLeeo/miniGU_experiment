import os
import pickle

from pandas.api.types import is_numeric_dtype

from DataPrepare.join_data_preparation import JoinDataPreparator
from Models.Bayescard_BN import Bayescard_BN, build_meta_info


def infer_attr_types(dataset, threshold=3000):
    attr_type = {}
    for col in dataset.columns:
        n_unique = dataset[col].nunique()
        if n_unique == 2:
            attr_type[col] = "boolean"
        elif is_numeric_dtype(dataset[col]) and (
            n_unique >= len(dataset) / 30 or n_unique > threshold
        ):
            attr_type[col] = "continuous"
        else:
            attr_type[col] = "categorical"
    return attr_type


def train_relationship_range(
    schema,
    hdf_path,
    model_folder,
    algorithm,
    max_parents,
    sample_size,
    join_sample_size,
    start_index,
    end_index,
    skip_existing=True,
):
    os.makedirs(model_folder, exist_ok=True)
    prep = JoinDataPreparator(
        os.path.join(hdf_path, "meta_data.pkl"),
        schema,
        max_table_data=20000000,
    )
    relationships = list(schema.relationships)
    if end_index is None:
        end_index = len(relationships)

    print("training relationships [{}:{}) out of {}".format(
        start_index,
        end_index,
        len(relationships),
    ))
    for i, relationship_obj in enumerate(
        relationships[start_index:end_index],
        start=start_index,
    ):
        model_path = os.path.join(
            model_folder,
            "{}_{}_{}.pkl".format(i, algorithm, max_parents),
        )
        if skip_existing and os.path.exists(model_path):
            print("skip existing {}".format(model_path))
            continue

        relation = [relationship_obj.identifier]
        print("training {}: {}".format(i, relationship_obj.identifier))
        df, meta_types, null_values, full_join_est = prep.generate_n_samples(
            join_sample_size,
            relationship_list=relation,
            post_sampling_factor=10,
        )
        columns = list(df.columns)
        assert len(columns) == len(meta_types) == len(null_values)
        meta_info = build_meta_info(columns, null_values)
        bn = Bayescard_BN(
            schema,
            relation,
            column_names=columns,
            full_join_size=full_join_est,
            table_meta_data=prep.table_meta_data,
            meta_types=meta_types,
            null_values=null_values,
            meta_info=meta_info,
        )
        bn.build_from_data(
            df,
            attr_type=infer_attr_types(df),
            algorithm=algorithm,
            max_parents=max_parents,
            ignore_cols=["id"],
            sample_size=sample_size,
        )
        with open(model_path, "wb") as handle:
            pickle.dump(bn, handle, pickle.HIGHEST_PROTOCOL)
        print("model saved at {}".format(model_path))
