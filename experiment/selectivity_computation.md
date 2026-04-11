# 选择率计算（compute_selectivity_with_predicates）

## 背景

选择率（selectivity）表示：在满足图结构的路径中，有多大比例**同时满足属性谓词**。

$$\text{selectivity} = \frac{|\text{结构匹配且谓词通过的路径数}|}{|\text{仅结构匹配的路径数}|}$$

**触发条件**：AbstractEdge 上有谓词（顶点或边的属性过滤），且谓词模式为 INNER 或 OUTER。

**输出用途**：
- INNER：`src_pcf / dst_pcf` 按比例截断（`truncate_by_ratio`），直接缩减 PCF 规模
- OUTER：最终基数 = 无谓词基数 × selectivity

---

## 示例 Schema 与查询

```
A -p1→ B -p2→ C -p3→ D   and   C -p4→ E
```

查询：`A -p1→ B`，谓词：`B.age > 30`

AbstractEdge 覆盖路径 `[A, B]`，original_edges `[p1]`，predicates `[{target=vertex, id=B, property=age, op=Gt, value=30}]`。

---

## 算法核心：WanderJoin 比值估计

对一条从起点 $v_0$ 出发的随机游走：

- 每步**均匀随机**选一个符合 edge label 的邻居
- **结构权重** $w = d_1 \times d_2 \times \cdots \times d_k$（各步邻居数之积）= 该路径被采到概率的倒数
- **谓词权重** = $w$（所有谓词通过）或 $0$（任意谓词失败）

$$\hat{s} = \frac{\sum_i w_{\text{pred}}^{(i)}}{\sum_i w_{\text{struct}}^{(i)}}$$

这是一个 HT（Horvitz-Thompson）比值估计器，无偏估计选择率。

---

## 执行流程

### Step 1：构建 PathQuery（`build_path_query`）

将 AbstractEdge 展开为交替的顶点/边步骤序列，并将谓词按位置分配：

```
A -p1→ B（谓词 B.age > 30）

path_elements:
  [0] Vertex(A, position=0)
  [1] Edge(p1, direction=Outgoing, position=1)
  [2] Vertex(B, position=2, pred: age > 30)
```

**边方向判断**：通过 edge label 名称（如 `A_p1_B`，src 段 == 当前顶点 label → Outgoing）。

---

### Step 2：预编译（`CompiledPathQuery::compile`）

将标签名和属性名解析为整数 ID，**只执行一次**：

```
CompiledStep::Vertex { label_id: id(A), predicates: [] }
CompiledStep::Edge   { label_id: id(p1), direction: Outgoing, predicates: [] }
CompiledStep::Vertex { label_id: id(B), predicates: [{ prop_index: idx(age), op: Gt, value: 30 }] }
```

之后每次 walk 直接用整数比较，不再做字符串查找。

---

### Step 3：采样起点顶点

对 src label（A 类）做**蓄水池采样**，取 `sample_size` 个顶点，全量顶点只扫一次：

```
count = 0, samples = []
for (vid, label_id) in txn.iter_vertex_ids():
    if label_id != id(A): skip
    count += 1
    if samples.len() < sample_size:
        samples.push(vid)
    else:
        j = rand(0..count)
        if j < sample_size:
            samples[j] = vid   // 等概率替换，保证均匀性
```

结果存入 `vertex_sample_cache`（DashMap），同一查询内同 label 只采样一次。

---

### Step 4：对每个起点执行随机游走

#### 4a. Pilot Walk（预热）

先对该起点跑 **5 次**游走，计算结构权重的变异系数 CV：

```
pilot_mean = mean(walk_struct[0..5])
cv = std(walk_struct[0..5]) / pilot_mean
```

若 `cv > 0.3`（分布波动大，起点出度差异大），追加至最多 **50 次**游走。

#### 4b. 单次游走（`execute_compiled_walk`）

按 CompiledStep 序列逐步执行，维护 `weight` 和 `pred_ok` 两个状态：

```
当前顶点 = start_vid
weight = 1.0, pred_ok = true
```

**Vertex 步骤**：

```
1. 检查 label_id 是否匹配（仅 get_vertex_label_id，无属性拷贝）
2. 若 pred_ok == true 且有谓词：
     取出顶点全量属性（get_vertex）
     for each pred in predicates:
         if !compare(vertex.props[prop_index], op, value):
             pred_ok = false; break
```

**Edge 步骤**：

```
蓄水池采样，一次遍历选出随机一个邻居（不收集全部邻居到 Vec）：

degree = 0, chosen = None
for neighbor in txn.iter_adjacency_outgoing/incoming(current_vid)
                   .filter(label == edge_label_id):
    degree += 1
    if degree == 1 or rand(0..degree) == 0:
        chosen = neighbor

if degree == 0: return (0.0, 0.0)   // 死路，结构不匹配

weight *= degree
current_vid = chosen.neighbor_id

若有边谓词：取出边属性检查，失败则 pred_ok = false
```

**游走结束**：

```
struct_weight = weight
pred_weight   = weight  (pred_ok == true)
              = 0.0     (pred_ok == false)
```

#### 具体示例（A -p1→ B，谓词 B.age > 30）

```
start_vid = a1（A 类顶点，有 3 个 p1 出向邻居：b1, b2, b3）

walk 1:
  Vertex(A): label 匹配 ✓, 无谓词
  Edge(p1):  邻居 {b1,b2,b3}，随机选 b2 → weight = 3.0
  Vertex(B): label 匹配 ✓, b2.age = 25 → 25 > 30 失败 → pred_ok = false
  → (struct=3.0, pred=0.0)

walk 2:
  Edge(p1):  随机选 b1 → weight = 3.0
  Vertex(B): b1.age = 45 → 通过 → pred_ok = true
  → (struct=3.0, pred=3.0)

walk 3: → (struct=3.0, pred=3.0)
walk 4: → (struct=3.0, pred=0.0)   // 选到 b2 或 b3（假设 b3.age=20）
walk 5: → (struct=3.0, pred=3.0)

pilot_mean = 3.0, cv = 0（所有权重相同，出度固定）→ 不追加

struct_weight_for_start = 3.0
pred_weight_for_start   = (0+3+3+0+3)/5 = 1.8
```

#### 4c. 累积到全局统计

```
sum_struct_weight += 3.0
sum_pred_weight   += 1.8
sum_struct_weight_sq += 9.0    // 用于 SE 估计
sum_pred_weight_sq   += 3.24
sum_cross_weight     += 3.0 × 1.8 = 5.4
struct_success_sample_count += 1
```

---

### Step 5：自适应早停

满足 `struct_success_sample_count >= 300` 后，每 **100 个样本**检查一次收敛：

**Delta 方法（比值估计的标准误）**：

设 $\bar{x} = \frac{\sum p_i}{k}$（谓词权重均值），$\bar{y} = \frac{\sum s_i}{k}$（结构权重均值），估计 $\hat{s} = \bar{x}/\bar{y}$ 的方差：

$$\widehat{\text{Var}}(\hat{s}) \approx \frac{1}{k} \left( \frac{\sigma_x^2}{\bar{y}^2} + \frac{\bar{x}^2 \sigma_y^2}{\bar{y}^4} - \frac{2\bar{x} \cdot \text{Cov}(x,y)}{\bar{y}^3} \right)$$

**收敛条件**（95% CI 相对半宽 ≤ 10%）：

$$\frac{1.96 \cdot \sqrt{\widehat{\text{Var}}(\hat{s})}}{|\hat{s}|} \leq 0.10$$

最多处理 **4000 个有效起点**（有出路的起点）。

---

### Step 6：计算最终选择率

```
selectivity = sum_pred_weight / sum_struct_weight
```

若 `sum_struct_weight == 0`（所有起点均为死路），返回 `0.0`。

续上例，假设累积了足够样本后：

```
sum_pred_weight / sum_struct_weight ≈ 1.8 / 3.0 = 0.6
```

即约 60% 的 A-B 路径满足 `B.age > 30`。

---

## 并行与串行一览

| 位置 | 方式 | 说明 |
|------|------|------|
| 起点顶点扫描 | **串行** | `iter_vertex_ids()` 顺序扫描 |
| 起点间循环 | **串行** | 顺序遍历 sampled_starts |
| 单起点内游走 | **串行** | pilot + 追加 walk 均顺序执行 |
| 单次 walk 内步骤 | **串行** | 逐步顺序推进 |
| 邻居遍历 | **串行**（蓄水池） | 单次 pass 即选出随机邻居 |

> 整个选择率计算**全程串行**。样本量通常在几百到几千，早停机制保证总体开销较小。

---

## 缓存机制

| 缓存 | 类型 | 键 | 复用范围 |
|------|------|----|---------|
| `vertex_sample_cache` | `DashMap<LabelId, Vec<VertexId>>` | src label id | 同一查询内所有 AbstractEdge |
| `selectivity_cache` | `DashMap<String, f64>` | `path:{alt_key}\|pred1\|pred2\|...` | 同一查询内相同路径+谓词组合 |

谓词缓存键排序方式：`(target, id, property, op)` 字典序，保证不同顺序的相同谓词集映射到同一键。

---

## 参数汇总

| 参数 | 值 | 含义 |
|------|----|------|
| `PILOT_WALKS` | 5 | 每个起点预热游走数 |
| `MAX_WALKS` | 50 | 高方差时最大游走数 |
| `CV_THRESHOLD` | 0.3 | 触发追加游走的变异系数阈值 |
| `struct_count_min` | 300 | 开始检查早停的最小有效样本数 |
| `struct_count_max` | 4000 | 最大有效样本数 |
| `rel_eps` | 0.10 | 相对误差阈值（10%） |
| `z_95` | 1.96 | 95% 置信区间系数 |
