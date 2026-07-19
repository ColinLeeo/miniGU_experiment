SELECT COUNT(*) FROM v9 v1, v3 v2, v1 v3, v3 v4, e0 e1, e0 e2, e0 e3 WHERE e1.src = v1.id AND e1.dst = v2.id AND e2.src = v2.id AND e2.dst = v3.id AND e3.src = v3.id AND e3.dst = v4.id
