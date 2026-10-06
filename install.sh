#!/bin/sh
# docsbase installer — Linux x86_64.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | sh
#   DOCSBASE_CHANNEL=prerelease sh install.sh
#   DOCSBASE_VERSION=v0.1.0-alpha.3 sh install.sh
# Env:
#   DOCSBASE_CHANNEL  stable (default) | prerelease — channel for "latest"
#   DOCSBASE_VERSION  exact release tag (overrides the channel)
#   DOCSBASE_BIN_DIR  install dir (default: $HOME/.local/bin)
#   DOCSBASE_REPO     owner/repo (default: punkhomov/docsbase-memory-mcp)
set -eu

REPO="${DOCSBASE_REPO:-punkhomov/docsbase-memory-mcp}"
BIN_DIR="${DOCSBASE_BIN_DIR:-$HOME/.local/bin}"
VERSION="${DOCSBASE_VERSION:-latest}"
CHANNEL="${DOCSBASE_CHANNEL:-stable}"

fail() { echo "install.sh: error: $*" >&2; exit 1; }
info() { echo "install.sh: $*"; }

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

OS="$(uname -s 2>/dev/null || echo unknown)"
ARCH="$(uname -m 2>/dev/null || echo unknown)"
[ "$OS" = "Linux" ] || fail "this script is for Linux; on Windows use install.ps1 (or WSL2 + this script). Got: $OS"
case "$ARCH" in
  x86_64|amd64) ARCH="x86_64" ;;
  *) fail "only x86_64 is supported in v1. Got: $ARCH" ;;
esac

if [ "$VERSION" = "latest" ]; then
  case "$CHANNEL" in
    stable)
      info "resolving latest stable release tag for $REPO..."
      TAG="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep -m1 '"tag_name"' | cut -d'"' -f4)"
      [ -n "$TAG" ] || fail "no stable release published yet; rerun with DOCSBASE_CHANNEL=prerelease (or pin DOCSBASE_VERSION=vX.Y.Z)"
      ;;
    prerelease)
      info "resolving newest release tag (prereleases included) for $REPO..."
      TAG="$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" | grep -m1 '"tag_name"' | cut -d'"' -f4)"
      [ -n "$TAG" ] || fail "could not resolve any release (set DOCSBASE_VERSION=vX.Y.Z)"
      ;;
    *)
      fail "unknown DOCSBASE_CHANNEL '$CHANNEL' (expected: stable or prerelease)"
      ;;
  esac
  VERSION="$TAG"
fi
case "$VERSION" in
  v*) TAG="$VERSION" ;;
  *) TAG="v$VERSION" ;;
esac
VER="${TAG#v}"

ASSET="docsbase-$VER-linux-x86_64.tar.gz"
BASE_URL="https://github.com/$REPO/releases/download/$TAG"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

info "downloading $ASSET ($TAG)..."
curl -fsSL -o "$TMP/$ASSET" "$BASE_URL/$ASSET" \
  || fail "download failed: $BASE_URL/$ASSET (check the tag exists)"
curl -fsSL -o "$TMP/$ASSET.sha256" "$BASE_URL/$ASSET.sha256" \
  || fail "checksum download failed: $BASE_URL/$ASSET.sha256"

# Integrity only: the .sha256 ships in the same release as the asset, so it
# catches truncated/corrupt downloads, not a compromised release.
info "verifying sha256..."
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$TMP" && sha256sum -c "$ASSET.sha256") || fail "checksum mismatch for $ASSET"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$TMP" && shasum -a 256 -c "$ASSET.sha256") || fail "checksum mismatch for $ASSET"
else
  fail "sha256sum (or shasum) is required to verify the artifact"
fi

info "extracting..."
tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/docsbase" ] || fail "archive did not contain a docsbase binary"

mkdir -p "$BIN_DIR"
install -m 0755 "$TMP/docsbase" "$BIN_DIR/docsbase"
info "installed $BIN_DIR/docsbase"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) info "NOTE: $BIN_DIR is not on PATH. Add: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

if [ -x "$BIN_DIR/docsbase" ]; then
  info "running: docsbase install"
  PATH="$BIN_DIR:$PATH" docsbase install
  info "done. Verify: docsbase status"
else
  fail "installed binary is not executable: $BIN_DIR/docsbase"
fi
