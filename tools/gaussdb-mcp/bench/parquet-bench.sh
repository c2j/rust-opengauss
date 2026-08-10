#!/usr/bin/env bash
# Parquet vs CSV export benchmark for gaussdb cli.
#
# Iterates {1k, 10k, 100k, 1M, 10M} rows × {csv, parquet-snappy, parquet-zstd}
# and prints wall-clock time, peak RSS, file size, rows/sec.
#
# Usage:
#   cargo build --release -p gaussdb-mcp
#   export GAUSSDB_URL="host=127.0.0.1 port=5433 user=gaussdb password=…"
#   bash tools/gaussdb-mcp/bench/parquet-bench.sh
#
# Each row is ~200 bytes (md5 payload dominates). See
# docs/ParquetExportBenchmark.md for methodology and analysis.

set -u

if [ -z "${GAUSSDB_URL:-}" ]; then
    echo "error: GAUSSDB_URL must be set" >&2
    exit 2
fi

GAUSSDB="${GAUSSDB:-./target/release/gaussdb}"
if [ ! -x "$GAUSSDB" ]; then
    echo "error: $GAUSSDB not found. Run 'cargo build --release -p gaussdb-mcp' first." >&2
    exit 2
fi

# 6-column schema, ~200 bytes/row.
SQL_TEMPLATE='SELECT n::int8 AS id, ('"'"'2026-01-01'"'"'::timestamptz + (n || '"'"' seconds'"'"')::interval) AS ts, (n % 1000000)::int4 AS user_id, (n::numeric * 12345.67) AS amount, (CASE n % 3 WHEN 0 THEN '"'"'active'"'"' WHEN 1 THEN '"'"'pending'"'"' ELSE '"'"'closed'"'"' END) AS status, (md5(n::text) || md5((n+1)::text) || md5((n+2)::text) || md5((n+3)::text) || substr(md5((n+5)::text), 1, 22)) AS payload FROM generate_series(1, ROWS) AS t(n)'

run_one() {
    local rows=$1
    local fmt=$2
    local extra=$3
    local label=$4
    local out="/tmp/perf_${rows}_${label}.bin"
    local sql="${SQL_TEMPLATE/ROWS/$rows}"
    rm -f "$out"
    local attempt=0
    local ec=1
    while [ $ec -ne 0 ] && [ $attempt -lt 3 ]; do
        attempt=$((attempt + 1))
        /usr/bin/time -v -o /tmp/perf_time.out \
            sh -c "$GAUSSDB cli --sql \"$sql\" --format $fmt $extra > $out 2>/tmp/perf_gaussdb.err"
        ec=$?
    done
    if [ $ec -ne 0 ]; then
        printf "%-10s %-17s FAIL (after %d tries): %s\n" \
            "$rows" "$label" "$attempt" "$(head -c 120 /tmp/perf_gaussdb.err)"
        return
    fi
    local elapsed
    elapsed=$(grep "Elapsed (wall clock)" /tmp/perf_time.out | awk '{print $NF}')
    local rss
    rss=$(grep "Maximum resident" /tmp/perf_time.out | awk '{print $NF}')
    local size
    size=$(stat -c%s "$out")
    local elapsed_sec
    elapsed_sec=$(echo "$elapsed" | awk -F: '{print ($1 * 60) + $2}')
    local rps
    rps=$(awk "BEGIN{printf \"%.0f\", $rows / $elapsed_sec}")
    local mbps
    mbps=$(awk "BEGIN{printf \"%.1f\", ($size / 1048576) / $elapsed_sec}")
    printf "%-10s %-17s %10s %12s KB %12s B %12s r/s %10s MB/s\n" \
        "$rows" "$label" "$elapsed" "$rss" "$size" "$rps" "$mbps"
}

printf "%-10s %-17s %10s %15s %13s %13s %13s\n" \
    "ROWS" "FORMAT" "ELAPSED" "MAX-RSS" "FILE-SIZE" "ROWS/SEC" "THROUGHPUT"
printf "%s\n" "------------------------------------------------------------------------------------------"

for rows in 1000 10000 100000 1000000 10000000; do
    run_one "$rows" csv "" "csv"
    run_one "$rows" parquet "--parquet-compression snappy" "parquet-snap"
    run_one "$rows" parquet "--parquet-compression zstd" "parquet-zstd"
    echo ""
done
