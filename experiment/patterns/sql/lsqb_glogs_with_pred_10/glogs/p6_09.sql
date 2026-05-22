SELECT COUNT(*)
FROM (SELECT * FROM forum_hasmember_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN forum_hasmember_person e2 ON e2.src = e1.src
JOIN forum_containerof_post e3 ON e3.src = e1.src
JOIN person_knows_person e4 ON e4.src = e1.dst AND e4.dst = e2.dst
JOIN person_likes_post e5 ON e5.src = e1.dst AND e5.dst = e3.dst
JOIN person_likes_post e6 ON e6.src = e2.dst AND e6.dst = e3.dst
JOIN (SELECT * FROM post v4 WHERE v4.creationdate >= 1262304000000 AND v4.explicitlydeleted = false AND v4.browserused = 'Internet Explorer') v4 ON v4.id = e3.dst
