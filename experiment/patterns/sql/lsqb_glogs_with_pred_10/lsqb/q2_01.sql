SELECT COUNT(*)
FROM (SELECT * FROM comment_hascreator_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_knows_person e2 ON e2.src = e1.dst
JOIN post_hascreator_person e3 ON e3.dst = e2.dst
JOIN comment_replyof_post e4 ON e4.src = e1.src AND e4.dst = e3.src
JOIN (SELECT * FROM person v1 WHERE v1.birthday >= 520905600000) v1 ON v1.id = e1.dst
JOIN (SELECT * FROM post v3 WHERE v3.explicitlydeleted = false) v3 ON v3.id = e3.src
