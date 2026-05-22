SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000 AND e1.deletiondate >= 1356998400000) e1
JOIN person_knows_person e2 ON e2.src = e1.dst
JOIN person_hasinterest_tag e3 ON e3.src = e2.dst
JOIN (SELECT * FROM person v3 WHERE v3.gender = 'male' AND v3.browserused = 'Chrome') v3 ON v3.id = e2.dst
