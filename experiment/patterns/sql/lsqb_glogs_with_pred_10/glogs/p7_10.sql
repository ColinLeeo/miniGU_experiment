SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_hascreator_person e2 ON e2.dst = e1.src
JOIN comment_replyof_comment e3 ON e3.dst = e2.src
JOIN comment_hascreator_person e4 ON e4.src = e3.src AND e4.dst = e1.dst
JOIN forum_hasmember_person e5 ON e5.dst = e1.src
JOIN forum_hasmember_person e6 ON e6.src = e5.src AND e6.dst = e1.dst
JOIN (SELECT * FROM comment v4 WHERE v4.creationdate >= 1262304000000 AND v4.explicitlydeleted = false) v4 ON v4.id = e3.src
JOIN (SELECT * FROM person v1 WHERE v1.gender = 'male') v1 ON v1.id = e1.src
