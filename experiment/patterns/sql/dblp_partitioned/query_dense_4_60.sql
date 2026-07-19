SELECT COUNT(*) FROM pe0_2_4 e1, pe0_2_1 e2, pe0_4_1 e3, pe0_4_9 e4 WHERE e2.src = e1.src AND e3.src = e1.dst AND e3.dst = e2.dst AND e4.src = e1.dst
