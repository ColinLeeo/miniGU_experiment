SELECT COUNT(*)
FROM (SELECT * FROM person_likes_comment e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_hascreator_person e2 ON e2.src = e1.dst
JOIN person_islocatedin_city e3 ON e3.src = e1.src
JOIN person_islocatedin_city e4 ON e4.src = e2.dst AND e4.dst = e3.dst
JOIN person_islocatedin_city e5 ON e5.dst = e3.dst
JOIN person_islocatedin_city e6 ON e6.dst = e3.dst
JOIN person_likes_comment e7 ON e7.src = e5.src
JOIN comment_hascreator_person e8 ON e8.src = e7.dst AND e8.dst = e6.src
JOIN (SELECT * FROM person v3 WHERE v3.birthday >= 447811200000) v3 ON v3.id = e2.dst
JOIN (SELECT * FROM comment v6 WHERE v6.explicitlydeleted = false) v6 ON v6.id = e7.dst
