SELECT COUNT(*)
FROM comment_hascreator_person e1
JOIN person_hasinterest_tag e2 ON e2.src = e1.dst
JOIN (SELECT * FROM comment_hastag_tag e3 WHERE e3.creationdate <= 1300000000000) e3 ON e3.src = e1.src AND e3.dst = e2.dst
JOIN (SELECT * FROM comment v1 WHERE v1.length >= 2 AND v1.explicitlydeleted = false) v1 ON v1.id = e1.src
