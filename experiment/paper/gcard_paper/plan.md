# GCard Paper Plan (Execution Version)

## 0. Core Positioning (必须在 Intro/Related Work 说清楚)

我们不是简单的 `SafeBound + PathCE` 组合，核心贡献是三点：

1. 新的 `Path Degree Sketch (PathSketch)` 压缩表示  
2. Predicate 解耦：将 predicate attach 到 path 子查询，估计相对该 path 的 selectivity  
3. PathSketch 增量更新算法（避免全量重建）

### 0.1 论文主张（Claim）

- `C1 (Accuracy/Bound)`: 在相同内存预算下，PathSketch 压缩比现有统计更紧（更低 q-error 或更紧 upper-bound）
- `C2 (Predicate Decoupling)`: 解耦后估计流程在理论上可解释，且在复杂谓词下更稳健
- `C3 (Maintainability)`: 增量更新明显优于 rebuild，且误差可控

### 0.2 风险点（审稿人可能质疑）

- 新颖性是否只是工程拼接
- 解耦后是否仍有上界保证/误差边界
- update 是否只在小规模变化有效

---

## Sec 1. Introduction

目标：把问题、差距、贡献三段式写实。

- 背景：graph cardinality estimation 对优化器的重要性
- Gap：
  - 现有方法在 path 结构统计、谓词处理、动态更新三者上无法同时兼顾
- 我们的方法：
  - PathSketch-based path-level compression
  - predicate-path decoupling
  - incremental update
- Contributions（与 0 节一致，一一对应）
- 结果预告（accuracy/latency/build/update/e2e）

## Sec 2. Preliminaries

- 基本定义
  - Property Graph / Query Graph / Match Cardinality
  - Predicate, Selectivity, Path Query
  - Flat DS, PathSketch, Compressed PathSketch
- 问题定义
  - 输入：图 + 查询（含谓词）
  - 输出：估计值


## Sec 3. GCard Framework Overview

- Offline
  - 路径统计抽取
  - PathSketch 构建与压缩
  - 可更新索引结构初始化
- Online
  - 查询分解为 path-level 子任务
  - predicate attach 与 selectivity 估计
  - 组合估计与 cycle 处理
- 关键挑战与章节映射
  - predicate -> Sec 4
  - pathsketch/compress/update -> Sec 5
  - cycles -> Sec 6

## Sec 4. Handling Predicates (Decoupling)

### 4.1 Motivation

- 直接在全查询上建模 predicate 成本高且泛化差
- 将 predicate attach 到 path 子查询可复用 PathSketch 统计

### 4.2 Method

- attach 规则（如何选 path P）
- 估计 `sel(f | P)` 的流程
- 多谓词组合策略（AND/OR）
- online sampling 只作为补偿机制，不是主路径

### 4.3 Theory / Guarantee
- 给出命题：解耦估计的可行性条件
- 若无法严格 upper-bound，至少给出误差边界或保守修正项

### 4.4 Algorithm

- 伪代码：`AttachPredicate`, `EstimateSelectivityOnPath`
- 复杂度分析

## Sec 5. PathSketch Construction and Update

PathSketch = Path Degree Sketch

### 5.1 Construction

- 并行收集 flat DS
- 转换为 PathSketch
- 压缩（fast compress）

### 5.2 Compression Format

- 数据结构定义（segment / cumulative / index）
- 相比 flat DS 或基线压缩的优势
  - memory
  - lookup latency
  - bound tightness

### 5.3 Incremental Update

- naive rebuild 方案及问题
- 增量更新算法（插入/删除/属性更新）
- 正确性与复杂度
- 误差传播控制（必要时）

### 5.4 Algorithm Box

- `BuildPathSketch`
- `CompressPathSketch`
- `UpdatePathSketch`

## Sec 6. Handling Cycles

- 为什么用删边/断环策略（trade-off：准确性 vs 复杂度）
- cycle 到可估计结构的转换流程
- 伪代码 + 示例
- 误差影响分析（micro-benchmark 里验证）

## Sec 7. Evaluation

## 7.1 Experimental Settings

- [ ] Datasets and Workloads (需要定下来)
  - LDBC
  - IMDB
  - Synthetic (可控规模/度分布/谓词选择率)
- [ ] Baselines
  - PathCE
  - SafeBound (可对齐部分任务)
  - FactorJoin / CEG / G-CARE（按可复现实验选择）
- [ ] Environment
  - CPU/Memory/threads
  - 参数统一与调参策略

## 7.2 Main Experiments

### Exp-1: Estimation Accuracy
- 目标：验证 C1/C2
- 指标：q-error (avg/p50/p90/p99), upper-bound violation rate

### Exp-2: Estimation Latency
- 目标：在线推理开销可接受
- 指标：single-query latency + variance

### Exp-3: PathSketch Construction Latency
- 目标：离线构建成本
- 指标：build time, peak memory, pathsketch size

### Exp-4: PathSketch Maintenance Efficiency
- 目标：验证 C3
- 指标：update throughput, amortized update cost, accuracy drift over
  updates (5% edge insertion)

### Exp-5: Scalability of DS Construction
- 目标：线程数和图规模扩展性
- 指标：speedup, weak/strong scaling

### Exp-6: Micro Benchmarks
- cycle handling effectiveness (prune edge vs break vertex)
- compression ratio vs accuracy (varying n)
- predicate decoupling strategy ablation (???)

### Exp-7: Impact on End-to-End Performance
- 与优化器集成后：
  - planning time
  - execution time
  - e2e time

---

## 8. Related Work 写作计划

- Summary-based estimators
- Upper-bound estimators
- Graph-specific estimators
- Dynamic/incremental statistics maintenance
- 每类最后一句写清：GCard 与其核心差异

---

## 9. Timeline (4 weeks)

### Week 1
- 完成 Sec 1-3 初稿
- 完成 Sec 4 方法与伪代码
- 固化实验环境与 baseline pipeline

### Week 2
- 完成 Sec 5-6 初稿
- 跑 Exp-1/2/3 初版结果
- 修正方法细节（尤其 predicate attach）

### Week 3
- 跑 Exp-4/5/6/7
- 补全 related work
- 完成全部图表与表格初稿

### Week 4
- 全文收敛（逻辑、术语、贡献对齐）
- 反复校对 claim 与实验一一对应
- 完成 rebuttal-ready 附录材料（复杂度/更多消融）

---

## 10. Done Criteria (投稿前自检)

- [ ] 三个 contribution 各有 method + theory/argument + experiment 支撑
- [ ] 所有 claim 在实验中可被直接定位验证
- [ ] Related Work 明确回答“为什么不是简单组合”
- [ ] 图表可复现，参数可公开
- [ ] 全文术语统一：PathSketch / decoupling / attach / selectivity
