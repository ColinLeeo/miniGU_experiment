SELECT COUNT(*) FROM "title" AS v1, "title_infoEdge_infoVertex" AS e1, "title_keywordEdge_keyword" AS e2 WHERE e1.src = v1.id AND e2.src = v1.id AND v1."production_year" > 1990;
