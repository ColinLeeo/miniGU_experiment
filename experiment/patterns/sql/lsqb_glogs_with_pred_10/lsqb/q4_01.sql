SELECT COUNT(*)
FROM (SELECT * FROM post_hastag_tag e1 WHERE e1.creationdate >= 1262304000000 AND e1.deletiondate >= 1356998400000) e1
JOIN post_hascreator_person e2 ON e2.src = e1.src
JOIN comment_replyof_post e3 ON e3.dst = e1.src
JOIN person_likes_post e4 ON e4.dst = e1.src
JOIN (SELECT * FROM post v1 WHERE v1.language = 'fa' AND v1.explicitlydeleted = false) v1 ON v1.id = e1.src
