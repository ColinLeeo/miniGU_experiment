#!/bin/bash
set -eu
set -o pipefail

# Build duckdb database from IMDB dataset.

workspace=$(realpath "$(dirname "$0")/../../")
duckdb=${DUCKDB_BIN:-$workspace/tools/duckdb}
if [[ ! -x "$duckdb" ]]; then
    duckdb=${DUCKDB_BIN:-duckdb}
fi

mkdir -p "$workspace/graphs/imdb/duckdb"
cd "$workspace/datasets/imdb/imdb"
rm -f "$workspace/graphs/imdb/duckdb/imdb.duckdb"
"$duckdb" "$workspace/graphs/imdb/duckdb/imdb.duckdb" <<EOF
begin;
create table akaName as from read_csv('akaName.csv');
create table akaTitle as from read_csv('akaTitle.csv');
create table castInfoVertex as from read_csv('castInfoVertex.csv');
create table character as from read_csv('character.csv');
create table companyName as from read_csv('companyName.csv');
create table complCastInfoVertex as from read_csv('complCastInfoVertex.csv');
create table infoVertex as from read_csv('infoVertex.csv');
create table infoIdxVertex as from read_csv('infoIdxVertex.csv');
create table keyword as from read_csv('keyword.csv');
create table person as from read_csv('person.csv');
create table personInfoVertex as from read_csv('personInfoVertex.csv');
create table title as from read_csv('title.csv');
create table person_akaNameEdge_akaName as from read_csv('person_akaNameEdge_akaName.csv');
create table title_akaTitleEdge_akaTitle as from read_csv('title_akaTitleEdge_akaTitle.csv');
create table castInfoVertex_castInfoEdge_person as from read_csv('castInfoVertex_castInfoEdge_person.csv');
create table castInfoVertex_castInfoEdge_title as from read_csv('castInfoVertex_castInfoEdge_title.csv');
create table castInfoVertex_castInfoEdge_character as from read_csv('castInfoVertex_castInfoEdge_character.csv');
create table complCastInfoVertex_complCastInfoEdge_title as from read_csv('complCastInfoVertex_complCastInfoEdge_title.csv');
create table title_episodeOfEdge_title as from read_csv('title_episodeOfEdge_title.csv');
create table title_infoEdge_infoVertex as from read_csv('title_infoEdge_infoVertex.csv');
create table title_infoEdge_infoIdxVertex as from read_csv('title_infoEdge_infoIdxVertex.csv');
create table title_keywordEdge_keyword as from read_csv('title_keywordEdge_keyword.csv');
create table title_linkTypeEdge_title as from read_csv('title_linkTypeEdge_title.csv');
create table title_movieCompanies_companyName as from read_csv('title_movieCompanies_companyName.csv');
create table person_personInfoEdge_personInfoVertex as from read_csv('person_personInfoEdge_personInfoVertex.csv');
commit;
EOF
