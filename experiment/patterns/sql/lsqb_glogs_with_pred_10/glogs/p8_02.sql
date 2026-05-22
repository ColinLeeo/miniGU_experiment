SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_hascreator_person e2 ON e2.dst = e1.src
JOIN comment_hastag_tag e3 ON e3.src = e2.src
JOIN comment_hascreator_person e4 ON e4.dst = e1.dst
JOIN comment_replyof_comment e5 ON e5.src = e4.src AND e5.dst = e2.src
JOIN comment_hastag_tag e6 ON e6.src = e4.src AND e6.dst = e3.dst
JOIN (SELECT * FROM person v2 WHERE v2.gender = 'male') v2 ON v2.id = e1.dst
