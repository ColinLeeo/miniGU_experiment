SELECT COUNT(*)
FROM (SELECT * FROM person_knows_person e1 WHERE e1.creationdate >= 1262304000000) e1
JOIN person_knows_person e2 ON e2.src = e1.src
JOIN person_knows_person e3 ON e3.src = e1.dst AND e3.dst = e2.dst
JOIN person_islocatedin_city e4 ON e4.src = e1.src
JOIN person_islocatedin_city e5 ON e5.src = e1.dst
JOIN person_islocatedin_city e6 ON e6.src = e2.dst
JOIN city_ispartof_country e7 ON e7.src = e4.dst
JOIN city_ispartof_country e8 ON e8.src = e5.dst AND e8.dst = e7.dst
JOIN city_ispartof_country e9 ON e9.src = e6.dst AND e9.dst = e7.dst
JOIN (SELECT * FROM person v1 WHERE v1.birthday >= 315532800000) v1 ON v1.id = e1.src
