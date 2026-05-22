SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000 AND e1.deletiondate >= 1356998400000) e1
JOIN comment_hascreator_person e2 ON e2.dst = e1.src
JOIN comment_hastag_tag e3 ON e3.src = e2.src
JOIN comment_hascreator_person e4 ON e4.dst = e1.dst
JOIN comment_replyof_comment e5 ON e5.src = e4.src AND e5.dst = e2.src
JOIN comment_hastag_tag e6 ON e6.src = e4.src AND e6.dst = e3.dst
JOIN (SELECT * FROM comment v4 WHERE v4.browserused = 'Chrome') v4 ON v4.id = e4.src
JOIN (SELECT * FROM comment v3 WHERE v3.explicitlydeleted = false) v3 ON v3.id = e2.src
