SELECT COUNT(*)
FROM comment_hascreator_person e1
JOIN comment_replyof_post e2 ON e2.src = e1.src
JOIN post_hascreator_person e3 ON e3.src = e2.dst
JOIN person_knows_person e4 ON e4.src = e1.dst AND e4.dst = e3.dst
JOIN person v2 ON v2.id = e1.dst
JOIN person v3 ON v3.id = e3.dst
JOIN post v4 ON v4.id = e2.dst
WHERE v2.birthday >= 315532800000 AND v3.creationdate >= 1271792357357 AND v4.browserused = 'Safari'
