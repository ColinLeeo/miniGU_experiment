SELECT COUNT(*)
FROM person_islocatedin_city e1
JOIN (SELECT * FROM person_islocatedin_city e2 WHERE e2.creationdate >= 1262304000000) e2 ON e2.dst = e1.dst
JOIN person_likes_comment e3 ON e3.src = e2.src
JOIN comment_hascreator_person e4 ON e4.src = e3.dst AND e4.dst = e1.src
JOIN (SELECT * FROM person v1 WHERE v1.birthday >= 315532800000) v1 ON v1.id = e1.src
JOIN (SELECT * FROM comment v3 WHERE v3.explicitlydeleted = false) v3 ON v3.id = e3.dst
