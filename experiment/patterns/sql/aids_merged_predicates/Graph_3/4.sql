select count(*) from vertex v1, vertex v2, vertex v3, 0 e1, 0 e2 where e1.src = v2.id and e1.dst = v1.id and e2.src = v3.id and e2.dst = v1.id and v1.v_mod10 <= 4 and e1.e_mod10 <= 6
