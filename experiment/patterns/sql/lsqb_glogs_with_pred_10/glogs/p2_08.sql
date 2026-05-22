SELECT COUNT(*)
FROM (SELECT * FROM person_islocatedin_city e1 WHERE e1.creationdate <= 1285917807738 AND e1.deletiondate >= 1356998400000) e1
JOIN person_islocatedin_city e2 ON e2.dst = e1.dst
JOIN person_likes_comment e3 ON e3.src = e2.src
JOIN comment_hascreator_person e4 ON e4.src = e3.dst AND e4.dst = e1.src
JOIN (SELECT * FROM person v1 WHERE v1.gender = 'male') v1 ON v1.id = e1.src
JOIN (SELECT * FROM comment v3 WHERE v3.explicitlydeleted = false) v3 ON v3.id = e3.dst
