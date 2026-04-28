#!/usr/bin/env sh
# install.sh — handoff one-liner installer
set -e

REPO="0xedev/handoff"
BIN_DIR="${HANDOFF_BIN_DIR:-/usr/local/bin}"

detect_target() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "$OS-$ARCH" in
        Linux-x86_64) echo "handoff-linux-x86_64" ;;
        Darwin-x86_64) echo "handoff-macos-x86_64" ;;
        Darwin-arm64) echo "handoff-macos-aarch64" ;;
        *) echo "Unsupported platform: $OS $ARCH" >&2; exit 1 ;;
    esac
}

ARTIFACT="$(detect_target)"
TAG="$(curl -sSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ARTIFACT}"

echo "Downloading handoff ${TAG} for ${ARTIFACT}..."
curl -sSL "$URL" -o /tmp/handoff-install
chmod +x /tmp/handoff-install
sudo mv /tmp/handoff-install "${BIN_DIR}/handoff"

echo "Installed handoff to ${BIN_DIR}/handoff"
echo "Run: handoff setup"
