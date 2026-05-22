SELECT COUNT(*)
FROM (SELECT * FROM comment_hastag_tag e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN comment_replyof_post e2 ON e2.src = e1.src
JOIN post_hastag_tag e3 ON e3.src = e2.dst
JOIN comment_hastag_tag e4 ON e4.src = e1.src AND e4.dst = e3.dst
JOIN (SELECT * FROM comment v2 WHERE v2.browserused = 'Firefox') v2 ON v2.id = e1.src
