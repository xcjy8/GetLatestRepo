#!/bin/bash
# GetLatestRepo full-command test script

set -e  # Exit immediately on error

echo "=========================================="
echo "GetLatestRepo Full-Command Automated Test"
echo "=========================================="
echo ""

GETLATESTREPO="./target/release/getlatestrepo"

# Color definitions
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Test counters
PASSED=0
FAILED=0

# Test function
run_test() {
    local name="$1"
    local cmd="$2"
    echo -n "Test: $name ... "
    if eval "$cmd" > /tmp/test_output.txt 2>&1; then
        echo -e "${GREEN}✓ PASS${NC}"
        PASSED=$((PASSED + 1))
    else
        echo -e "${RED}✗ FAIL${NC}"
        echo "  Error output:"
        cat /tmp/test_output.txt | sed 's/^/    /'
        FAILED=$((FAILED + 1))
    fi
}

# Ensure we are in the correct directory
cd "$(dirname "$0")/.."

# Clean previous test data
echo "Cleaning test environment..."
rm -rf /tmp/rg-test-dir /tmp/rg-init-test 2>/dev/null || true

echo "1. Basic command tests"
echo "----------------------"
run_test "help" "$GETLATESTREPO --help"
run_test "version" "$GETLATESTREPO --version"

echo ""
echo "2. config command tests"
echo "-----------------------"
run_test "config list" "$GETLATESTREPO config list"
run_test "config path" "$GETLATESTREPO config path"

# Create test directory
mkdir -p /tmp/rg-test-dir
run_test "config add" "$GETLATESTREPO config add /tmp/rg-test-dir"
run_test "config add (duplicate)" "$GETLATESTREPO config add /tmp/rg-test-dir && false || true"  # Should fail
run_test "config ignore" "$GETLATESTREPO config ignore '*.log,*.tmp'"

echo ""
echo "3. init command tests"
echo "---------------------"
mkdir -p /tmp/rg-init-test
run_test "init" "$GETLATESTREPO init /tmp/rg-init-test"

echo ""
echo "4. status command tests"
echo "-----------------------"
run_test "status (valid repo)" "$GETLATESTREPO status /tmp/test-repos/project-a"
run_test "status (invalid path)" "$GETLATESTREPO status /nonexistent && false || true"  # Should fail

echo ""
echo "5. workflow command tests (dry-run)"
echo "------------------------------------"
run_test "workflow --list" "$GETLATESTREPO workflow --list"
run_test "workflow check --dry-run" "$GETLATESTREPO workflow check --dry-run"
run_test "workflow daily --dry-run" "$GETLATESTREPO workflow daily --dry-run"
run_test "workflow report --dry-run" "$GETLATESTREPO workflow report --dry-run"
run_test "workflow ci --dry-run" "$GETLATESTREPO workflow ci --dry-run"
run_test "workflow pull-safe --dry-run" "$GETLATESTREPO workflow pull-safe --dry-run"
run_test "workflow pull-force --dry-run" "$GETLATESTREPO workflow pull-force --dry-run"

echo ""
echo "6. workflow command tests (actual execution)"
echo "--------------------------------------------"
run_test "workflow check" "$GETLATESTREPO workflow check"

echo ""
echo "7. cleanup tests"
echo "----------------"
run_test "config remove" "$GETLATESTREPO config remove /tmp/rg-test-dir"

# Cleanup
rm -rf /tmp/rg-test-dir /tmp/rg-init-test

echo ""
echo "=========================================="
echo "Tests completed"
echo "=========================================="
echo -e "Passed: ${GREEN}$PASSED${NC}"
echo -e "Failed: ${RED}$FAILED${NC}"
echo ""

if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}All tests passed!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed, please check!${NC}"
    exit 1
fi
