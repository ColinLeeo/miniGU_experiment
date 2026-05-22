SELECT COUNT(*)
FROM (SELECT * FROM comment_hascreator_person e1 WHERE e1.deletiondate <= 1356998400000) e1
JOIN (SELECT * FROM person_hasinterest_tag e2 WHERE e2.creationdate >= 1262304000000) e2 ON e2.src = e1.dst
JOIN comment_hastag_tag e3 ON e3.src = e1.src AND e3.dst = e2.dst
JOIN (SELECT * FROM comment v1 WHERE v1.browserused = 'Chrome' AND v1.explicitlydeleted = false) v1 ON v1.id = e1.src
