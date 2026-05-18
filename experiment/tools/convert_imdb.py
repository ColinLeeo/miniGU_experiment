#!/usr/bin/env python3
import sys
import os
import argparse
import csv
import multiprocessing as mp
from pathlib import Path


class Converter:

    def __init__(self, dataset_dir: str, output_dir: str, num_workers: int):
        self.dataset_dir = Path(dataset_dir)
        self.output_dir = Path(output_dir)
        self.movie_info_map = {}
        self.movie_info_idx_map = {}
        self.person_info_map = {}
        self.movie_info_vertex_id = 0
        self.movie_info_idx_vertex_id = 0
        self.person_info_vertex_id = 0
        self.num_workers = num_workers

    def process(self):
        with mp.Pool(self.num_workers) as p:
            result = []
            result.append(p.apply_async(self.process_title))
            result.append(p.apply_async(self.process_aka_title))
            result.append(p.apply_async(self.process_company_name))
            result.append(p.apply_async(self.process_movie_companies))
            result.append(p.apply_async(self.process_movie_info))
            result.append(p.apply_async(self.process_movie_info_idx))
            result.append(p.apply_async(self.process_keyword))
            result.append(p.apply_async(self.process_movie_keyword))
            result.append(p.apply_async(self.process_movie_link))
            result.append(p.apply_async(self.process_name))
            result.append(p.apply_async(self.process_aka_name))
            result.append(p.apply_async(self.process_person_info))
            result.append(p.apply_async(self.process_character))
            result.append(p.apply_async(self.process_cast_info))
            result.append(p.apply_async(self.process_complete_cast))
            for r in result:
                r.get()

    def __get_movie_info_vertex_id(self):
        id = self.movie_info_vertex_id
        self.movie_info_vertex_id += 1
        return id

    def __get_movie_info_idx_vertex_id(self):
        id = self.movie_info_idx_vertex_id
        self.movie_info_idx_vertex_id += 1
        return id

    def __get_person_info_vertex_id(self):
        id = self.person_info_vertex_id
        self.person_info_vertex_id += 1
        return id

    def process_title(self):
        f1 = open(self.dataset_dir.joinpath("title.csv"))
        f2 = open(self.output_dir.joinpath("title.csv"), "w+")
        f3 = open(self.output_dir.joinpath("title_episodeOfEdge_title.csv"),
                  "w+")
        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()

        for record in r:
            id = int(record[0])
            w1.writerow({"id": id})
            if record[7] != "":
                episode_of_id = int(record[7])
                w2.writerow({"src": id, "dst": episode_of_id})

        f1.close()
        f2.close()
        f3.close()

    def process_aka_title(self):
        f1 = open(self.dataset_dir.joinpath("aka_title.csv"))
        f2 = open(self.output_dir.joinpath("akaTitle.csv"), "w+")
        f3 = open(self.output_dir.joinpath("title_akaTitleEdge_akaTitle.csv"),
                  "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()

        for record in r:
            id = int(record[0])
            title_id = int(record[1])
            w1.writerow({"id": id})
            if title_id != 0:
                w2.writerow({"src": title_id, "dst": id})

        f1.close()
        f2.close()
        f3.close()

    def process_company_name(self):
        f1 = open(self.dataset_dir.joinpath("company_name.csv"))
        f2 = open(self.output_dir.joinpath("companyName.csv"), "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w1.writeheader()

        for record in r:
            id = int(record[0])
            w1.writerow({"id": id})

        f1.close()
        f2.close()

    def process_movie_companies(self):
        f1 = open(self.dataset_dir.joinpath("movie_companies.csv"))
        f2 = open(
            self.output_dir.joinpath("title_movieCompanies_companyName.csv"),
            "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["src", "dst"])
        w1.writeheader()

        for record in r:
            title_id = int(record[1])
            company_id = int(record[2])
            w1.writerow({"src": title_id, "dst": company_id})

        f1.close()
        f2.close()

    def process_movie_info(self):
        f1 = open(self.dataset_dir.joinpath("movie_info.csv"))
        f2 = open(self.output_dir.joinpath("infoVertex.csv"), "w+")
        f3 = open(self.output_dir.joinpath("title_infoEdge_infoVertex.csv"),
                  "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()

        for record in r:
            title_id = int(record[1])
            info = record[3]
            if info in self.movie_info_map:
                info_id = self.movie_info_map[info]
            else:
                info_id = self.__get_movie_info_vertex_id()
                self.movie_info_map[info] = info_id
                w1.writerow({"id": info_id})
            w2.writerow({"src": title_id, "dst": info_id})

        f1.close()
        f2.close()
        f3.close()

    def process_movie_info_idx(self):
        f1 = open(self.dataset_dir.joinpath("movie_info_idx.csv"))
        f2 = open(self.output_dir.joinpath("infoIdxVertex.csv"), "w+")
        f3 = open(self.output_dir.joinpath("title_infoEdge_infoIdxVertex.csv"),
                  "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()

        for record in r:
            title_id = int(record[1])
            info = record[3]
            if info in self.movie_info_idx_map:
                info_id = self.movie_info_idx_map[info]
            else:
                info_id = self.__get_movie_info_idx_vertex_id()
                self.movie_info_idx_map[info] = info_id
                w1.writerow({"id": info_id})
            w2.writerow({"src": title_id, "dst": info_id})

        f1.close()
        f2.close()
        f3.close()

    def process_keyword(self):
        f1 = open(self.dataset_dir.joinpath("keyword.csv"))
        f2 = open(self.output_dir.joinpath("keyword.csv"), "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w1.writeheader()

        for record in r:
            id = int(record[0])
            w1.writerow({"id": id})

        f1.close()
        f2.close()

    def process_movie_keyword(self):
        f1 = open(self.dataset_dir.joinpath("movie_keyword.csv"))
        f2 = open(self.output_dir.joinpath("title_keywordEdge_keyword.csv"),
                  "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["src", "dst"])
        w1.writeheader()

        for record in r:
            title_id = int(record[1])
            keyword_id = int(record[2])
            w1.writerow({"src": title_id, "dst": keyword_id})

        f1.close()
        f2.close()

    def process_movie_link(self):
        f1 = open(self.dataset_dir.joinpath("movie_link.csv"))
        f2 = open(self.output_dir.joinpath("title_linkTypeEdge_title.csv"),
                  "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["src", "dst"])
        w1.writeheader()

        for record in r:
            title_id = int(record[1])
            linked_title_id = int(record[2])
            w1.writerow({"src": title_id, "dst": linked_title_id})

        f1.close()
        f2.close()

    def process_name(self):
        f1 = open(self.dataset_dir.joinpath("name.csv"))
        f2 = open(self.output_dir.joinpath("person.csv"), "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w1.writeheader()

        for record in r:
            person_id = int(record[0])
            w1.writerow({"id": person_id})

        f1.close()
        f2.close()

    def process_aka_name(self):
        f1 = open(self.dataset_dir.joinpath("aka_name.csv"))
        f2 = open(self.output_dir.joinpath("akaName.csv"), "w+")
        f3 = open(self.output_dir.joinpath("person_akaNameEdge_akaName.csv"),
                  "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()

        for record in r:
            aka_name_id = int(record[0])
            person_id = int(record[1])
            w1.writerow({"id": aka_name_id})
            w2.writerow({"src": person_id, "dst": aka_name_id})

        f1.close()
        f2.close()
        f3.close()

    def process_person_info(self):
        f1 = open(self.dataset_dir.joinpath("person_info.csv"))
        f2 = open(self.output_dir.joinpath("personInfoVertex.csv"), "w+")
        f3 = open(
            self.output_dir.joinpath(
                "person_personInfoEdge_personInfoVertex.csv"), "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()

        for record in r:
            person_id = int(record[1])
            info = record[3]
            if info in self.person_info_map:
                info_id = self.person_info_map[info]
            else:
                info_id = self.__get_person_info_vertex_id()
                self.person_info_map[info] = info_id
                w1.writerow({"id": info_id})
            w2.writerow({"src": person_id, "dst": info_id})

        f1.close()
        f2.close()
        f3.close()

    def process_character(self):
        f1 = open(self.dataset_dir.joinpath("char_name.csv"))
        f2 = open(self.output_dir.joinpath("character.csv"), "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w1.writeheader()

        for record in r:
            char_id = int(record[0])
            w1.writerow({"id": char_id})

        f1.close()
        f2.close()

    def process_cast_info(self):
        f1 = open(self.dataset_dir.joinpath("cast_info.csv"))
        f2 = open(self.output_dir.joinpath("castInfoVertex.csv"), "w+")
        f3 = open(
            self.output_dir.joinpath("castInfoVertex_castInfoEdge_person.csv"),
            "w+")
        f4 = open(
            self.output_dir.joinpath("castInfoVertex_castInfoEdge_title.csv"),
            "w+")
        f5 = open(
            self.output_dir.joinpath(
                "castInfoVertex_castInfoEdge_character.csv"), "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w3 = csv.DictWriter(f4, ["src", "dst"])
        w4 = csv.DictWriter(f5, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()
        w3.writeheader()
        w4.writeheader()

        for record in r:
            cast_info_id = int(record[0])
            person_id = int(record[1])
            movie_id = int(record[2])
            w1.writerow({"id": cast_info_id})
            w2.writerow({"src": cast_info_id, "dst": person_id})
            w3.writerow({"src": cast_info_id, "dst": movie_id})
            if record[3] != "":
                char_id = int(record[3])
                w4.writerow({"src": cast_info_id, "dst": char_id})

        f1.close()
        f2.close()
        f3.close()
        f4.close()
        f5.close()

    def process_complete_cast(self):
        f1 = open(self.dataset_dir.joinpath("complete_cast.csv"))
        f2 = open(self.output_dir.joinpath("complCastInfoVertex.csv"), "w+")
        f3 = open(
            self.output_dir.joinpath(
                "complCastInfoVertex_complCastInfoEdge_title.csv"), "w+")

        r = csv.reader(f1,
                       delimiter=",",
                       quotechar="\"",
                       lineterminator="\n",
                       escapechar="\\")
        w1 = csv.DictWriter(f2, ["id"])
        w2 = csv.DictWriter(f3, ["src", "dst"])
        w1.writeheader()
        w2.writeheader()

        for record in r:
            compl_cast_id = int(record[0])
            movie_id = int(record[1])
            w1.writerow({"id": compl_cast_id})
            w2.writerow({"src": compl_cast_id, "dst": movie_id})

        f1.close()
        f2.close()
        f3.close()


def main():
    parser = argparse.ArgumentParser(
        prog=sys.argv[0],
        description="Convert the IMDB dataset to property graph format.")
    parser.add_argument("-d",
                        "--dataset",
                        help="Specify the dataset dir",
                        required=True)
    parser.add_argument("-o",
                        "--output",
                        help="Specify the output dir",
                        required=True)
    parser.add_argument("-w",
                        "--workers",
                        help="Specify the number of worker processes",
                        default=8)
    args = parser.parse_args()

    os.makedirs(args.output, exist_ok=True)

    converter = Converter(args.dataset, args.output, args.workers)
    converter.process()


if __name__ == "__main__":
    main()
