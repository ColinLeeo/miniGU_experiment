SELECT COUNT(*)
FROM (SELECT * FROM post_hastag_tag e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN post_hascreator_person e2 ON e2.src = e1.src
JOIN comment_replyof_post e3 ON e3.dst = e1.src
JOIN person_likes_post e4 ON e4.dst = e1.src
JOIN (SELECT * FROM person v3 WHERE v3.birthday >= 520905600000) v3 ON v3.id = e2.dst
JOIN (SELECT * FROM comment v4 WHERE v4.explicitlydeleted = false AND v4.browserused = 'Chrome') v4 ON v4.id = e3.src
