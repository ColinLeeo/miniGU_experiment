SELECT COUNT(*) FROM pe0_3_5 e1, pe0_5_9 e2, pe0_5_10 e3, pe0_9_10 e4 WHERE e2.src = e1.dst AND e3.src = e1.dst AND e4.src = e2.dst AND e4.dst = e3.dst
