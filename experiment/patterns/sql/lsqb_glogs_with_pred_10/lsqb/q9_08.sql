SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_knows_person e2 ON e2.src = e1.dst
JOIN person_hasinterest_tag e3 ON e3.src = e2.dst
JOIN person_knows_person e4 ON e4.src = e1.src AND e4.dst = e2.dst
JOIN (SELECT * FROM tag v4 WHERE v4.name = 'Rumi') v4 ON v4.id = e3.dst
