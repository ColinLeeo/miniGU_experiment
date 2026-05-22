# FactorJoin LDBC 支持改动说明

这份说明记录为了让 FactorJoin 能跑 LDBC / LSQB / GLogS 图查询所做的主要改动。核心思路是：保留 FactorJoin 原来的 BN + join-bound 推理逻辑，只补齐 LDBC schema，并在查询侧把 GCard 图模式转换成 FactorJoin 能理解的多表 join、alias、subplan 形式。

## 1. 新增 LDBC schema

位置：`experiment/baseline/FactorJoin/Schemas/ldbc/schema.py`

新增 `gen_ldbc_sf1_schema(data_path)`，把 LDBC SF1 的点表和边表注册成 FactorJoin 的 `SchemaGraph`：

- 点表：`person`、`post`、`comment`、`forum`、`tag`、`tagclass`、`city`、`country`、`continent`、`company`、`university` 等。
- 边表：例如 `person_knows_person`、`person_likes_post`、`post_hascreator_person`、`comment_replyof_post`、`forum_containerof_post` 等。
- 边表统一使用 `src` / `dst` 作为 join key。
- 对文本类或当前实验不需要的列配置了 `irrelevant_attributes`，避免 BN 训练时把高基数字符串列纳入模型。
- 用 `schema.add_relationship(...)` 描述边表 `src/dst` 到点表 `id` 的等价 join-key 关系，供 FactorJoin 做 key bucketization 和 join-bound 推理。

## 2. 扩展训练入口以支持 `dataset="ldbc"`

位置：`experiment/baseline/FactorJoin/Join_scheme/data_prepare.py`

主要改动：

- 引入 `gen_ldbc_sf1_schema`。
- 在 `process_stats_data(...)` 中新增 `dataset == "ldbc"` 分支，复用 stats 风格的 CSV 读取方式。
- LDBC 数据路径使用格式化路径，例如：

```text
/home/zxz/miniGU/experiment/datasets/ldbc/sf1/{}.csv
```

训练时会按 schema 中的 table name 替换 `{}` 读取对应 CSV。

实际训练命令由 runner 封装，默认使用：

```text
dataset=ldbc
n_dim_dist=2
n_bins=200
bucket_method=fixed_start_key
```

这里用 `fixed_start_key` 是因为当前环境缺 `jenkspy`，`greedy` bucketization 无法直接运行；`fixed_start_key` 不需要额外依赖，训练速度也更稳定。

## 3. 新增 LDBC runner

位置：`experiment/scripts/factorjoin/run_factorjoin_ldbc.py`

这个脚本提供两个子命令：

```bash
python run_factorjoin_ldbc.py build \
  --model-dir <model_dir> \
  --data-path '/home/zxz/miniGU/experiment/datasets/ldbc/sf1/{}.csv'

python run_factorjoin_ldbc.py query \
  --truth <truth.csv> \
  --model-path <model.pkl> \
  --output-csv <estimate.csv>
```

`build` 子命令调用 FactorJoin 原有训练函数：

```python
train_one_stats(dataset="ldbc", ...)
```

生成模型文件：

```text
model_ldbc_<bucket_method>_<n_bins>.pkl
```

`query` 子命令读取 truth/manifest 中的 `gcard_path`，把 GCard JSON 图模式转换成 FactorJoin 查询上下文，然后调用：

```python
get_cardinality_bound_all(sql, subplans)
```

最终输出：

```text
query_file,prediction,latency_s,status,notes
```

## 4. GCard 图模式到 FactorJoin join 查询的转换

位置：`run_factorjoin_ldbc.py::build_factorjoin_query(...)`

转换规则：

- 每条图边变成一个 FactorJoin 表 alias：`e<edge_id>`。
  - 例如边 label `person_knows_person` 会生成 `person_knows_person AS e0`。
- 图中同一个 vertex 如果连接了多条边，则通过边表的 `src` / `dst` 字段建立等值 join。
  - 例如同一个点同时出现在 `e0.dst` 和 `e1.src`，会生成 `e1.src = e0.dst`。
- 如果 vertex 上有谓词，则额外引入点表 alias：`v<vertex_id>`。
  - 例如 `person` 点有谓词，会生成 `person AS v3`，并加上 `v3.id = eX.src/dst`。
- edge 谓词直接作用在对应边表 alias 上。
- 谓词操作符从 GCard 风格转换为 SQL 风格：`eq/ge/le/gt/lt` -> `=/>=/<=/>/<`。
- categorical 字符串值按 FactorJoin parser 的能力做了简化处理，不额外加 SQL 引号。

这一步生成的不只是 SQL 字符串，还会生成 FactorJoin 需要的：

- `alias_to_table`
- `join_conditions`
- `table_queries`
- `subplans`
- `has_self_join`

## 5. 支持 alias 和 self-join 的适配层

原始 FactorJoin 模型里的 BN 和 bucket 都按物理表名存储，例如 `person`、`person_knows_person`。但 LDBC 查询里同一张表可能出现多次，例如多条 `person_knows_person` 边，必须区分不同 alias。

位置：`run_factorjoin_ldbc.py`

新增了几个轻量适配对象/函数：

- `AliasBN`
  - 包装原始 BN。
  - 把 alias 属性名映射回物理属性名做 `query_id_prob`。
  - 再把返回的 id 属性名映射回 alias。
- `make_alias_bucket(...)`
  - 复制物理表 bucket。
  - 把 bucket 中的 key、bin size、mode 信息改写成 alias 属性名。
- `make_alias_model(...)`
  - 复制原始模型。
  - 用 alias 版本的 BN、bucket、`equivalent_keys` 替换原始表名版本。
  - 重写 `parse_query_simple`，让 FactorJoin 跳过 SQL parser，直接使用我们从 GCard JSON 构造好的 tables、filters、joins、join_keys。

这样可以让 FactorJoin 的核心 join-bound 逻辑继续工作，同时支持：

- 多 alias 查询；
- 同一物理表重复出现；
- 图结构中的边表 join；
- vertex/edge predicates。

## 6. subplan 构造

位置：`run_factorjoin_ldbc.py::build_left_deep_subplans(...)`

FactorJoin 的 `get_cardinality_bound_all` 需要 sub-plan 顺序。LDBC runner 根据 alias join 图构造一个 connected left-deep plan：

- 从排序后的第一个 alias 开始；
- 每次选择一个和已 join 集合相连的 alias；
- 生成 `(new_alias, joined_aliases)` 形式的 subplan。

如果图不连通，会直接报错，避免给 FactorJoin 一个无意义的 join plan。

## 7. 估计结果处理

位置：`run_factorjoin_ldbc.py::query(...)`

估计时会记录：

- `ok`：FactorJoin 返回正常正数；
- `ok_floor`：FactorJoin 返回 `None`、非有限值或非正数，统一 floor 到 `1.0`；
- `failed: ...`：查询转换或估计异常。

`notes` 会标记当前查询是普通 alias subplan，还是包含 self-join：

```text
alias_subplan
alias_subplan_self_join
```

## 8. 未改动的部分

这次适配没有重写 FactorJoin 的核心算法：

- BN 训练仍使用原来的 `Bayescard_BN`。
- join key bucketization 仍使用 FactorJoin 原实现。
- join-bound 推理仍走 `Bound_ensemble.get_cardinality_bound_all(...)`。

新增逻辑主要集中在 LDBC schema 和查询适配层，目的是让 LDBC/GCard 风格图查询能映射到 FactorJoin 原本的关系 join 推理接口。
