SELECT COUNT(*)
FROM (SELECT * FROM comment_hastag_tag e1 WHERE e1.creationdate >= 1325376000000) e1
JOIN comment_replyof_post e2 ON e2.src = e1.src
JOIN post_hastag_tag e3 ON e3.src = e2.dst
JOIN (SELECT * FROM tag v1 WHERE v1.name = 'Rumi') v1 ON v1.id = e1.dst
