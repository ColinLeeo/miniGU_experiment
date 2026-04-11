# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Language

Always respond in Chinese (中文).

## Build & Development Commands

**Toolchain:** Nightly Rust (`nightly-2025-12-08`), specified in `rust-toolchain.toml`. Always pass `--features std,serde,miette` when building or testing.

```bash
# Build
cargo build --features std,serde,miette
cargo build -r --features std,serde,miette   # release

# Run the interactive shell
cargo run -- shell
cargo run -- execute <script.gql>

# Tests (CI uses nextest)
cargo nextest run --features std,serde,miette
cargo test --features std,serde,miette --doc  # doc tests only
cargo test -p <crate> --test <test_name>      # single test

# Lint & format
cargo clippy --tests --features std,serde,miette --no-deps
cargo fmt
cargo fmt --check
taplo fmt --check --diff   # TOML files
```

## Codebase Architecture

### Workspace layout

The repo is a Cargo workspace. Core logic lives in `minigu/` sub-crates; the CLI lives in `minigu-cli/`; end-to-end tests live in `minigu-test/`.

```
minigu/
  core/         # Public API surface: Database, Session
  context/      # SessionContext, DatabaseContext, GraphContainer
  catalog/      # In-memory schema catalog (hierarchy: directory → schema → graph/procedure)
  transaction/  # MVCC primitives: Timestamp, UndoEntry<T>, GraphTxnManager
  storage/      # TP (OLTP) and AP (OLAP) graph storage engines, WAL, DiskANN vector index
  gql/
    parser/     # GQL lexer + parser → AST (supports no_std)
    planner/    # Binder → LogicalPlanner → Optimizer → PhysicalPlan
    execution/  # Volcano-model executors, expression evaluators
minigu-cli/     # Interactive shell (rustyline) + script executor
minigu-test/    # sqllogictest + insta snapshot tests
```

### Query pipeline

A query enters through `Session::query()` (`minigu/core/src/session.rs`) and flows:

1. **Parse** — `parse_gql()` → `gql_parser::ast::Procedure`
2. **Plan** — Planner runs three sub-passes:
   - *Binder*: resolves names against the catalog
   - *LogicalPlanner*: builds a logical plan tree
   - *Optimizer*: converts to a physical plan tree
3. **Execute** — `ExecutorBuilder::build()` constructs a pull-based executor tree; calling `.next_chunk()` in a loop yields `DataChunk` batches (default batch size 2048, Arrow columnar layout)
4. **Format** — The CLI formats chunks as a table (sharp/csv/etc.) and optionally prints query metrics

### Storage & MVCC

**TP mode** (`minigu/storage/src/tp/`) is the default transactional engine:
- Vertices and edges each have a `VersionChain` holding the current version plus a linked undo log of `UndoEntry<DeltaOp>`.
- Visibility: if `commit_ts == txn_id` the current transaction wrote it; if `commit_ts ≤ start_ts` it was committed before the transaction began; otherwise walk the undo chain.
- WAL entries (`RedoEntry` with LSN, txn_id, operation) are serialized with postcard into a single `.minigu` file that also contains a checkpoint region.
- `InMemoryPersistence` (no disk) and `DbFilePersistence` (single-file with crash recovery) are swappable providers.

**AP mode** (`minigu/storage/src/ap/`) is column-oriented for analytical queries.

### Execution model

Executors live in `minigu/gql/execution/src/executor/` and implement the `Executor` trait (pull-based, `next_chunk() → Option<DataChunk>`). Expression evaluation is done by *evaluators* (`minigu/gql/execution/src/evaluator/`) that operate over Arrow arrays. The `ExecutorBuilder` (`builder.rs`) recursively converts a physical plan tree into an executor tree.

### Workflow

- After modifying Rust code, always run `cargo fmt` to format the code before finishing.
- All script/benchmark test results must be output to `experiment/result/`, organized by corresponding subdirectory paths (e.g., `experiment/result/comparison/`, `experiment/result/qerror/`).

### Key patterns

- **Feature flags:** The parser targets `no_std`; features `std`, `serde`, and `miette` must be explicitly enabled for normal use.
- **Error handling:** `thiserror` + `miette` for diagnostics; each crate has its own `Error` enum and a local `Result<T>` alias.
- **Concurrency:** `Arc`/`RwLock` for shared state; `DashMap`/`DashSet` for concurrent maps; `rayon` for parallelism.
- **Catalog hierarchy:** `MemoryCatalog` → `MemoryDirectoryCatalog` → `MemorySchemaCatalog` → graphs/procedures.

### GCard: Graph Cardinality Estimation（核心算法）

GCard 是基于度序列分段常数函数（Piecewise Constant Function, PCF）的**图模式匹配基数估计**算法。代码位于 `minigu/core/src/procedures/gcard_query/`。

#### 模块结构

```
procedures/gcard_query/
  mod.rs              # 入口：注册 gcard_query procedure，参数解析，结果选择
  types.rs            # Query/Predicate/AbstractEdge 等核心类型，JSON 反序列化
  query_graph.rs      # QueryGraph：查询图构建、环检测、生成树枚举、路径分解
  abs_graph.rs        # AbstractGraph：抽象图构建、拓扑排序、get_es() 基数计算
  degreepiecewise/    # PCF（分段常数函数）实现：alpha/beta 运算
  catalog.rs          # DegreeSeqGraphCompressed：预计算的度序列统计信息
  create_catalog.rs   # GCard_build procedure：遍历图数据构建统计 catalog
  statistic.rs        # Statistic：label → path pattern → 直方图
  block_statistic.rs  # BlockStatistic：压缩直方图
  graph.rs            # 基础图骨架
  union_find.rs       # 并查集，用于环检测
  update_log.rs       # 增量更新日志
  compact_update_log.rs # 日志压缩
  compression.rs      # 数据压缩
  stat_quality.rs     # 统计质量检查
```

#### 算法流程

```
输入: Query JSON (vertices, edges, predicates)
  ↓
[1] 解析 → QueryGraph (邻接表 + 谓词索引)
  ↓
[2] 环检测 (union-find)
  ├─ 无环: 直接构建单个 AbstractGraph
  └─ 有环: 枚举 k-best 生成树（基于边评分的贪心策略）
  ↓
[3] 对每棵生成树:
  a) 找 pivot 节点 (度 ≥ 3)
  b) 路径分解：pivot 之间的简单路径
  c) 压缩为 AbstractGraph（路径 → 抽象边）
  d) 从 DegreeSeqGraphCompressed 填充每条边的 PCF
  ↓
[4] 基数计算 (get_es):
  - 拓扑排序，从叶到根自底向上归约
  - PCF 运算: alpha（逐点乘积）/ beta_left / beta_right
  ↓
[5] 取所有 AbstractGraph 中最小非零基数作为最终估计
  - OUTER 模式: 基数 × 谓词选择率
  ↓
输出: cardinality (Int64)
```

#### 谓词处理模式

- **INNER (0)**: 在抽象图构建阶段就应用谓词（融入 PCF 计算）
- **OUTER (1)**: 先算无谓词基数，再乘以选择率
- **IGNORE (2)**: 完全忽略谓词





