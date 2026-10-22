#!/bin/sh
# AMARA installer for macOS and Linux.
#   curl -fsSL https://raw.githubusercontent.com/KalarisLabs/amara/main/install.sh | sh
# Env: AMARA_VERSION (default: latest), AMARA_INSTALL_DIR (default: ~/.local/bin)
set -eu

REPO="KalarisLabs/amara"
VERSION="${AMARA_VERSION:-latest}"
DIR="${AMARA_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '\033[35m▸\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }

need uname
if command -v curl >/dev/null 2>&1; then fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then fetch() { wget -qO "$2" "$1"; }
else die "curl or wget is required"; fi

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) die "unsupported OS $(uname -s). On Windows use install.ps1; elsewhere: cargo install --git https://github.com/$REPO amara-cli" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch=x64 ;;
  aarch64|arm64) arch=arm64 ;;
  *) die "unsupported CPU $(uname -m); build from source: cargo install --git https://github.com/$REPO amara-cli" ;;
esac

asset="amara-$os-$arch"
if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "downloading $asset ($VERSION)"
fetch "$base/$asset" "$tmp/amara" || die "download failed: $base/$asset"

if fetch "$base/$asset.sha256" "$tmp/amara.sha256" 2>/dev/null; then
  expected="$(cut -d' ' -f1 < "$tmp/amara.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then actual="$(sha256sum "$tmp/amara" | cut -d' ' -f1)"
  else actual="$(shasum -a 256 "$tmp/amara" | cut -d' ' -f1)"; fi
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset"
  say "checksum verified"
fi

mkdir -p "$DIR"
install -m 0755 "$tmp/amara" "$DIR/amara" 2>/dev/null || { cp "$tmp/amara" "$DIR/amara"; chmod 0755 "$DIR/amara"; }
say "installed $DIR/amara"

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) printf '\n  add to PATH:  export PATH="%s:$PATH"\n' "$DIR" ;;
esac

printf '\n  next:  amara doctor   ·   amara simulate   ·   amara setup claude-code --write\n\n'
