SELECT COUNT(*) FROM pe0_4_5 e1, pe0_5_13 e2, pe0_5_5 e3, pe0_13_5 e4 WHERE e2.src = e1.dst AND e3.src = e1.dst AND e4.src = e2.dst AND e4.dst = e3.dst
