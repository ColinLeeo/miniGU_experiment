from Schemas.graph_representation import SchemaGraph, Table


def _table(data_path, name, attrs, primary_key=None):
    if primary_key is None:
        primary_key = ["id"] if "id" in attrs else []
    return Table(
        name,
        primary_key=primary_key,
        attributes=attrs,
        irrelevant_attributes=[],
        no_compression=[],
        csv_file_location=data_path.format(name),
        table_size=0,
    )


def gen_job_light_unique_schema(data_path):
    schema = SchemaGraph()
    tables = {
        "castinfovertex": ["id", "person_role_id", "role_id"],
        "companyname": ["id"],
        "infoidxvertex": ["id", "info_type_id"],
        "infovertex": ["id", "info_type_id"],
        "keyword": ["id"],
        "title": ["id", "kind_id", "production_year"],
        "castinfovertex_castinfoedge_title": ["src", "dst"],
        "title_infoedge_infoidxvertex": ["src", "dst"],
        "title_infoedge_infovertex": ["src", "dst"],
        "title_keywordedge_keyword": ["src", "dst", "keyword_id"],
        "title_moviecompanies_companyname": ["src", "dst", "company_id", "company_type_id"],
    }
    for name, attrs in tables.items():
        schema.add_table(_table(data_path, name, attrs, primary_key=["id"] if "id" in attrs else []))

    relationships = [
        ("castinfovertex_castinfoedge_title", "src", "castinfovertex", "id"),
        ("castinfovertex_castinfoedge_title", "dst", "title", "id"),
        ("title_infoedge_infovertex", "src", "title", "id"),
        ("title_infoedge_infovertex", "dst", "infovertex", "id"),
        ("title_infoedge_infoidxvertex", "src", "title", "id"),
        ("title_infoedge_infoidxvertex", "dst", "infoidxvertex", "id"),
        ("title_keywordedge_keyword", "src", "title", "id"),
        ("title_keywordedge_keyword", "dst", "keyword", "id"),
        ("title_moviecompanies_companyname", "src", "title", "id"),
        ("title_moviecompanies_companyname", "dst", "companyname", "id"),
    ]
    for rel in relationships:
        schema.add_relationship(*rel)
    return schema
