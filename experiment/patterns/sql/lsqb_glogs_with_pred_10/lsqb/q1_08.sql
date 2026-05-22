SELECT COUNT(*)
FROM tag_hastype_tagclass e1
JOIN comment_hastag_tag e2 ON e2.dst = e1.src
JOIN comment_replyof_post e3 ON e3.src = e2.src
JOIN forum_containerof_post e4 ON e4.dst = e3.dst
JOIN forum_hasmember_person e5 ON e5.src = e4.src
JOIN person_islocatedin_city e6 ON e6.src = e5.dst
JOIN city_ispartof_country e7 ON e7.src = e6.dst
JOIN (SELECT * FROM comment v6 WHERE v6.creationdate <= 1325376000000 AND v6.explicitlydeleted = false) v6 ON v6.id = e2.src
