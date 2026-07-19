SELECT COUNT(*) FROM pe0_10_13 e1, pe0_13_10 e2, pe0_13_3 e3, pe0_10_3 e4 WHERE e2.src = e1.dst AND e3.src = e1.dst AND e4.src = e2.dst AND e4.dst = e3.dst
