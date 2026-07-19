SELECT COUNT(*) FROM "title" AS v2, "castInfoVertex_castInfoEdge_title" AS e1 WHERE e1.dst = v2.id AND v2."production_year" > 1980 AND v2."production_year" < 1995;
