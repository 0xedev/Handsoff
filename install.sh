#!/usr/bin/env sh
# install.sh — handoff one-liner installer from GitHub Releases
set -e

REPO="0xedev/handoff"
BIN_DIR="${HANDOFF_BIN_DIR:-/usr/local/bin}"

detect_target() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "$OS-$ARCH" in
        Linux-x86_64)  echo "handoff-linux-x86_64" ;;
        Darwin-x86_64) echo "handoff-macos-x86_64" ;;
        Darwin-arm64)  echo "handoff-macos-aarch64" ;;
        *) echo "Unsupported platform: $OS $ARCH" >&2; exit 1 ;;
    esac
}

verify_sha256() {
    FILE="$1"
    EXPECTED="$2"
    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "$FILE" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 "$FILE" | awk '{print $1}')"
    else
        echo "WARNING: no sha256sum or shasum found — skipping checksum verification" >&2
        return 0
    fi
    if [ "$ACTUAL" != "$EXPECTED" ]; then
        echo "Checksum mismatch!" >&2
        echo "  expected: $EXPECTED" >&2
        echo "  got:      $ACTUAL" >&2
        rm -f "$FILE"
        exit 1
    fi
    echo "  ✓ Checksum verified"
}

ARTIFACT="$(detect_target)"
TAG="$(curl -sSfL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | cut -d'"' -f4)"

if [ -z "$TAG" ]; then
    echo "Could not determine latest release tag" >&2
    exit 1
fi

BASE_URL="https://github.com/${REPO}/releases/download/${TAG}"
URL="${BASE_URL}/${ARTIFACT}"
CHECKSUM_URL="${BASE_URL}/${ARTIFACT}.sha256"

echo "Downloading handoff ${TAG} for ${ARTIFACT}..."
curl -sSfL "$URL" -o /tmp/handoff-install
curl -sSfL "$CHECKSUM_URL" -o /tmp/handoff-install.sha256 2>/dev/null && \
    EXPECTED="$(cat /tmp/handoff-install.sha256 | awk '{print $1}')" && \
    verify_sha256 /tmp/handoff-install "$EXPECTED" || \
    echo "  WARNING: checksum file not found for this release — skipping verification" >&2

chmod +x /tmp/handoff-install
sudo mv /tmp/handoff-install "${BIN_DIR}/handoff"
rm -f /tmp/handoff-install.sha256

echo "Installed handoff to ${BIN_DIR}/handoff"
echo "Run: handoff init"
