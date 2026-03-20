#!/bin/sh
set -eu

# Install composer binary.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/architect-xyz/composer/main/install.sh | sh
#   curl -fsSL ... | sh -s -- --version v0.10.5
#   curl -fsSL ... | sh -s -- --to /opt/bin

VERSION="latest"
INSTALL_DIR="/usr/local/bin"

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --to) INSTALL_DIR="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)  ARCH="amd64" ;;
    aarch64) ARCH="arm64" ;;
    arm64)   ARCH="arm64" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

BINARY="composer-${OS}-${ARCH}"
REPO="architect-xyz/composer"

if [ "$VERSION" = "latest" ]; then
    URL="https://github.com/${REPO}/releases/latest/download/${BINARY}"
else
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${BINARY}"
fi

echo "Downloading ${BINARY} (${VERSION})..."
curl -fsSL "$URL" -o "${INSTALL_DIR}/composer"
chmod +x "${INSTALL_DIR}/composer"

echo "Installed composer to ${INSTALL_DIR}/composer"
"${INSTALL_DIR}/composer" --version
