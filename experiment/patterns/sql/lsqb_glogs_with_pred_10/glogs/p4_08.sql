SELECT COUNT(*)
FROM (SELECT * FROM comment_hascreator_person e1 WHERE e1.creationdate >= 1262304000000 AND e1.deletiondate >= 1356998400000) e1
JOIN comment_replyof_post e2 ON e2.src = e1.src
JOIN forum_containerof_post e3 ON e3.dst = e2.dst
JOIN forum_hasmember_person e4 ON e4.src = e3.src
JOIN (SELECT * FROM person v5 WHERE v5.gender = 'male') v5 ON v5.id = e4.dst
JOIN (SELECT * FROM forum v4 WHERE v4.explicitlydeleted = false) v4 ON v4.id = e3.src
