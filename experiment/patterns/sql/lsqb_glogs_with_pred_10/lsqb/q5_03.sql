SELECT COUNT(*)
FROM (SELECT * FROM comment_hastag_tag e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_replyof_post e2 ON e2.src = e1.src
JOIN post_hastag_tag e3 ON e3.src = e2.dst
JOIN (SELECT * FROM comment v2 WHERE v2.creationdate <= 1285917807738 AND v2.explicitlydeleted = false) v2 ON v2.id = e1.src
JOIN (SELECT * FROM post v3 WHERE v3.browserused = 'Chrome') v3 ON v3.id = e2.dst
