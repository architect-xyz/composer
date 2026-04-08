#!/bin/sh
set -eu

# Install composer binary to ~/.local/bin (no sudo required).
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/architect-xyz/composer/main/install.sh | sh
#   curl -fsSL ... | sh -s -- --version v0.10.5
#   curl -fsSL ... | sh -s -- --to /usr/local/bin   # system-wide (needs sudo)

VERSION="latest"
INSTALL_DIR=""

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --to) INSTALL_DIR="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# Default install directory: use ~/.local/bin (no sudo needed) unless overridden
if [ -z "$INSTALL_DIR" ]; then
    INSTALL_DIR="${HOME}/.local/bin"
fi

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

mkdir -p "$INSTALL_DIR"

echo "Downloading ${BINARY} (${VERSION})..."
curl -fsSL "$URL" -o "${INSTALL_DIR}/composer"
chmod +x "${INSTALL_DIR}/composer"

echo "Installed composer to ${INSTALL_DIR}/composer"
"${INSTALL_DIR}/composer" --version

# Warn if the install directory is not in PATH
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) echo "WARNING: ${INSTALL_DIR} is not in your PATH. Add it with:"
       echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
       ;;
esac
