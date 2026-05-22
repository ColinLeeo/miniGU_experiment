SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_knows_person e2 ON e2.src = e1.dst
JOIN person_hasinterest_tag e3 ON e3.src = e2.dst
JOIN person_knows_person e4 ON e4.src = e1.src AND e4.dst = e2.dst
JOIN (SELECT * FROM person v1 WHERE v1.birthday >= 520905600000) v1 ON v1.id = e1.src
JOIN (SELECT * FROM person v2 WHERE v2.gender = 'male') v2 ON v2.id = e1.dst
