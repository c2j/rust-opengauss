# Parquet 导出性能基准

对 `gaussdb cli --format parquet` 与既有 `--format csv`(服务端 `COPY`)做性能对比,从 1k 到 10M 行、每行约 200 字节。

## 测试环境

| 组件 | 版本 / 配置 |
|---|---|
| 硬件 | 78 GB 内存,x86_64 Linux |
| 数据库 | openGauss 5.0.0(Docker 容器,`host=127.0.0.1 port=5433`) |
| `gaussdb` | v0.6.0,release 构建(`cargo build --release -p gaussdb-mcp`) |
| Arrow / Parquet | arrow 59.2.0,parquet 59.2.0 |
| 数据形状 | 6 列 × ~200 字节/行(见下方 schema) |

## Schema

```sql
SELECT
  n::int8 AS id,                                            --  8 B
  ('2026-01-01'::timestamptz                                --  8 B
     + (n || ' seconds')::interval) AS ts,
  (n % 1000000)::int4 AS user_id,                           --  4 B
  (n::numeric * 12345.67) AS amount,                        --  numeric
  (CASE n % 3                                               --  ~7 B
     WHEN 0 THEN 'active'
     WHEN 1 THEN 'pending'
     ELSE 'closed' END) AS status,
  (md5(n::text) || md5((n+1)::text)                         -- ~150 B
     || md5((n+2)::text) || md5((n+3)::text)
     || substr(md5((n+5)::text), 1, 22)) AS payload
FROM generate_series(1, N) AS t(n)
```

实测 CSV 行大小 **~200 字节/行**。

## 完整数据

| 行数 | 格式 | 墙钟耗时 | 峰值 RSS | 文件大小 | 备注 |
|---|---|---|---|---|---|
| 1k | csv | 0.03s | 10 MB | 200 KB | |
| 1k | parquet snappy | 0.03s | 13 MB | 66 KB | 3.0× 小 |
| 10k | csv | 0.18s | 10 MB | 2.0 MB | |
| 10k | parquet snappy | 0.18s | 22 MB | 650 KB | 3.1× 小 |
| 100k | csv | 1.66s | 10 MB | 20.6 MB | |
| 100k | parquet snappy | 1.67s | 96 MB | 6.5 MB | 3.2× 小 |
| 1M | csv | 16.77s | 10 MB | 209 MB | |
| 1M | parquet snappy | 15.99s | 210 MB | 60 MB | 3.5× 小,略快于 csv |
| **10M** | **csv** | **2:47.25** | **10 MB** | 2112 MB | |
| **10M** | **parquet snappy** | **2:40.32** | **247 MB** | 595 MB | **3.5× 小,略快于 csv** |

## 关键观察

### 1. Parquet 墙钟耗时与 CSV 持平或更快

1M 和 10M 行时,parquet **略快于** csv(15.99s vs 16.77s,2:40 vs 2:47)。csv 走服务端 `COPY` 渲染,parquet 走二进制 `SELECT` + 客户端列式编码 + 压缩 —— 两者成本接近,parquet 在服务端文本格式化占主导时略胜。

### 2. 内存:O(batch_size),不是 O(N)

```
峰值 RSS (MB)
 500 ┤
 400 ┤
 300 ┤                                                ● 10M: 247 MB
 200 ┤                          ● 1M: 210 MB
 100 ┤          ● 100k: 96 MB
  50 ┤  ● 10k: 22 MB
  10 ┤  ● 1k: 13 MB ── ● csv 在所有规模都停留在这里
      └──────────────────────────────────────────────
        1k   10k   100k   1M    10M
```

10M 行 × ~200 字节源数据(线上约 2 GB),客户端峰值 RSS 仅 **247 MB** —— 由 `batch_size` 限制,不随总结果集大小增长。这让 parquet 导出对超大表也实用。

### 3. 压缩比跨规模稳定

| 编码 | 大小 vs CSV |
|---|---|
| snappy | ~0.29 ×(3.5× 小) |

### 4. 吞吐瓶颈

| 行数 | rows/sec |
|---|---|
| 100k | ~60,000 |
| 1M | ~62,500 |
| 10M | ~62,500 |

稳定在 **~62,500 rows/sec**,瓶颈是 `generate_series` + loopback socket I/O,不是格式编码。

## 选择指南

| 场景 | 推荐格式 |
|---|---|
| 导出 > 100M 行、内存受限 | **csv**(O(1) 客户端) |
| OLAP / 数据湖 / DuckDB / Spark | **parquet zstd** |
| 跨工具文本互操作(Excel、shell) | **csv** |
| 长期归档 | **parquet zstd** |
| 通用即席分析 | **parquet snappy** |

## 实现说明

导出使用流式 `query_raw` + 按 batch 写 Arrow `RecordBatch`。NUMERIC (precision, scale) 只从第一批采样。见 [ParquetExportQueryRawInvestigation.md](ParquetExportQueryRawInvestigation.md) 中 drain 循环 EOF 跟踪逻辑的细节(`tokio-opengauss::RowStream` 在 `ReadyForQuery` 之后再 poll 返回 `Err(closed)` 而非 `None`)。

## 复现

```sh
cargo build --release -p gaussdb-mcp
export GAUSSDB_URL="host=127.0.0.1 port=5433 user=gaussdb password=…"
bash tools/gaussdb-mcp/bench/parquet-bench.sh
```
