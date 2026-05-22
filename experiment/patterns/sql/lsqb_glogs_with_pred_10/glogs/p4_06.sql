SELECT COUNT(*)
FROM (SELECT * FROM comment_hascreator_person e1 WHERE e1.creationdate >= 1262304000000 AND e1.deletiondate >= 1356998400000) e1
JOIN comment_replyof_post e2 ON e2.src = e1.src
JOIN forum_containerof_post e3 ON e3.dst = e2.dst
JOIN forum_hasmember_person e4 ON e4.src = e3.src
JOIN (SELECT * FROM post v3 WHERE v3.browserused = 'Chrome') v3 ON v3.id = e2.dst
JOIN (SELECT * FROM comment v2 WHERE v2.explicitlydeleted = false) v2 ON v2.id = e1.src
