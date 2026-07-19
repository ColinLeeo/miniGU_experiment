SELECT COUNT(*) FROM "title" AS v2, "title_keywordEdge_keyword" AS e1, "castInfoVertex_castInfoEdge_title" AS e2 WHERE e1.src = v2.id AND e2.dst = v2.id AND v2."production_year" > 2014;
