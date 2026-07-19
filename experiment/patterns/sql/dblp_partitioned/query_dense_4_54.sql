SELECT COUNT(*) FROM pe0_2_2 e1, pe0_2_2 e2, pe0_2_5 e3, pe0_2_5 e4 WHERE e2.src = e1.dst AND e3.src = e1.dst AND e4.src = e2.dst AND e4.dst = e3.dst
