SELECT COUNT(*)
FROM (SELECT * FROM comment_hascreator_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_knows_person e2 ON e2.src = e1.dst
JOIN post_hascreator_person e3 ON e3.dst = e2.dst
JOIN comment_replyof_post e4 ON e4.src = e1.src AND e4.dst = e3.src
JOIN (SELECT * FROM comment v4 WHERE v4.browserused = 'Chrome') v4 ON v4.id = e1.src
