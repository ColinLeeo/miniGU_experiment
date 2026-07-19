SELECT COUNT(*) FROM pe0_8_9 e1, pe0_9_8 e2, pe0_9_6 e3, pe0_8_6 e4 WHERE e2.src = e1.dst AND e3.src = e1.dst AND e4.src = e2.dst AND e4.dst = e3.dst
