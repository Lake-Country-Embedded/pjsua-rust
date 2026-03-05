#!/usr/bin/env bash
#
# Run leak tests under Valgrind to detect memory leaks in FFI code.
#
# Usage:
#   ./scripts/valgrind-test.sh           # run all leak tests
#   ./scripts/valgrind-test.sh <test>    # run a specific test
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

echo "=== Building leak tests ==="
cargo test --features integration-tests --test leak_tests --no-run 2>&1

# Find the test binary
TEST_BIN=$(cargo test --features integration-tests --test leak_tests --no-run 2>&1 \
    | grep -oP 'Executable.*\(\K[^)]+')

if [ -z "$TEST_BIN" ]; then
    echo "ERROR: Could not find leak_tests binary"
    exit 1
fi

echo "=== Test binary: $TEST_BIN ==="

# Build valgrind args
VALGRIND_ARGS=(
    --leak-check=full
    --show-leak-kinds=definite,possible
    --errors-for-leak-kinds=definite,possible
    --error-exitcode=1
    --track-origins=yes
    --suppressions="$PROJECT_DIR/valgrind.supp"
)

TEST_ARGS=(--test-threads=1)

# If a specific test name was given, filter to it
if [ "${1:-}" != "" ]; then
    TEST_ARGS+=(--exact "$1")
fi

echo "=== Running under Valgrind ==="
echo "valgrind ${VALGRIND_ARGS[*]} $TEST_BIN ${TEST_ARGS[*]}"
echo ""

valgrind "${VALGRIND_ARGS[@]}" "$TEST_BIN" "${TEST_ARGS[@]}"

EXIT_CODE=$?
echo ""
if [ $EXIT_CODE -eq 0 ]; then
    echo "=== PASS: No memory leaks detected ==="
else
    echo "=== FAIL: Memory leaks detected (exit code $EXIT_CODE) ==="
fi

exit $EXIT_CODE
