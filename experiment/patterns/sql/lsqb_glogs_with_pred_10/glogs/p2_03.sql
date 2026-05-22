SELECT COUNT(*)
FROM (SELECT * FROM person_islocatedin_city e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_islocatedin_city e2 ON e2.dst = e1.dst
JOIN person_likes_comment e3 ON e3.src = e2.src
JOIN comment_hascreator_person e4 ON e4.src = e3.dst AND e4.dst = e1.src
JOIN (SELECT * FROM person v2 WHERE v2.birthday >= 315532800000 AND v2.gender = 'male') v2 ON v2.id = e2.src
JOIN (SELECT * FROM comment v3 WHERE v3.explicitlydeleted = false) v3 ON v3.id = e3.dst
