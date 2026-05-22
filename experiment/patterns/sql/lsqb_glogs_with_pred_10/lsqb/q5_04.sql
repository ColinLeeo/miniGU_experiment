SELECT COUNT(*)
FROM (SELECT * FROM comment_hastag_tag e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_replyof_post e2 ON e2.src = e1.src
JOIN post_hastag_tag e3 ON e3.src = e2.dst
JOIN (SELECT * FROM post v3 WHERE v3.browserused = 'Chrome' AND v3.explicitlydeleted = false) v3 ON v3.id = e2.dst
