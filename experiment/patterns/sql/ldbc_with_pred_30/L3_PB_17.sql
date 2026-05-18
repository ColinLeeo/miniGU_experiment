SELECT COUNT(*)
FROM comment_replyof_post e1
JOIN post_hascreator_person e2 ON e2.src = e1.dst
JOIN post_hastag_tag e3 ON e3.src = e1.dst
JOIN person_likes_post e4 ON e4.dst = e1.dst
JOIN comment v1 ON v1.id = e1.src
JOIN post v4 ON v4.id = e1.dst
JOIN person v3 ON v3.id = e4.src
WHERE v1.browserused = 'Safari' AND v4.length >= 150 AND v3.deletiondate = 1577664000000
