# LDBC SNB Query Patterns for GCard Experiments

## Schema

```
Vertex Labels: person, comment, post, forum, city, country, tag, tagclass
Edge Labels:
  person_knows_person, person_likes_comment, person_likes_post,
  person_islocatedin_city, person_hasinterest_tag,
  comment_hascreator_person, comment_replyof_post, comment_replyof_comment,
  comment_hastag_tag, post_hascreator_person, post_hastag_tag,
  forum_containerof_post, forum_hasmember_person,
  city_ispartof_country, tag_hastype_tagclass

Vertex Properties:
  person:  birthday (Int64), gender (String: "male"/"female"), creationdate (Int64), language (String)
  comment: creationdate (Int64), length (Int64)
  post:    creationdate (Int64), length (Int64)
  forum:   creationdate (Int64)
  tag:     name (String)

Edge Properties:
  creationdate (Int64), explicitlydeleted (Boolean)
```

---

## Query Summary

| ID | Source | Structure | Nodes | Edges | Cycle | Predicate | Description |
|----|--------|-----------|-------|-------|-------|-----------|-------------|
| L1 | LSQB Q1 | chain-7 | 8 | 7 | No | **PB (scattered)** | Country-City-Person-Forum-Post-Comment-Tag-TagClass |
| L2 | LSQB Q2 | cycle | 4 | 4 | **Yes** | **PA (clustered)** | Person₁-knows-Person₂, Comment-replyOf-Post 构成环 |
| L3 | LSQB Q3 | triangle+geo | 7 | 9 | **Yes** | **PB (scattered)** | 三人互 knows, 各自 isLocatedIn City isPartOf 同一 Country |
| L4 | LSQB Q4 | star-4 | 5 | 4 | No | **PB (scattered)** | Post 为中心, 连 Tag/Creator/Liker/Comment |
| L5 | LSQB Q6 | path-3 | 4 | 3 | No | **PA (clustered)** | Person-knows-Person-knows-Person-hasInterest-Tag |
| L6 | Custom (p8) | cycle+branch | 5 | 6 | **Yes** | **PA (clustered)** | Person₁-knows-Person₂, 各创建 Comment, replyOf + 共享 Tag |

Total: 6 base queries + 6 with predicates = **12 query configurations**

- PA (clustered): L2, L5, L6 — predicates on adjacent/nearby nodes
- PB (scattered): L1, L3, L4 — predicates spread across distant nodes

---

## L1: Long Chain (LSQB Q1)

Original Cypher:
```cypher
MATCH (:Country)<-[:IS_PART_OF]-(:City)<-[:IS_LOCATED_IN]-(:Person)
      <-[:HAS_MEMBER]-(:Forum)-[:CONTAINER_OF]->(:Post)
      <-[:REPLY_OF]-(:Comment)-[:HAS_TAG]->(:Tag)-[:HAS_TYPE]->(:TagClass)
RETURN count(*) AS count
```

```
Country ←isPartOf── City ←isLocatedIn── Person ←hasMember── Forum
                                                              ↓ containerOf
TagClass ←hasType── Tag ←hasTag── Comment ──replyOf──→ Post
```

- 8 nodes, 7 edges, acyclic
- Longest chain in LSQB, tests path decomposition

### L1-PB (Predicate Scattered)
Predicates on both ends and middle (node + edge mixed):
```
WHERE Person.language = 'zh;en'              (node, left side)
  AND hasMember.creationdate > 2012-01-01    (edge, middle)
  AND Comment.creationdate > 2012-01-01      (node, right side)
```

---

## L2: Cycle (LSQB Q2)

Original Cypher:
```cypher
MATCH (person1:Person)-[:KNOWS]-(person2:Person),
      (person1)<-[:HAS_CREATOR]-(comment:Comment)
        -[:REPLY_OF]->(post:Post)-[:HAS_CREATOR]->(person2)
RETURN count(*) AS count
```

```
Person₁ ──knows── Person₂
    ↑ hasCreator         ↑ hasCreator
    Comment ──replyOf──→ Post
```

- 4 nodes, 4 edges, **cyclic**
- Smallest cycle in LSQB: Person₁-knows-Person₂-hasCreator←Post-replyOf←Comment-hasCreator→Person₁

### L2-PA (Predicate Clustered)
Predicates on adjacent Person₁, Person₂ and the edge between them (cycle top):
```
WHERE Person₁.gender = 'female'              (node)
  AND Person₂.gender = 'male'                (node)
  AND knows.creationdate > 2012-01-01        (edge, between P₁-P₂)
```

---

## L3: Triangle + Geography (LSQB Q3)

Original Cypher:
```cypher
MATCH (country:Country)
MATCH (person1:Person)-[:IS_LOCATED_IN]->(city1:City)-[:IS_PART_OF]->(country)
MATCH (person2:Person)-[:IS_LOCATED_IN]->(city2:City)-[:IS_PART_OF]->(country)
MATCH (person3:Person)-[:IS_LOCATED_IN]->(city3:City)-[:IS_PART_OF]->(country)
MATCH (person1)-[:KNOWS]-(person2)-[:KNOWS]-(person3)-[:KNOWS]-(person1)
RETURN count(*) AS count
```

```
Person₁ ──knows── Person₂ ──knows── Person₃
    │                │                  │
    └──knows─────────┼──────────────────┘
    ↓ isLocatedIn    ↓ isLocatedIn     ↓ isLocatedIn
   City₁            City₂             City₃
    ↓ isPartOf       ↓ isPartOf        ↓ isPartOf
                  Country
```

- 7 nodes, 9 edges, **cyclic** (triangle of KNOWS)
- Most complex LSQB query

### L3-PB (Predicate Scattered)
Predicates on different attributes across cycle and branches:
```
WHERE Person₁.gender = 'female'      (cycle node)
  AND Person₃.birthday > 1990-01-01  (opposite cycle node)
  AND Person₂.language = 'en'        (third cycle node, different attribute)
```

---

## L4: Star (LSQB Q4)

Original Cypher:
```cypher
MATCH (:Tag)<-[:HAS_TAG]-(message:Message)-[:HAS_CREATOR]->(creator:Person),
      (message)<-[:LIKES]-(liker:Person),
      (message)<-[:REPLY_OF]-(comment:Comment)
RETURN count(*) AS count
```

Note: In the actual LDBC SNB data, Message is split into Post and Comment.
The existing benchmark JSON uses Post as the center node.

```
    Tag ←hasTag── Post ──hasCreator──→ Creator(Person)
                    ↑ likes
                 Liker(Person)
                    ↑ replyOf
                 Comment
```

- 5 nodes (Tag, Post, Person(creator), Person(liker), Comment), 4 edges, acyclic
- Star centered on Post

### L4-PB (Predicate Scattered)
Predicates on 3 different branches (node + edge mixed):
```
WHERE Creator.birthday > 1990-01-01          (node, creator branch)
  AND likes.creationdate > 2012-06-01        (edge, liker branch)
  AND Comment.creationdate > 2012-01-01      (node, comment branch)
```

---

## L5: Path (LSQB Q6)

Original Cypher:
```cypher
MATCH (person1:Person)-[:KNOWS]-(person2:Person)
      -[:KNOWS]-(person3:Person)-[:HAS_INTEREST]->(tag:Tag)
WHERE person1 <> person3
RETURN count(*) AS count
```

```
Person₁ ──knows── Person₂ ──knows── Person₃ ──hasInterest──→ Tag
```

- 4 nodes, 3 edges, acyclic
- person1 <> person3 (inequality filter, not a structural edge)

### L5-PA (Predicate Clustered)
Predicates concentrated on adjacent Person₁, Person₂ and the edge between them:
```
WHERE Person₁.gender = 'female'              (node)
  AND Person₂.gender = 'male'                (node)
  AND knows₁.creationdate > 2012-01-01       (edge, P₁-P₂)
```

---

## L6: Cycle + Branch (Custom, based on LSQB p8)

This pattern does NOT exist in LSQB Q1-Q9. It is based on the p8 pattern
used in existing benchmarks, which combines a cycle with branch structure.

```
Person₁ ──knows── Person₂
    ↑ hasCreator         ↑ hasCreator
 Comment₁ ──replyOf──→ Comment₂
    ↓ hasTag             ↓ hasTag
         Tag (same)
```

- 5 nodes (Person₁, Person₂, Comment₁, Comment₂, Tag), 6 edges, **cyclic**
- Cycle: Person₁-knows-Person₂-hasCreator←Comment₂-replyOf←Comment₁-hasCreator→Person₁
- Branch: both Comments share the same Tag (creates additional structural constraint)

### L6-PA (Predicate Clustered)
Predicates on adjacent Person₁, Person₂ and nearby edge:
```
WHERE Person₁.gender = 'male'                (node)
  AND Person₂.gender = 'male'                (node)
  AND replyOf.creationdate > 2012-01-01      (edge, Comment₁→Comment₂)
```

---

## Predicate Design Summary

| Query | Style | Node Predicates | Edge Predicates | Location |
|-------|-------|----------------|-----------------|----------|
| L1-PB | Scattered | Person.language + Comment.creationdate | hasMember.creationdate | Left(node)/middle(edge)/right(node) |
| L2-PA | Clustered | Person₁.gender + Person₂.gender | knows.creationdate | Cycle top: 2 nodes + 1 edge |
| L3-PB | Scattered | Person₁.gender + Person₃.birthday + Person₂.language | — | 3 cycle nodes, different attrs |
| L4-PB | Scattered | Creator.birthday + Comment.creationdate | likes.creationdate | 3 branches: node/edge/node |
| L5-PA | Clustered | Person₁.gender + Person₂.gender | knows₁.creationdate | Path head: 2 nodes + 1 edge |
| L6-PA | Clustered | Person₁.gender + Person₂.gender | replyOf.creationdate | Cycle top: 2 nodes + 1 edge |

---

## File Index

```
pattern/LDBC/
├── README.md
├── L1_chain/
│   ├── L1.json
│   └── L1_PB.json        # Scattered predicates
├── L2_cycle/
│   ├── L2.json
│   └── L2_PA.json        # Clustered predicates
├── L3_triangle/
│   ├── L3.json
│   └── L3_PB.json        # Scattered predicates
├── L4_star/
│   ├── L4.json
│   └── L4_PB.json        # Scattered predicates
├── L5_path/
│   ├── L5.json
│   └── L5_PA.json        # Clustered predicates
└── L6_cycle_branch/
    ├── L6.json
    └── L6_PA.json        # Clustered predicates
```



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

