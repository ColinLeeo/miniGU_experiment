SELECT COUNT(*)
FROM person_knows_person e1
JOIN person_knows_person e2 ON e2.src = e1.dst
JOIN person_hasinterest_tag e3 ON e3.src = e2.dst
JOIN person v1 ON v1.id = e1.src
JOIN person v2 ON v2.id = e1.dst
JOIN person v3 ON v3.id = e2.dst
WHERE v1.creationdate >= 1285917807738 AND v2.deletiondate = 1577664000000 AND v3.birthday >= 550800000000
