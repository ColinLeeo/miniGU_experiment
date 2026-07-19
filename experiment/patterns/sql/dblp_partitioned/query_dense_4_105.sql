SELECT COUNT(*) FROM pe0_1_10 e1, pe0_10_12 e2, pe0_10_12 e3, pe0_12_12 e4 WHERE e2.src = e1.dst AND e3.src = e1.dst AND e4.src = e2.dst AND e4.dst = e3.dst
