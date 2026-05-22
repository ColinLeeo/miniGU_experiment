SELECT COUNT(*)
FROM (SELECT * FROM comment_hascreator_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_hasinterest_tag e2 ON e2.src = e1.dst
JOIN comment_hastag_tag e3 ON e3.src = e1.src AND e3.dst = e2.dst
JOIN (SELECT * FROM person v2 WHERE v2.birthday >= 315532800000) v2 ON v2.id = e1.dst
JOIN (SELECT * FROM comment v1 WHERE v1.explicitlydeleted = false) v1 ON v1.id = e1.src
