#!/bin/sh
set -eu

# Install composer binary system-wide to /usr/local/bin (uses sudo by default).
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/architect-xyz/composer/main/install.sh | sh
#   curl -fsSL ... | sh -s -- --version v0.10.5
#   curl -fsSL ... | sh -s -- --local              # install to ~/.local/bin (no sudo)
#   curl -fsSL ... | sh -s -- --to /opt/bin        # custom directory

VERSION="latest"
INSTALL_DIR=""
LOCAL=0

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --to) INSTALL_DIR="$2"; shift 2 ;;
        --local) LOCAL=1; shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# Default install directory: /usr/local/bin (system-wide, uses sudo).
# With --local, use ~/.local/bin (no sudo). --to overrides both.
if [ -z "$INSTALL_DIR" ]; then
    if [ "$LOCAL" -eq 1 ]; then
        INSTALL_DIR="${HOME}/.local/bin"
    else
        INSTALL_DIR="/usr/local/bin"
    fi
fi

# Use sudo when the install directory isn't writable by the current user.
SUDO=""
if [ ! -w "$INSTALL_DIR" ] && [ ! -w "$(dirname "$INSTALL_DIR")" ]; then
    if [ "$(id -u)" -ne 0 ]; then
        if command -v sudo >/dev/null 2>&1; then
            SUDO="sudo"
        else
            echo "ERROR: ${INSTALL_DIR} is not writable and sudo is not available." >&2
            echo "Re-run with --local to install to ~/.local/bin instead." >&2
            exit 1
        fi
    fi
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

$SUDO mkdir -p "$INSTALL_DIR"

echo "Downloading ${BINARY} (${VERSION})..."
$SUDO curl -fsSL "$URL" -o "${INSTALL_DIR}/composer"
$SUDO chmod +x "${INSTALL_DIR}/composer"

echo "Installed composer to ${INSTALL_DIR}/composer"
"${INSTALL_DIR}/composer" --version

# Warn if the install directory is not in PATH
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) echo "WARNING: ${INSTALL_DIR} is not in your PATH. Add it with:"
       echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
       ;;
esac
