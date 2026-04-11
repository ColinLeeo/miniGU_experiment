# GCard 算法框架图 Prompt

复制 "Prompt 开始" 到 "Prompt 结束" 之间的内容到 Claude 网页端。

---

## Prompt 开始

请帮我绘制一张论文级别的算法框架图（React artifact + SVG），描述 GCard 图模式基数估计算法的完整流程，包括查询估计和增量更新。

### 布局原则

整张图分**上下两层**：
- **上层：内存（Runtime）**— 所有计算都在这里发生
- **下层：磁盘（Persistent Storage）**— 只存统计信息的序列化文件

两层之间只有一对垂直箭头（save / load），保持整洁。上层内部从左到右是主数据流。

---

### Running Example

贯穿全图的例子：

**数据图 G**：6 个节点
```
Person: p1, p2, p3, p4
Movie:  m1, m2
边: p1→m1, p2→m1, p3→m1, p4→m2
(都是 ACTED_IN 类型)
```

**查询 Q**：三角形 — "两个人共演一部电影"
```
(a:Person)─[ACTED_IN]→(m:Movie)←[ACTED_IN]─(b:Person)
```

---

### 上层（内存）— 从左到右分三个区域

#### ① 左区：数据图 + 度序列提取

**画数据图 G**（小巧）：4 个 Person 圆形节点 + 2 个 Movie 方形节点，边连好。

**度序列提取过程**（这是核心，要画清楚）：

给定路径模式 `P = Person─ACTED_IN─Movie`，统计每个端点参与了多少条匹配路径：

Person 端（每个 Person 连了几个 Movie）：
```
p1: 1,  p2: 1,  p3: 1,  p4: 1
排序后度序列: [1, 1, 1, 1]
```

Movie 端（每个 Movie 被几个 Person 连）：
```
m1: 3,  m2: 1
排序后度序列: [1, 3]   (升序)
```

用一个小表格或竖条图清晰展示这个对应关系。

**度序列 → PCF（分段常数函数）**：

画两个并排的 PCF 小图（阶梯函数），体现压缩效果：

```
Person 端 PCF f_P(i):          Movie 端 PCF f_M(i):
  度                              度
  1 |████████                     3 |    ████
    |████████                     1 |████
    +--------→ i(排名)              +--------→ i(排名)
    0    4                          0  1  2

  只有1段: (0,4] → 1              两段: (0,1] → 1, (1,2] → 3
```

标注：PCF 是度序列的有损压缩表示，X 轴是顶点排名，Y 轴是度值。段数远小于顶点数。

**箭头**：从 PCF 向右指向中间区域的 "统计信息存储"。

#### ② 中区：统计信息（Statistic）

这是中心枢纽，画一个较大的框。

**内部结构**（用学术风格的伪结构体）：

```
Catalog C
──────────────────────────────
  路径模式 P → 两端 PCF
  ──────────────────────────
  Person─ACTED_IN─Movie:
      Person端: f_P(i) = [1,1,1,1] → PCF
      Movie端:  f_M(i) = [1,3]     → PCF

  (K=2 时还有更长的路径模式...)
──────────────────────────────
```

标注重点：
- "预计算所有 ≤K 跳路径模式的端点度序列"
- "每端存储为 PCF（紧凑表示）"
- 画一个小标注："同时保留精确度序列用于增量更新"

#### ③ 右区：查询估计流程

这是最重要的部分，要画得最详细。

**Step 1: 查询图分析**

画查询 Q 的三角形图：a ─e1→ m ←e2─ b

标注"环检测：发现 1 个环"

**Step 2: 生成树分解**

三角形有 3 条边，枚举生成树 = 每次去掉一条边。画 2-3 棵生成树（用虚线标记被删除的非树边）：

```
树 T1: 去掉 e_ab        树 T2: 去掉 e1
a─e1→m←e2─b             a    m←e2─b
a···········b            a·e1·m
(非树边 e_ab)            (非树边 e1)
```

注意：这里的例子是 (a)-[e1]->(m)<-[e2]-(b)，这是一个路径不是三角形。让我用一个真正的三角形：a-b, b-m, a-m。或者更自然地说查询 Q 本身就是路径 a→m←b，没有环。那换一个有环的例子。

实际上这个查询 (a:Person)→(m:Movie)←(b:Person) 是一个 **星型/path** 查询（2条边3个节点），不是环。让我改成真正的三角形：

改用 4 节点查询（保留 running example 的 Person/Movie）：

```
查询 Q:
(a:Person)─[ACTED_IN]→(m1:Movie)
(b:Person)─[ACTED_IN]→(m1:Movie)
(a:Person)─[ACTED_IN]→(m2:Movie)
(b:Person)─[ACTED_IN]→(m2:Movie)
即：两人共演了两部电影 (矩形查询)
```

不，太复杂了。保持简单，用 **star query**（无环）和 **triangle**（有环）各展示一步：

**简化方案**：展示一个 **星型查询**（无环），重点展示 PCF 的 alpha/beta 归约过程。在旁边小框补充说明有环时用生成树分解。

查询 Q（星型，3 条边）:
```
        a:Person
       ↗    ↖
   e1↗      ↖e3
    ↗          ↖
m1:Movie  m2:Movie
    ↖          ↗
   e2↖      ↗e4
      ↖  ↗
      b:Person
```

算了这也复杂。**最终方案：用原始的 path query 做主例子，侧边标注环处理。**

**最终 Running Example 查询**:
```
(a:Person)─[ACTED_IN]→(m:Movie)←[ACTED_IN]─(b:Person)
```
这是一个 path（无环），3 个节点 2 条边。

**Step 2: 构建 AbstractGraph**

路径分解（这里只有一棵树，因为无环）：
```
AbstractGraph:
  a ──edge1──→ m ←──edge2── b

edge1 对应路径 Person─ACTED_IN─Movie:
  src_pcf (a端) = f_P    dst_pcf (m端) = f_M

edge2 对应路径 Person─ACTED_IN─Movie:
  src_pcf (b端) = f_P    dst_pcf (m端) = f_M
```

**用箭头从 edge1/edge2 指向中区的 Catalog**，标注 "查表获取 PCF"。

**Step 3: 自底向上归约（get_es）**— 这是算法核心，画最大

选 m 为根（度最大），画出归约树：

```
        m (root, gen=0)
       / \
      /   \
     a     b    (leaves, gen=1)
```

归约过程（从叶到根，展示具体 PCF 计算）：

```
叶子 a:  取 edge1 在 m 端的 PCF → f_M
叶子 b:  取 edge2 在 m 端的 PCF → f_M

根 m:
  1) alpha(f_M, f_M) = f_M ⊙ f_M   (逐点乘积)

     f_M(i):         f_M⊙f_M:
     3|   ██         9|   ██
     1|██            1|██
      +----→          +----→
      0 1 2           0 1 2

  2) result.get_num_rows() = 1×1 + 1×9 = 10

Output: cardinality ≈ 10
```

**画清楚 alpha 操作的可视化**：两个 PCF 逐段相乘，画出输入和输出的阶梯图。

**侧边小框：有环查询处理**

在 Step 2 旁边加一个带虚线边框的注释框：
```
有环查询:
  1. Union-Find 检测环
  2. 枚举 k-best 生成树
     (每棵移除一条环边)
  3. 每棵树独立估计
  4. 取最小非零基数
```

画一个小三角形→去掉一条边变成 path 的示意即可，不需要完整展开。

---

### 更新流程（在上层右下角或下方，作为第二条流水线）

在查询流程下方或右侧，画更新流水线。用不同颜色（绿色）区分。

**触发事件**：INSERT EDGE (p4)─[ACTED_IN]→(m1)

画出 G 中新增的这条绿色虚线边。

**Step U1: 记录 UpdateEntry（延迟，不立即计算）**

```
UpdateLog（追加列表）:
  ┌──────────────────────────────────────────┐
  │ [Person,ACTED_IN,Movie] p4: +1  depth=1 │
  │ [Movie,ACTED_IN,Person] m1: +1  depth=1 │
  └──────────────────────────────────────────┘
```

标注："仅记录受影响端点和 delta，O(1)"

**Step U2: Compact & Apply（批量触发）**

```
Expand:  depth>0 的 entry 沿图遍历邻居
         p4 的邻居: m1, m2 → 生成 2-hop entry
         直到 depth=0

Accumulate: 同一路径模式下同一顶点的 delta 累加

Apply:   更新 Statistic 中的精确度序列
         p4 的度: 1→2,  m1 的度: 3→4

Rebuild: 度序列 → 新 PCF
```

用一个小的"前后对比"展示 Movie 端 PCF 的变化：

```
更新前:               更新后:
  度                    度
  3|  ██               4|  ██
  1|██                 1|██
   +----→               +----→
   0 1 2                0 1 2
```

**箭头**：从 Apply 指向中区 Statistic/Catalog，标注 "增量更新 PCF"。

---

### 下层（磁盘）

浅灰底色 + 虚线边框的窄条区域。

中间画一个文件图标 + 标签：
```
┌─────────────────────┐
│  graph.statistic.bin │
│                     │
│  精确度序列 + 元数据  │
│  组件级压缩:         │
│   · vertex_ids: Δ编码+varint │
│   · 度值: RLE + zstd         │
│   · 路径标签: 字典编码        │
└─────────────────────┘
```

与上层 Statistic 之间的两条垂直箭头：
- ↓ "save（序列化 + 压缩）"
- ↑ "load（反序列化 → 重建 PCF）"

---

### 全局编号和数据流标注

用圆圈编号标注主流程顺序：
- ① 度序列提取（Graph → 度序列）
- ② PCF 压缩（度序列 → Catalog）
- ③ 查表（Query → 查 Catalog 获取 PCF）
- ④ 归约（alpha/beta 计算基数）
- ⑤ 增量更新（Graph 变化 → UpdateLog → Apply → Catalog 更新）
- ⑥ 持久化（Catalog ↔ 磁盘文件）

### 视觉要求

1. 上下两层，内存浅白/浅蓝色，磁盘浅灰色
2. 主数据流从左到右，层间仅垂直箭头
3. **PCF 阶梯函数图是视觉重点**，至少出现 3 次：原始 PCF、alpha 运算、更新后 PCF
4. alpha 运算画出"两个 PCF → 逐点相乘 → 结果 PCF"的可视化
5. 查询流程用蓝色调，更新流程用绿色调，构建/持久化用灰色调
6. 节点用形状区分（Person 圆形，Movie 方形），颜色柔和
7. 整体紧凑，适合论文 single-column full-width figure (约 width 1000-1200px)
8. 无衬线字体，数学符号用斜体，数据结构名用等宽
9. 不要太多文字说明，尽量用图形和符号表达
10. 边框简洁，不要 3D 效果或阴影

### 数学符号对照

在图的某个角落放一个小的 legend：
- f_P(i): Person 端点的度 PCF (x=排名, y=度值)
- α(f,g): pointwise product, α(f,g)(i) = f(i)·g(i)
- β(f,g,h): join projection, 将子树 PCF 投影到父边
- |Q|_est = α结果的累积行数 (PCF 下方面积)

## Prompt 结束
