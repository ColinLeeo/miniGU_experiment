SELECT COUNT(*)
FROM comment_hascreator_person e1
CROSS JOIN comment_hascreator_person e2
JOIN comment_hastag_tag e3 ON e3.src = e1.src
JOIN comment_hastag_tag e4 ON e4.src = e2.src AND e4.dst = e3.dst
JOIN person_knows_person e5 ON e5.src = e1.dst AND e5.dst = e2.dst
JOIN comment_replyof_comment e6 ON e6.src = e1.src AND e6.dst = e2.src
JOIN comment v1 ON v1.id = e1.src
JOIN person v3 ON v3.id = e1.dst
JOIN person v4 ON v4.id = e2.dst
WHERE v1.creationdate >= 1351864583841 AND v3.creationdate >= 1333330850691 AND v4.browserused = 'Safari'
