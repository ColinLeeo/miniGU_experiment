SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_hascreator_person e2 ON e2.dst = e1.src
JOIN comment_replyof_comment e3 ON e3.dst = e2.src
JOIN comment_hascreator_person e4 ON e4.src = e3.src AND e4.dst = e1.dst
JOIN forum_hasmember_person e5 ON e5.dst = e1.src
JOIN forum_hasmember_person e6 ON e6.src = e5.src AND e6.dst = e1.dst
JOIN (SELECT * FROM comment v4 WHERE v4.length >= 2) v4 ON v4.id = e3.src
JOIN (SELECT * FROM comment v3 WHERE v3.explicitlydeleted = false) v3 ON v3.id = e2.src
