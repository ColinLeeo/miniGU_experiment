SELECT COUNT(*) FROM "title" AS v2, "title_movieCompanies_companyName" AS e1, "title_keywordEdge_keyword" AS e2 WHERE e1.src = v2.id AND e2.src = v2.id AND e2."keyword_id" = 117;
