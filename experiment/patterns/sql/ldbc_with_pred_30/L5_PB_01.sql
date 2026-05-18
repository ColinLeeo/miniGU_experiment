SELECT COUNT(*)
FROM city_ispartof_country e1
JOIN city_ispartof_country e2 ON e2.dst = e1.dst
JOIN city_ispartof_country e3 ON e3.dst = e1.dst
CROSS JOIN person_knows_person e4
JOIN person_knows_person e5 ON e5.src = e4.dst
JOIN person_knows_person e6 ON e6.src = e4.src AND e6.dst = e5.dst
JOIN person_islocatedin_city e7 ON e7.src = e4.src AND e7.dst = e1.src
JOIN person_islocatedin_city e8 ON e8.src = e4.dst AND e8.dst = e2.src
JOIN person_islocatedin_city e9 ON e9.src = e5.dst AND e9.dst = e3.src
JOIN person v5 ON v5.id = e4.src
JOIN person v6 ON v6.id = e4.dst
JOIN person v7 ON v7.id = e5.dst
WHERE v5.gender = 'female' AND v6.birthday >= 346550400000 AND v7.creationdate >= 1285917807738
