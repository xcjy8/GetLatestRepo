#!/bin/bash
# GetLatestRepo release build script

set -e

echo "=========================================="
echo "GetLatestRepo Release Build"
echo "=========================================="
echo ""

cd "$(dirname "$0")/.."

# Clean old build
echo "Cleaning old build..."
cargo clean

# Run tests
echo ""
echo "Running tests..."
./scripts/test-all.sh

# Build release version
echo ""
echo "Building release version..."
cargo build --release

# Check build result
if [ ! -f "target/release/getlatestrepo" ]; then
    echo "✗ Build failed"
    exit 1
fi

echo ""
echo "✓ Build successful!"
echo ""
echo "Binary: target/release/getlatestrepo"
echo "File size: $(ls -lh target/release/getlatestrepo | awk '{print $5}')"
echo ""
echo "Running test:"
./target/release/getlatestrepo --version

echo ""
echo "Release checklist:"
echo "  [ ] Version number updated"
echo "  [ ] CHANGELOG updated"
echo "  [ ] All tests passed"
echo "  [ ] Documentation updated"
