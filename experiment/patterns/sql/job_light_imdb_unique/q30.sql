SELECT COUNT(*) FROM "title" AS v1, "title_movieCompanies_companyName" AS e1, "castInfoVertex_castInfoEdge_title" AS e2 WHERE e1.src = v1.id AND e2.dst = v1.id AND v1."production_year" > 1990;
