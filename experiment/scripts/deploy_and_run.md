# 部署和运行实验指令

## 1. 本地同步到远程

```bash
./experiment/sync_remote.sh
```

## 2. 远程编译

```bash
cd ~/miniGU
cargo build -r --features std,serde,miette
```

## 3. 安装 DuckDB（venv 方式）

```bash
python3 -m venv ~/venv
~/venv/bin/pip install duckdb
source ~/venv/bin/activate
```

## 4. 重建 FlatGraph（含统计信息）+ Statistic

先删掉旧的 db 目录让 load_flatgraph 重新注册：

```bash
rm -rf /home/shuolin/miniGU/experiment/dataset/ldbc/sf1/minigu_db
mkdir -p /home/shuolin/miniGU/experiment/dataset/ldbc/sf1/minigu_db
```

确保 manifest.json 存在：

```bash
/home/shuolin/miniGU/target/release/minigu execute /dev/stdin --path /home/shuolin/miniGU/experiment/dataset/ldbc/sf1/minigu_db <<'EOF'
call build_ldbc_manifest('/home/shuolin/miniGU/experiment/dataset/ldbc/sf1', '/home/shuolin/miniGU/experiment/dataset/ldbc/sf1/manifest.json')
call load_flatgraph('/home/shuolin/miniGU/experiment/dataset/ldbc/sf1', 'ldbc')
EOF
```

然后进入 shell 构建 K=2 的统计信息：

```bash
/home/shuolin/miniGU/target/release/minigu shell /home/shuolin/miniGU/experiment/dataset/ldbc/sf1/minigu_db
```

```sql
session set graph ldbc
call create_catalog('ldbc', 2, 0)
```

## 5. 删掉旧备份

```bash
rm -rf /home/shuolin/miniGU/experiment/result/insert_impact/.backup
```

## 6. 运行实验

前台运行：

```bash
/home/shuolin/miniGU/experiment/scripts/run_insert_impact.sh \
    /home/shuolin/miniGU/experiment/dataset/ldbc/sf1/minigu_db \
    /home/shuolin/miniGU/experiment/dataset/ldbc/sf1
```

后台运行：

```bash
nohup /home/shuolin/miniGU/experiment/scripts/run_insert_impact.sh \
    /home/shuolin/miniGU/experiment/dataset/ldbc/sf1/minigu_db \
    /home/shuolin/miniGU/experiment/dataset/ldbc/sf1 \
    > /home/shuolin/miniGU/experiment/result/insert_impact/run.log 2>&1 &
```

查看进度：

```bash
tail -f /home/shuolin/miniGU/experiment/result/insert_impact/run.log
```
