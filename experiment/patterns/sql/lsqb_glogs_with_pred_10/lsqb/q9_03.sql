SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_knows_person e2 ON e2.src = e1.dst
JOIN person_hasinterest_tag e3 ON e3.src = e2.dst
JOIN person_knows_person e4 ON e4.src = e1.src AND e4.dst = e2.dst
JOIN (SELECT * FROM person v2 WHERE v2.birthday >= 315532800000 AND v2.gender = 'male' AND v2.creationdate >= 1262304000000) v2 ON v2.id = e1.dst
