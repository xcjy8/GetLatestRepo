#!/bin/bash
# GetLatestRepo installation script

set -e

echo "=========================================="
echo "GetLatestRepo Installation Script"
echo "=========================================="
echo ""

# Detect operating system
OS="$(uname -s)"
ARCH="$(uname -m)"

echo "Detected system: $OS $ARCH"

# Check if already compiled
if [ ! -f "../target/release/getlatestrepo" ]; then
    echo "Compiled getlatestrepo not found, starting build..."
    cd ..
    cargo build --release
    cd scripts
fi

# Installation path
INSTALL_DIR="/usr/local/bin"
GETLATESTREPO_BIN="../target/release/getlatestrepo"

echo ""
echo "Installing getlatestrepo to $INSTALL_DIR ..."

# Check permissions
if [ ! -w "$INSTALL_DIR" ]; then
    echo "Administrator privileges required to install to $INSTALL_DIR"
    sudo cp "$GETLATESTREPO_BIN" "$INSTALL_DIR/getlatestrepo"
    sudo chmod +x "$INSTALL_DIR/getlatestrepo"
else
    cp "$GETLATESTREPO_BIN" "$INSTALL_DIR/getlatestrepo"
    chmod +x "$INSTALL_DIR/getlatestrepo"
fi

echo "✓ Installation complete!"
echo ""
echo "Verify installation:"
getlatestrepo --version

echo ""
echo "Usage:"
echo "  getlatestrepo init <path>     # Initialize"
echo "  getlatestrepo workflow daily  # Daily check"
echo ""
echo "View help: getlatestrepo --help"
