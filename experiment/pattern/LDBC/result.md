
=== Results ===
sf  pattern_group    pattern  tool      cardinality    time_s       status
1   L1_path          L1       pathce    4834972697     0.001390794  ok
1   L2_chain         L2       pathce    1570013963     0.004715028  ok
1   L3_star          L3       pathce    5502799        0.000810236  ok
1   L4_cycle         L4       pathce    1194196        0.015280658  ok
1   L5_triangle      L5       pathce    121473793      0.042301892  ok
1   L6_cycle_branch  L6       pathce    2801462        0.054351986  ok
1   L1_path          L1       gcare-wj  1749904286.92  0.004799     ok
1   L2_chain         L2       gcare-wj  343068074.66   0.029756     ok
1   L3_star          L3       gcare-wj  4425476.50     0.027224     ok
1   L4_cycle         L4       gcare-wj  482120.52      0.039363     ok
1   L5_triangle      L5       gcare-wj  0.00           0.004160     ok
1   L6_cycle_branch  L6       gcare-wj  0.00           0.043804     ok
1   L1_path          L1       color     607194675.82   0.000138     ok
1   L2_chain         L2       color     4.30           0.000169     ok
1   L3_star          L3       color     255687.36      0.000109     ok
1   L4_cycle         L4       color     3033.60        0.000361     ok
1   L5_triangle      L5       color     1.00           0.003149     ok
1   L6_cycle_branch  L6       color     83.19          0.000536     ok
1   L1_path          L1       truth     1662673006     0            ok
1   L2_chain         L2       truth     216673831      0            ok
1   L3_star          L3       truth     -1             0            timeout
1   L4_cycle         L4       truth     -1             0            timeout
1   L5_triangle      L5       truth     -1             0            timeout
1   L6_cycle_branch  L6       truth     -1             0            timeout




=== Summary ===
  L1                      1,662,673,006  (1.31s)
  L1_PA                     438,799,026  (0.80s)
  L2                        216,673,831  (0.49s)
  L2_PB                       1,511,619  (0.53s)
  L3                          5,502,799  (0.21s)
  L3_PB                               0  (0.32s)
  L4                          1,019,467  (2.79s)
  L4_PA                         251,387  (0.30s)
  L5                            706,992  (29.43s)
  L5_PB                               0  (2.85s)
  L6                             62,418  (51.67s)
  L6_PA                           2,422  (0.60s)



Catalog loaded for graph 'ldbc'
total: 1, min_es index: Some(1), is highest: true, L1, cardinality: 5674635264
Time: 0.001s
total: 1, min_es index: Some(1), is highest: true, L1, cardinality: 5676990464
Time: 0.000s
total: 1, min_es index: Some(1), is highest: true, L1_PA, cardinality: 3506032568
Time: 13.119s
total: 1, min_es index: Some(1), is highest: true, L1_PA, cardinality: 3437258165
Time: 12.798s
total: 1, min_es index: Some(1), is highest: true, L2, cardinality: 5349950464
Time: 0.024s
total: 1, min_es index: Some(1), is highest: true, L2, cardinality: 5349965824
Time: 0.022s
total: 1, min_es index: Some(1), is highest: true, L2_PB, cardinality: 2117950576
Time: 5.617s
total: 1, min_es index: Some(1), is highest: true, L2_PB, cardinality: 37927372
Time: 5.765s
total: 1, min_es index: Some(1), is highest: true, L3, cardinality: 790427776
Time: 0.055s
total: 1, min_es index: Some(1), is highest: true, L3, cardinality: 790430720
Time: 0.052s
total: 1, min_es index: Some(1), is highest: true, L3_PB, cardinality: 53
Time: 0.790s
total: 1, min_es index: None, is highest: false, L3_PB, cardinality: 0
Time: 0.786s
total: 4, min_es index: Some(1), is highest: true, L4, cardinality: 2391707
Time: 0.126s
total: 4, min_es index: Some(1), is highest: true, L4, cardinality: 2391707
Time: 0.125s
total: 4, min_es index: Some(4), is highest: false, L4_PA, cardinality: 1074816
Time: 91.803s
total: 4, min_es index: Some(4), is highest: false, L4_PA, cardinality: 540741
Time: 94.166s
total: 49, min_es index: Some(1), is highest: true, L5, cardinality: 644285440
Time: 0.010s
total: 49, min_es index: Some(1), is highest: true, L5, cardinality: 644285440
Time: 0.010s

