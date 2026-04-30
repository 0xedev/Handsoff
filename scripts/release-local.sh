#!/usr/bin/env bash
# Build and publish a Handsoff release asset from the local machine.
#
# This is the fallback path when GitHub Actions is unavailable. It publishes
# the binary for the current OS/CPU only.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/release-local.sh [tag] [--no-upload]

Examples:
  scripts/release-local.sh
  scripts/release-local.sh v0.4.1-alpha.2
  scripts/release-local.sh --no-upload

Environment:
  HANDOFF_REPO=owner/repo   GitHub repository, default: 0xedev/Handsoff
USAGE
}

REPO="${HANDOFF_REPO:-0xedev/Handsoff}"
TAG=""
UPLOAD=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --no-upload)
      UPLOAD=0
      ;;
    v*)
      TAG="$1"
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/rust/Cargo.toml" | head -n 1)"
if [[ -z "$TAG" ]]; then
  TAG="v$VERSION"
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    ARTIFACT="handoff-linux-x86_64"
    ;;
  Darwin-x86_64)
    ARTIFACT="handoff-macos-x86_64"
    ;;
  Darwin-arm64)
    ARTIFACT="handoff-macos-aarch64"
    ;;
  *)
    echo "unsupported platform: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

DIST="$ROOT/dist"
BIN="$ROOT/rust/target/release/handoff"
TARGET_SHA="$(git -C "$ROOT" rev-parse HEAD)"

echo "Building handoff $TAG for $ARTIFACT..."
cargo build --locked --release --bin handoff --manifest-path "$ROOT/rust/crates/cli/Cargo.toml"

mkdir -p "$DIST"
cp "$BIN" "$DIST/$ARTIFACT"
chmod +x "$DIST/$ARTIFACT"

if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$DIST/$ARTIFACT" > "$DIST/$ARTIFACT.sha256"
else
  sha256sum "$DIST/$ARTIFACT" > "$DIST/$ARTIFACT.sha256"
fi

echo "Built:"
echo "  $DIST/$ARTIFACT"
echo "  $DIST/$ARTIFACT.sha256"

if [[ "$UPLOAD" -eq 0 ]]; then
  echo "Skipped upload because --no-upload was provided."
  exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "GitHub CLI not found. Install gh or rerun with --no-upload." >&2
  exit 1
fi

if ! gh auth status >/dev/null 2>&1; then
  echo "GitHub CLI is not authenticated. Run: gh auth login" >&2
  exit 1
fi

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "Uploading assets to existing release $TAG..."
  gh release upload "$TAG" "$DIST/$ARTIFACT" "$DIST/$ARTIFACT.sha256" \
    --repo "$REPO" \
    --clobber
else
  echo "Creating release $TAG and uploading assets..."
  gh release create "$TAG" "$DIST/$ARTIFACT" "$DIST/$ARTIFACT.sha256" \
    --repo "$REPO" \
    --target "$TARGET_SHA" \
    --title "$TAG" \
    --notes "Local release for $TAG."
fi

echo "Published $ARTIFACT to https://github.com/$REPO/releases/tag/$TAG"
