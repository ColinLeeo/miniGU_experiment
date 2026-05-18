select count(*) from title v0, title v1, keyword v2, title_linkTypeEdge_title e0, title_keywordEdge_keyword e1 where e0.src = v0.id and e0.dst = v1.id and e1.src = v0.id and e1.dst = v2.id
