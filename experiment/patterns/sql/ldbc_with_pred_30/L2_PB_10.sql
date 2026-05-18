SELECT COUNT(*)
FROM city_ispartof_country e1
JOIN person_islocatedin_city e2 ON e2.dst = e1.src
JOIN forum_hasmember_person e3 ON e3.dst = e2.src
JOIN forum_containerof_post e4 ON e4.src = e3.src
JOIN comment_replyof_post e5 ON e5.dst = e4.dst
JOIN comment_hastag_tag e6 ON e6.src = e5.src
JOIN tag_hastype_tagclass e7 ON e7.src = e6.dst
JOIN person v3 ON v3.id = e2.src
JOIN post v5 ON v5.id = e4.dst
JOIN comment v6 ON v6.id = e5.src
WHERE v3.deletiondate = 1577664000000 AND v5.browserused = 'Firefox' AND v6.deletiondate >= 1356998400000
