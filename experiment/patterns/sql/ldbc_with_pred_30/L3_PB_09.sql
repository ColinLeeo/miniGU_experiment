SELECT COUNT(*)
FROM comment_replyof_post e1
JOIN post_hascreator_person e2 ON e2.src = e1.dst
JOIN post_hastag_tag e3 ON e3.src = e1.dst
JOIN person_likes_post e4 ON e4.dst = e1.dst
JOIN comment v1 ON v1.id = e1.src
JOIN person v2 ON v2.id = e2.dst
JOIN person v3 ON v3.id = e4.src
JOIN post v4 ON v4.id = e1.dst
WHERE v1.creationdate >= 1326464435099 AND v2.browserused = 'Safari' AND v3.creationdate >= 1309606682229 AND v4.browserused = 'Firefox'
