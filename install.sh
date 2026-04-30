#!/usr/bin/env sh
# install.sh — handoff one-liner installer from GitHub Releases
set -e

REPO="0xedev/Handsoff"
BIN_DIR="${HANDOFF_BIN_DIR:-/usr/local/bin}"
TMP_DIR="$(mktemp -d)"

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

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
TAG="$(curl -sSfL "https://api.github.com/repos/${REPO}/tags?per_page=1" \
    | grep '"name"' | head -n 1 | cut -d'"' -f4)"

if [ -z "$TAG" ]; then
    echo "Could not determine latest tag" >&2
    exit 1
fi

BASE_URL="https://github.com/${REPO}/releases/download/${TAG}"
URL="${BASE_URL}/${ARTIFACT}"
CHECKSUM_URL="${BASE_URL}/${ARTIFACT}.sha256"

echo "Downloading handoff ${TAG} for ${ARTIFACT}..."
if curl -fsSfL "$URL" -o "$TMP_DIR/handoff-install"; then
    if curl -fsSfL "$CHECKSUM_URL" -o "$TMP_DIR/handoff-install.sha256" 2>/dev/null; then
        EXPECTED="$(awk '{print $1}' "$TMP_DIR/handoff-install.sha256")"
        verify_sha256 "$TMP_DIR/handoff-install" "$EXPECTED"
    else
        echo "  WARNING: checksum file not found for this release — skipping verification" >&2
    fi
    chmod +x "$TMP_DIR/handoff-install"
    sudo mv "$TMP_DIR/handoff-install" "${BIN_DIR}/handoff"
    echo "Installed handoff release ${TAG} to ${BIN_DIR}/handoff"
else
    echo "Release binary not available yet for ${TAG}; building from source..." >&2
    SRC_URL="https://github.com/${REPO}/archive/refs/tags/${TAG}.tar.gz"
    curl -fsSL "$SRC_URL" -o "$TMP_DIR/handoff-src.tar.gz"
    tar -xzf "$TMP_DIR/handoff-src.tar.gz" -C "$TMP_DIR"
    SRC_DIR="$(find "$TMP_DIR" -maxdepth 1 -type d | grep -E '/[Hh]andsoff-' | head -n 1)"
    if [ -z "$SRC_DIR" ]; then
        echo "Could not unpack source archive" >&2
        exit 1
    fi
    cargo build --locked --release --bin handoff --manifest-path "$SRC_DIR/rust/crates/cli/Cargo.toml"
    chmod +x "$SRC_DIR/rust/target/release/handoff"
    sudo mv "$SRC_DIR/rust/target/release/handoff" "${BIN_DIR}/handoff"
    echo "Installed handoff from source tag ${TAG} to ${BIN_DIR}/handoff"
fi

echo "Run: handoff init"
