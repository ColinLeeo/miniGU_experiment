select count(*) from vertex v0, vertex v1, vertex v2, vertex v3, 0 e0, 0 e1, 1 e2 where e0.src = v0.id and e0.dst = v1.id and e1.src = v1.id and e1.dst = v2.id and e2.src = v3.id and e2.dst = v1.id
