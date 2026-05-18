SELECT COUNT(*)
FROM comment_replyof_post e1
JOIN post_hascreator_person e2 ON e2.src = e1.dst
JOIN post_hastag_tag e3 ON e3.src = e1.dst
JOIN person_likes_post e4 ON e4.dst = e1.dst
JOIN comment v1 ON v1.id = e1.src
JOIN person v2 ON v2.id = e2.dst
JOIN person v3 ON v3.id = e4.src
WHERE v1.browserused = 'Chrome' AND v2.creationdate >= 1347672536188 AND v3.browserused = 'Safari'
