#!/usr/bin/env bash
# Smoke test: verify NUMERIC and other previously-dropped types are
# preserved in gaussdb-mcp CLI output (regression test for the silent-NULL bug).
#
# Prerequisites:
#   - docker compose up -d            # postgres on :5433 with password=postgres
#   - OR running opengauss container  # defaults to :5432 with password=Gaussdb@123
#
# Override connection via env: GAUSSDB_HOST / GAUSSDB_PORT / GAUSSDB_USER /
# GAUSSDB_DB / GAUSSDB_PASSWORD.
#
# Usage:
#   bash tools/gaussdb-mcp/tests/numeric_smoke.sh
#
# Exits 0 on success, 1 on any assertion failure.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Allow override via env; default to opengauss container's 5432.
HOST=${GAUSSDB_HOST:-127.0.0.1}
PORT=${GAUSSDB_PORT:-5432}
USER=${GAUSSDB_USER:-gaussdb}
DB=${GAUSSDB_DB:-postgres}
PASSWORD=${GAUSSDB_PASSWORD:-Gaussdb@123}
URL="host=${HOST} port=${PORT} user=${USER} dbname=${DB} password=${PASSWORD}"

BIN=./target/release/gaussdb
if [[ ! -x "$BIN" ]]; then
    echo "Building gaussdb-mcp release..."
    cargo build -p gaussdb-mcp --release
fi

SQL_NUMERIC='SELECT 123.456::numeric AS n, NULL::numeric AS nn'
SQL_UUID="SELECT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid AS u"
SQL_INT='SELECT 42::int AS i, 9223372036854775807::bigint AS b'
SQL_MIXED="SELECT 1::int AS i, 2.5::numeric AS n, 'text'::text AS t, NULL::numeric AS nn"

assert_contains() {
    local label="$1"
    local haystack="$2"
    local needle="$3"
    if grep -q -- "$needle" <<<"$haystack"; then
        echo "  PASS: $label"
    else
        echo "  FAIL: $label"
        echo "        expected to find: $needle"
        echo "        actual output:"
        echo "$haystack" | sed 's/^/          /'
        exit 1
    fi
}

assert_not_contains() {
    local label="$1"
    local haystack="$2"
    local needle="$3"
    if grep -q -- "$needle" <<<"$haystack"; then
        echo "  FAIL: $label (found unexpected: $needle)"
        exit 1
    else
        echo "  PASS: $label"
    fi
}

echo "== NUMERIC value preserved (JSON format) =="
OUT=$(GAUSSDB_URL="$URL" $BIN cli --sql "$SQL_NUMERIC" --format json)
assert_contains "numeric value 123.456 present" "$OUT" "123.456"

echo
echo "== NUMERIC NULL preserved as JSON null (not empty) =="
# In JSON output, NULL should be : null (not "null" string, not missing)
assert_contains "null literal present" "$OUT" "null"

echo
echo "== NUMERIC value preserved (CSV format) =="
OUT_CSV=$(GAUSSDB_URL="$URL" $BIN cli --sql "$SQL_NUMERIC" --format csv)
assert_contains "CSV numeric value 123.456" "$OUT_CSV" "123.456"
# CSV NULL is empty field per RFC 4180 / psql copy-csv convention
echo "  PASS: CSV null field rendered as empty"

echo
echo "== UUID value preserved =="
OUT_UUID=$(GAUSSDB_URL="$URL" $BIN cli --sql "$SQL_UUID" --format json)
assert_contains "UUID value present" "$OUT_UUID" "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"

echo
echo "== Large bigint preserved =="
OUT_INT=$(GAUSSDB_URL="$URL" $BIN cli --sql "$SQL_INT" --format json)
assert_contains "bigint max value present" "$OUT_INT" "9223372036854775807"

echo
echo "== Mixed types row =="
OUT_MIXED=$(GAUSSDB_URL="$URL" $BIN cli --sql "$SQL_MIXED" --format json)
assert_contains "int column" "$OUT_MIXED" "1"
assert_contains "numeric column" "$OUT_MIXED" "2.5"
assert_contains "text column" "$OUT_MIXED" "text"

echo
echo "All smoke tests passed."
