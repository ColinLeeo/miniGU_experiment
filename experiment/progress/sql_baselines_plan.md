# SQL Baselines 集成计划

Last updated: 2026-03-20

## 目标

将 FactorJoin、BayesCard、FLAT、SafeBound 四个 SQL 基数估计工具适配为图模式匹配 benchmark 的 baseline。

核心思路：LDBC CSV 数据本身就是关系表，图模式可转换为等价 SQL join 查询。

## 进度

| 步骤 | 状态 | 文件 |
|------|------|------|
| 模式转 SQL 工具 | ✅ 完成 | `experiment/tools/minigu2sql.py` |
| SQL schema 元数据生成 | ✅ 完成 | `experiment/tools/generate_sql_schema.py` |
| Clone 四个仓库 | ✅ 完成 | `experiment/baseline/{factorjoin,bayescard,flat,safebound}` |
| LDBC Schema for FactorJoin | ✅ 完成 | `experiment/baseline/factorjoin/Schemas/ldbc/schema.py` |
| 准备脚本 | ✅ 完成 | `experiment/scripts/prepare_sql_baselines.sh` |
| 查询 wrapper × 4 | ✅ 完成 | `experiment/scripts/sql_baselines/run_{factorjoin,bayescard,flat,safebound}.py` |
| Benchmark 脚本 | ✅ 完成 | `experiment/scripts/bench_sql_baselines.sh` |
| 结果合并 | ✅ 完成 | `experiment/scripts/merge_results.py` |
| 创建 conda 环境 | ⏳ 待做 | 需要在远程机器上执行 |
| 训练模型 | ⏳ 待做 | `prepare_sql_baselines.sh <sf>` |
| 运行 benchmark | ⏳ 待做 | `bench_sql_baselines.sh <sf>` |

## 需要 Clone 的仓库

```bash
cd experiment/baseline/

# 1. FactorJoin (Sampling/Traditional, SIGMOD 2023)
git clone https://github.com/wuziniu/FactorJoin.git factorjoin

# 2. BayesCard (Learning-based, Bayesian Network)
git clone https://github.com/wuziniu/BayesCard.git bayescard

# 3. FLAT/FSPN (Learning-based, Factorized SPN, VLDB 2021)
git clone https://github.com/wuziniu/FSPN.git flat

# 4. SafeBound (Bound-based, upper bound estimator)
git clone https://github.com/kylebd99/SafeBound.git safebound
```

## Conda 环境（每个工具独立）

| 工具 | conda env | 关键依赖 |
|------|-----------|---------|
| FactorJoin | `minigu-factorjoin` | PyTorch + pgmpy |
| BayesCard | `minigu-bayescard` | pomegranate（旧版） |
| FLAT | `minigu-flat` | TensorFlow |
| SafeBound | `minigu-safebound` | Cython |

## 工具流程

```
minigu2sql.py: miniGU pattern JSON → SQL COUNT(*) query
                                       ↓
prepare_sql_baselines.sh: clone → conda env → load data → train model
                                       ↓
bench_sql_baselines.sh: pattern → SQL → 逐工具估计 → CSV
                                       ↓
merge_results.py: 合并图 baseline + SQL baseline → 统一 CSV → 绘图
```

## 与现有图 baseline 的区别

| | 图 baseline (pathce/gcare/color) | SQL baseline (FactorJoin/BayesCard/FLAT/SafeBound) |
|---|---|---|
| 输入格式 | 图 pattern (JSON/txt) | SQL join 查询 |
| 谓词支持 | ❌ 不支持 | ✅ 支持 |
| bench 脚本 | `bench_all.sh` | `bench_sql_baselines.sh`（独立） |
| 结果文件 | `comparison_sf${SF}.csv` | `comparison_sql_sf${SF}.csv` |

## 技术风险

1. 自 join：同一边表多次出现，SQL 工具可能假设独立性
2. 谓词类型：Int64 日期 vs 字符串
3. 依赖冲突：conda 隔离解决
4. 训练时间：大 SF 可能很慢
