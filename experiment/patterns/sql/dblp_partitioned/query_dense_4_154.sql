SELECT COUNT(*) FROM pe0_10_5 e1, pe0_5_1 e2, pe0_5_13 e3, pe0_1_13 e4 WHERE e2.src = e1.dst AND e3.src = e1.dst AND e4.src = e2.dst AND e4.dst = e3.dst
