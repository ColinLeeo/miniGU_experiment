SELECT COUNT(*)
FROM (SELECT * FROM post_hastag_tag e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN post_hascreator_person e2 ON e2.src = e1.src
JOIN comment_replyof_post e3 ON e3.dst = e1.src
JOIN person_likes_post e4 ON e4.dst = e1.src
JOIN (SELECT * FROM comment v4 WHERE v4.creationdate >= 1285917807738 AND v4.explicitlydeleted = false) v4 ON v4.id = e3.src
