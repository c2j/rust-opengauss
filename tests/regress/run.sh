#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Regression test runner for gaussdb-mcp type handling (issue #39)
#
# Spins up an openGauss container, runs the integration tests in
# tests/regress/, and tears down. Designed for CI and local dev.
#
# Prerequisites: docker, cargo (Rust toolchain)
#
# Overrides:
#   OG_IMAGE   openGauss Docker image (default: opengauss/opengauss:5.0.0)
#   OG_PORT    host port mapping          (default: 5433)
#   OG_PASSWORD superuser password        (default: Test@12345)
#   OG_DB      database name              (default: postgres)
#
# Usage:
#   ./tests/regress/run.sh
#   OG_IMAGE=opengauss/opengauss:3.0.0 ./tests/regress/run.sh
# ============================================================================

OG_IMAGE="${OG_IMAGE:-opengauss/opengauss:5.0.0}"
OG_PORT="${OG_PORT:-5433}"
OG_PASSWORD="${OG_PASSWORD:-Test@12345}"
OG_DB="${OG_DB:-postgres}"
OG_USER="gaussdb"
ADMIN_USER="gaussdb"
GSQL="export LD_LIBRARY_PATH=/usr/local/opengauss/lib:\$LD_LIBRARY_PATH && /usr/local/opengauss/bin/gsql"
CONTAINER_NAME="gaussdb-regress-$$"

cleanup() {
    echo "==> Tearing down container ${CONTAINER_NAME}..."
    docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> Pulling ${OG_IMAGE}..."
docker pull "${OG_IMAGE}" --quiet 2>/dev/null || true

echo "==> Starting ${OG_IMAGE} as ${CONTAINER_NAME} on port ${OG_PORT}..."
docker run -d --rm \
    --name "${CONTAINER_NAME}" \
    --privileged \
    -p "${OG_PORT}:5432" \
    -e GS_PASSWORD="${OG_PASSWORD}" \
    "${OG_IMAGE}" \
    >/dev/null

echo "==> Waiting for openGauss to be ready (this may take a few minutes)..."
MAX_WAIT=600
ELAPSED=0
READY=0
while [ $ELAPSED -lt $MAX_WAIT ]; do
    if ! docker inspect --format='{{.State.Running}}' "${CONTAINER_NAME}" 2>/dev/null | grep -q true; then
        echo "ERROR: Container ${CONTAINER_NAME} exited prematurely"
        docker logs "${CONTAINER_NAME}" --tail 30
        exit 1
    fi
    if docker exec "${CONTAINER_NAME}" \
        bash -c "${GSQL} -d ${OG_DB} -U ${ADMIN_USER} -W ${OG_PASSWORD} -c 'SELECT 1'" >/dev/null 2>&1; then
        READY=1
        break
    fi
    printf "." >&2
    sleep 5
    ELAPSED=$((ELAPSED + 5))
done
echo ""

if [ $READY -eq 0 ]; then
    echo "ERROR: openGauss did not become ready within ${MAX_WAIT}s"
    docker logs "${CONTAINER_NAME}" --tail 20
    exit 1
fi
echo "==> openGauss ready after ${ELAPSED}s"

export GAUSSDB_TEST_URL="host=127.0.0.1 port=${OG_PORT} user=${OG_USER} password=${OG_PASSWORD} dbname=${OG_DB} sslmode=disable"

echo "==> Running regression tests..."
cd "$(dirname "$0")/../.."
cargo test -p regress --features integration -- --test-threads=2
RC=$?

if [ $RC -eq 0 ]; then
    echo "==> All regression tests passed."
else
    echo "==> Regression tests FAILED (exit code ${RC})."
fi

exit $RC
