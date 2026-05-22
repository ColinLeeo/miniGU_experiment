from Schemas.graph_representation import SchemaGraph, Table


def _table(data_path, name, attrs, primary_key=None, irrelevant=None, no_compression=None):
    if primary_key is None:
        primary_key = ["id"] if "id" in attrs else []
    return Table(
        name,
        primary_key=primary_key,
        attributes=attrs,
        irrelevant_attributes=irrelevant or [],
        no_compression=no_compression or [],
        csv_file_location=data_path.format(name),
        table_size=0,
    )


def gen_ldbc_sf1_schema(data_path):
    schema = SchemaGraph()

    tables = {
        "city": ["id", "name", "url", "type"],
        "country": ["id", "name", "url", "type"],
        "continent": ["id", "name", "url", "type"],
        "tag": ["id", "name", "url"],
        "tagclass": ["id", "name", "url"],
        "person": ["id", "creationdate", "deletiondate", "explicitlydeleted", "firstname", "lastname", "gender", "birthday", "locationip", "browserused", "language", "email"],
        "comment": ["id", "creationdate", "deletiondate", "explicitlydeleted", "locationip", "browserused", "content", "length"],
        "post": ["id", "creationdate", "deletiondate", "explicitlydeleted", "imagefile", "locationip", "browserused", "language", "content", "length"],
        "forum": ["id", "creationdate", "deletiondate", "explicitlydeleted", "title"],
        "company": ["id", "type", "name", "url"],
        "university": ["id", "type", "name", "url"],
        "city_ispartof_country": ["src", "dst"],
        "country_ispartof_continent": ["src", "dst"],
        "tag_hastype_tagclass": ["src", "dst"],
        "tagclass_issubclassof_tagclass": ["src", "dst"],
        "comment_hascreator_person": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "comment_hastag_tag": ["src", "dst", "creationdate", "deletiondate"],
        "comment_islocatedin_country": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "comment_replyof_comment": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "comment_replyof_post": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "company_islocatedin_country": ["src", "dst"],
        "forum_containerof_post": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "forum_hasmember_person": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "forum_hasmoderator_person": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "forum_hastag_tag": ["src", "dst", "creationdate", "deletiondate"],
        "person_hasinterest_tag": ["src", "dst", "creationdate", "deletiondate"],
        "person_islocatedin_city": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "person_knows_person": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "person_likes_comment": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "person_likes_post": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "person_studyat_university": ["src", "dst", "creationdate", "deletiondate", "classyear"],
        "person_workat_company": ["src", "dst", "creationdate", "deletiondate", "workfrom"],
        "post_hascreator_person": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "post_hastag_tag": ["src", "dst", "creationdate", "deletiondate"],
        "post_islocatedin_country": ["src", "dst", "creationdate", "deletiondate", "explicitlydeleted"],
        "university_islocatedin_city": ["src", "dst"],
    }
    irrelevant = {
        "city": ["name", "url", "type"],
        "country": ["name", "url", "type"],
        "continent": ["name", "url", "type"],
        "tag": ["url"],
        "tagclass": ["name", "url"],
        "person": ["firstname", "lastname", "locationip", "language", "email"],
        "comment": ["locationip", "content"],
        "post": ["imagefile", "locationip", "content"],
        "forum": ["title"],
        "company": ["type", "name", "url"],
        "university": ["type", "name", "url"],
    }
    for name, attrs in tables.items():
        schema.add_table(_table(data_path, name, attrs, primary_key=["id"] if "id" in attrs else [], irrelevant=irrelevant.get(name, [])))

    relationships = [
        ("city_ispartof_country", "src", "city", "id"),
        ("city_ispartof_country", "dst", "country", "id"),
        ("country_ispartof_continent", "src", "country", "id"),
        ("country_ispartof_continent", "dst", "continent", "id"),
        ("tag_hastype_tagclass", "src", "tag", "id"),
        ("tag_hastype_tagclass", "dst", "tagclass", "id"),
        ("tagclass_issubclassof_tagclass", "src", "tagclass", "id"),
        ("tagclass_issubclassof_tagclass", "dst", "tagclass", "id"),
        ("comment_hascreator_person", "src", "comment", "id"),
        ("comment_hascreator_person", "dst", "person", "id"),
        ("comment_hastag_tag", "src", "comment", "id"),
        ("comment_hastag_tag", "dst", "tag", "id"),
        ("comment_islocatedin_country", "src", "comment", "id"),
        ("comment_islocatedin_country", "dst", "country", "id"),
        ("comment_replyof_comment", "src", "comment", "id"),
        ("comment_replyof_comment", "dst", "comment", "id"),
        ("comment_replyof_post", "src", "comment", "id"),
        ("comment_replyof_post", "dst", "post", "id"),
        ("company_islocatedin_country", "src", "company", "id"),
        ("company_islocatedin_country", "dst", "country", "id"),
        ("forum_containerof_post", "src", "forum", "id"),
        ("forum_containerof_post", "dst", "post", "id"),
        ("forum_hasmember_person", "src", "forum", "id"),
        ("forum_hasmember_person", "dst", "person", "id"),
        ("forum_hasmoderator_person", "src", "forum", "id"),
        ("forum_hasmoderator_person", "dst", "person", "id"),
        ("forum_hastag_tag", "src", "forum", "id"),
        ("forum_hastag_tag", "dst", "tag", "id"),
        ("person_hasinterest_tag", "src", "person", "id"),
        ("person_hasinterest_tag", "dst", "tag", "id"),
        ("person_islocatedin_city", "src", "person", "id"),
        ("person_islocatedin_city", "dst", "city", "id"),
        ("person_knows_person", "src", "person", "id"),
        ("person_knows_person", "dst", "person", "id"),
        ("person_likes_comment", "src", "person", "id"),
        ("person_likes_comment", "dst", "comment", "id"),
        ("person_likes_post", "src", "person", "id"),
        ("person_likes_post", "dst", "post", "id"),
        ("person_studyat_university", "src", "person", "id"),
        ("person_studyat_university", "dst", "university", "id"),
        ("person_workat_company", "src", "person", "id"),
        ("person_workat_company", "dst", "company", "id"),
        ("post_hascreator_person", "src", "post", "id"),
        ("post_hascreator_person", "dst", "person", "id"),
        ("post_hastag_tag", "src", "post", "id"),
        ("post_hastag_tag", "dst", "tag", "id"),
        ("post_islocatedin_country", "src", "post", "id"),
        ("post_islocatedin_country", "dst", "country", "id"),
        ("university_islocatedin_city", "src", "university", "id"),
        ("university_islocatedin_city", "dst", "city", "id"),
    ]
    for rel in relationships:
        schema.add_relationship(*rel)
    return schema
