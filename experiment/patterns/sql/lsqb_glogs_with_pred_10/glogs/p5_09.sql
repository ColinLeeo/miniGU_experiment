SELECT COUNT(*)
FROM (SELECT * FROM comment_replyof_comment e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_hascreator_person e2 ON e2.src = e1.src
JOIN comment_hascreator_person e3 ON e3.src = e1.dst
JOIN person_likes_comment e4 ON e4.src = e2.dst AND e4.dst = e1.dst
JOIN person_likes_comment e5 ON e5.src = e3.dst AND e5.dst = e1.src
JOIN (SELECT * FROM comment v1 WHERE v1.browserused = 'Chrome') v1 ON v1.id = e1.src
