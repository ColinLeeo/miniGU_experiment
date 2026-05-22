SELECT COUNT(*)
FROM (SELECT * FROM post_hastag_tag e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN post_hascreator_person e2 ON e2.src = e1.src
JOIN comment_replyof_post e3 ON e3.dst = e1.src
JOIN person_likes_post e4 ON e4.dst = e1.src
JOIN (SELECT * FROM person v5 WHERE v5.gender = 'male') v5 ON v5.id = e4.src
