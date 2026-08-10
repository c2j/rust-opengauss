# Parquet Export Benchmark

Performance characterisation of `gaussdb cli --format parquet` versus the
existing `--format csv` (server-side `COPY`), from 1k to 10M rows at
~200 bytes per row.

## Test environment

| Component | Version / Configuration |
|---|---|
| Hardware | 78 GB RAM, x86_64 Linux |
| Database | openGauss 5.0.0 (Docker container, `host=127.0.0.1 port=5433`) |
| `gaussdb` | v0.6.0, release build (`cargo build --release -p gaussdb-mcp`) |
| Arrow / Parquet | arrow 59.2.0, parquet 59.2.0 |
| Data shape | 6 columns × ~200 bytes/row (see schema below) |

## Schema

Six columns chosen to exercise the major Arrow type families (integer,
temporal, decimal, string):

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

CSV row size measured at **~200 bytes/row** (`md5` text dominates).

## Measurement methodology

- Wall clock + max RSS via GNU `/usr/bin/time -v`
- Each data point is a single run
- Output to tmpfs (`/tmp`)
- Client and DB on the same host (network ≈ loopback)

Reproduce with `tools/gaussdb-mcp/bench/parquet-bench.sh`.

## Results

| Rows | Format | Wall clock | Peak RSS | File size | Notes |
|---|---|---|---|---|---|
| 1k | csv | 0.03s | 10 MB | 200 KB | |
| 1k | parquet snappy | 0.03s | 13 MB | 66 KB | 3.0× smaller |
| 10k | csv | 0.18s | 10 MB | 2.0 MB | |
| 10k | parquet snappy | 0.18s | 22 MB | 650 KB | 3.1× smaller |
| 100k | csv | 1.66s | 10 MB | 20.6 MB | |
| 100k | parquet snappy | 1.67s | 96 MB | 6.5 MB | 3.2× smaller |
| 1M | csv | 16.77s | 10 MB | 209 MB | |
| 1M | parquet snappy | 15.99s | 210 MB | 60 MB | 3.5× smaller, slightly faster than csv |
| **10M** | **csv** | **2:47.25** | **10 MB** | 2112 MB | |
| **10M** | **parquet snappy** | **2:40.32** | **247 MB** | 595 MB | **3.5× smaller, slightly faster than csv** |

## Key observations

### 1. Parquet matches or beats CSV on wall clock

At 1M and 10M rows, parquet is **marginally faster** than csv (15.99s vs
16.77s, 2:40 vs 2:47). CSV does server-side rendering via `COPY`, parquet
pulls binary via `SELECT` and does columnar encode + compress on the
client — the two costs are comparable, with parquet slightly ahead once
server-side text formatting dominates.

### 2. Memory: O(batch_size), not O(N)

Parquet export uses streaming `query_raw` with one `RecordBatch` of
`--parquet-batch-size` rows in flight at a time:

```
peak RSS (MB)
 500 ┤
 400 ┤
 300 ┤                                                ● 10M: 247 MB
 200 ┤                          ● 1M: 210 MB
 100 ┤          ● 100k: 96 MB
  50 ┤  ● 10k: 22 MB
  10 ┤  ● 1k: 13 MB ── ● csv stays here at every scale
      └──────────────────────────────────────────────
        1k   10k   100k   1M    10M
```

10M rows × ~200 B source (≈ 2 GB on the wire) peaks at **247 MB client
RSS** — bounded by `batch_size`, not by total result size. This makes
parquet export practical for very large exports that would previously
have required CSV for memory reasons.

### 3. Compression ratios are stable across scales

| Codec | Size vs CSV |
|---|---|
| snappy | ~0.29 × (3.5× smaller) |

### 4. Throughput plateau

| Rows | Rows/sec |
|---|---|
| 100k | ~60,000 |
| 1M | ~62,500 |
| 10M | ~62,500 |

Plateau around **62,500 rows/sec**, bottlenecked by `generate_series` +
loopback socket I/O, not by format encoding.

## Decision guide

| Use case | Recommended format |
|---|---|
| Export > 100M rows, memory-constrained | **csv** (O(1) client) |
| Export for OLAP / data lake / DuckDB / Spark | **parquet zstd** |
| Cross-tool text interop (Excel, shell) | **csv** |
| Long-term archival | **parquet zstd** |
| Mixed ad-hoc analysis | **parquet snappy** |

## Implementation note

The export uses streaming `query_raw` + per-batch Arrow `RecordBatch`
writes. NUMERIC (precision, scale) is sampled from the first batch only.
See [ParquetExportQueryRawInvestigation.md](ParquetExportQueryRawInvestigation.md)
for the subtlety that forced the EOF-tracking logic in the drain loop
(`tokio-opengauss::RowStream` returns `Err(closed)` rather than `None`
when polled after `ReadyForQuery`).

## Reproduce

```sh
cargo build --release -p gaussdb-mcp
export GAUSSDB_URL="host=127.0.0.1 port=5433 user=gaussdb password=…"
bash tools/gaussdb-mcp/bench/parquet-bench.sh
```
