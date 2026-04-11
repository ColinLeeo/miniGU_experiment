# GCard Query：A-B-C-D 链式查询的基数估计流程

## 查询定义

```
A -e1→ B -e2→ C -e3→ D
```

4 个查询顶点，3 条查询边，无环，无谓词（IGNORE 模式）。
参数：`max_path_length k = 2`，`max_subgraphs = 1`。

---

## 总体流程

```
JSON 输入
  ↓
[1] 解析 → QueryGraph
  ↓
[2] 环检测（union-find）→ 无环
  ↓
[3] 找 Pivot 节点（度 ≥ 3）→ 无 Pivot
  ↓
[4] 路径分解 → 单条路径 A-B-C-D
  ↓
[5] 路径切分为 AbstractEdge（每段 ≤ k 跳）
  ↓
[6] 查 Catalog：为每条 AbstractEdge 填充 PCF
  ↓
[7] 构建 AbstractGraph
  ↓
[8] get_es()：拓扑树规约 → 输出基数
```

---

## Step 1：解析 QueryGraph

JSON 输入包含顶点 `{A, B, C, D}` 和边 `{e1: A→B, e2: B→C, e3: C→D}`。
构建邻接结构：

```
QueryGraph:
  顶点: A(id=1), B(id=2), C(id=3), D(id=4)
  边:   e1(1→2), e2(2→3), e3(3→4)
  度:   A=1, B=2, C=2, D=1
```

---

## Step 2：环检测

使用 union-find 遍历所有边：

```
union(A, B) → 成功（不同集合）
union(B, C) → 成功
union(C, D) → 成功
→ 无环 → 直接进入单 AbstractGraph 构建路径，不需要枚举生成树
```

---

## Step 3：找 Pivot 节点

Pivot 定义：度 ≥ 3 的顶点（有多条分支，需要作为路径端点）。

```
A: degree=1  ✗
B: degree=2  ✗
C: degree=2  ✗
D: degree=1  ✗
→ pivot_nodes = {}（空集）
```

---

## Step 4：路径分解

`pivot_nodes` 为空 → 调用 `build_path_from_entire_graph`：

1. 找度为 1 的顶点作为起点：A（或 D，取 id 最小者）
2. 从 A 出发，沿未访问邻居贪心遍历：A → B → C → D
3. 返回单条路径：

```
Path {
  start: A, end: D,
  vertices: [A, B, C, D],
  edges:    [e1, e2, e3]
}
```

---

## Step 5：路径切分为 AbstractEdge（k=2）

`build_abstract_edges_for_path`：路径长度 l=3，k=2。

```
num_abstract_edges = ceil(3/2) = 2
l % k = 1 ≠ 0 → 调用 build_abstract_edges_optimal
short_size = 3 - (2-1)*2 = 1
```

枚举短段放置位置，最小化谓词数量（本例无谓词，默认取位置 0）：

| 方案 | AE0 | AE1 |
|------|-----|-----|
| 短段在位置 0 | A→B（1跳，edges=[e1]） | B→D（2跳，edges=[e2,e3]） |
| 短段在位置 1 | A→C（2跳，edges=[e1,e2]） | C→D（1跳，edges=[e3]） |

选择方案（假设选位置 1，短段少谓词优先）：

```
AE0: src=A, dst=C, path_vertices=[A,B,C], original_edges=[e1,e2]
AE1: src=C, dst=D, path_vertices=[C,D],   original_edges=[e3]
```

> **注意**：k=1 时 3 条原始边变成 3 个 AbstractEdge，每条一一对应。

---

## Step 6：查 Catalog 填充 PCF

`fill_pcf_for_abstract_edge` 对每个 AbstractEdge：

1. 从 `path_vertices` 和 `original_edges` 提取标签序列
2. 构造 `AltKey`（路径模式的规范键）
3. 从 `DegreeSeqGraphCompressed` 查询两个端点的 PCF

**PCF（分段常数函数）含义**：

对路径模式 `A -e1→ B -e2→ C`：
- `src_pcf`：以 A 的路径度数 x 为横轴，满足"A 通过此路径能到达 x 个 C"的 A-顶点密度为纵轴
- `dst_pcf`：以 C 的路径度数 x 为横轴，满足"C 通过此路径能被 x 个 A 到达"的 C-顶点密度为纵轴

```
AE0 (A-e1-B-e2-C):
  alt_key = make_alt_key([A,B,C], [e1,e2])
  src_pcf = catalog.get_piece_func_by_path(alt_key, "A")  → A 的路径度数分布
  dst_pcf = catalog.get_piece_func_by_path(alt_key, "C")  → C 的路径度数分布

AE1 (C-e3-D):
  alt_key = make_alt_key([C,D], [e3])
  src_pcf = catalog.get_piece_func_by_path(alt_key, "C")
  dst_pcf = catalog.get_piece_func_by_path(alt_key, "D")
```

---

## Step 7：构建 AbstractGraph

AbstractGraph 包含：

```
顶点: A, C, D
抽象边:
  AE0: A ----[A-e1-B-e2-C, src_pcf=f_A, dst_pcf=f_C]---→ C
  AE1: C ----[C-e3-D,       src_pcf=g_C, dst_pcf=g_D]---→ D
```

---

## Step 8：get_es() — 拓扑树规约

### 8a. 选 Root（pick_root）

遍历所有顶点候选，选使"最大层数"最小的顶点作为 root：

```
Root=A: 层数 A=0, C=1, D=2 → max=2
Root=C: 层数 C=0, A=1, D=1 → max=1  ← 最优
Root=D: 层数 D=0, C=1, A=2 → max=2

→ root = C
```

### 8b. 拓扑分层（BFS 从 root 出发）

```
gen(C) = 0
gen(A) = 1  （C 的邻居，经 AE0）
gen(D) = 1  （C 的邻居，经 AE1）
max_gen = 1
```

### 8c. 自底向上归约（从 gen=max 到 gen=0）

**gen=1，处理叶子节点 A：**

```
parent_edge = AE0（A-C 之间）
parent_to_vertex_pcf = get_edge_pcf_at_vertex(AE0, parent=C) = AE0.dst_pcf = f_C
  （C 在路径 A-e1-B-e2-C 中的度数 PCF）

children 为空 →
  result = parent_to_vertex_pcf = f_C

child_vertex_pcf[A] = f_C
```

**gen=1，处理叶子节点 D：**

```
parent_edge = AE1（C-D 之间）
parent_to_vertex_pcf = get_edge_pcf_at_vertex(AE1, parent=C) = AE1.src_pcf = g_C
  （C 在路径 C-e3-D 中的度数 PCF）

children 为空 →
  result = g_C

child_vertex_pcf[D] = g_C
```

**gen=0，处理 root C：**

```
parent = None（根节点）
children = [(A, AE0), (D, AE1)]

child_pcfs = [child_vertex_pcf[A], child_vertex_pcf[D]]
           = [f_C,  g_C]

multiplied_child_pcf = alpha_refs([f_C, g_C])
                     = f_C ⊗ g_C   （逐点乘积）

cur_gen == 0 → projected = multiplied_child_pcf

return projected.get_num_rows()
```

### 8d. 最终计算

```
answer = (f_C ⊗ g_C).get_num_rows()
```

其中：
- `f_C` = C 在模式 A-e1-B-e2-C 中的度数 PCF（每个 C 能被多少条 A-B-C 路径到达）
- `g_C` = C 在模式 C-e3-D 中的度数 PCF（每个 C 有多少 D 邻居）
- `⊗`（alpha）= 逐点乘积：对每个 C 顶点，两个度数相乘表示同时参与两侧路径的贡献
- `get_num_rows()` = PCF 的积分（对所有 C 顶点的贡献求和）

**直觉**：估计 A-B-C-D 路径总数 ≈ sum over all C { (C 能被多少 A-B-C 路径到达) × (C 有多少 D 邻居) }

---

## PCF 运算说明

| 运算 | 语义 |
|------|------|
| `alpha(f, g)` = `f ⊗ g` | 逐点乘积：两侧路径在共享顶点处的联合 |
| `beta_right(rx, sx, sy)` | 从子节点视角投影到父节点视角（跨边传播度数分布） |
| `get_num_rows()` | PCF 积分 = 估计总匹配数 |

---

## k 值对 AbstractEdge 切分的影响

| k | AbstractEdge 数 | 路径切分 | Catalog 需要路径长度 |
|---|----------------|---------|-------------------|
| 1 | 3 | A-B, B-C, C-D（各 1 跳） | max_k ≥ 1 |
| 2 | 2 | A-B-C（2跳）+ C-D（1跳） | max_k ≥ 2 |
| 3 | 1 | A-B-C-D（整段，3跳） | max_k ≥ 3 |

k 越大，AbstractEdge 越少，拓扑树越浅，规约步骤越少，但 catalog 需要更长路径的 PCF 统计。

---

## 完整数据流示意

```
QueryGraph [A-B-C-D]
      │
      ▼
  无环检测 → 单图路径
      │
      ▼
  路径 [A,B,C,D] / [e1,e2,e3]
      │  k=2切分
      ▼
  AE0: A→C (e1,e2)   AE1: C→D (e3)
      │  查Catalog
      ▼
  AE0.src_pcf=f_A    AE1.src_pcf=g_C
  AE0.dst_pcf=f_C    AE1.dst_pcf=g_D
      │
      ▼
  AbstractGraph: A --AE0--> C --AE1--> D
      │  get_es(), root=C
      ▼
  gen=1: child_pcf[A]=f_C,  child_pcf[D]=g_C
  gen=0: alpha(f_C, g_C).get_num_rows()
      │
      ▼
  估计基数 ≈ Σ_{c ∈ C} f_C(c) × g_C(c)
```
