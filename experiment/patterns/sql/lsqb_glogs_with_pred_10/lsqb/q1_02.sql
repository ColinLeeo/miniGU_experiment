SELECT COUNT(*)
FROM tag_hastype_tagclass e1
JOIN (SELECT * FROM comment_hastag_tag e2 WHERE e2.creationdate >= 1262304000000) e2 ON e2.dst = e1.src
JOIN comment_replyof_post e3 ON e3.src = e2.src
JOIN forum_containerof_post e4 ON e4.dst = e3.dst
JOIN forum_hasmember_person e5 ON e5.src = e4.src
JOIN person_islocatedin_city e6 ON e6.src = e5.dst
JOIN city_ispartof_country e7 ON e7.src = e6.dst
JOIN (SELECT * FROM person v2 WHERE v2.birthday <= 631152000000) v2 ON v2.id = e5.dst
JOIN (SELECT * FROM post v5 WHERE v5.explicitlydeleted = false) v5 ON v5.id = e3.dst
