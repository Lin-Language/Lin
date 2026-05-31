#!/bin/sh
# Lin installer — https://github.com/Lin-Language/Lin
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Lin-Language/Lin/master/install.sh | sh
#
# Environment overrides:
#   LIN_LIB_DIR   where the three co-located files go   (default: /usr/local/lib/lin)
#   LIN_BIN_DIR   where the `lin` symlink is placed      (default: /usr/local/bin)
#   LIN_VERSION   release tag to install                 (default: latest)

set -eu

REPO="Lin-Language/Lin"
VERSION="${LIN_VERSION:-latest}"
LIB_DIR="${LIN_LIB_DIR:-/usr/local/lib/lin}"
BIN_DIR="${LIN_BIN_DIR:-/usr/local/bin}"

# ---- pretty output --------------------------------------------------------
if [ -t 1 ]; then
  BOLD="$(printf '\033[1m')"; RED="$(printf '\033[31m')"
  GREEN="$(printf '\033[32m')"; YELLOW="$(printf '\033[33m')"; RESET="$(printf '\033[0m')"
else
  BOLD=""; RED=""; GREEN=""; YELLOW=""; RESET=""
fi
info()  { printf '%s\n' "${BOLD}==>${RESET} $*"; }
warn()  { printf '%s\n' "${YELLOW}warning:${RESET} $*" >&2; }
error() { printf '%s\n' "${RED}error:${RESET} $*" >&2; exit 1; }

# ---- detect platform ------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64|amd64) artifact="lin-linux-x86_64" ;;
      *) error "unsupported Linux architecture '$arch' (only x86_64 has a prebuilt binary — build from source instead)" ;;
    esac ;;
  Darwin)
    case "$arch" in
      arm64|aarch64) artifact="lin-macos-arm64" ;;
      *) error "unsupported macOS architecture '$arch' (only Apple Silicon has a prebuilt binary — build from source instead)" ;;
    esac ;;
  *) error "unsupported OS '$os' (supported: Linux x86_64, macOS arm64)" ;;
esac

# ---- prerequisites --------------------------------------------------------
command -v curl >/dev/null 2>&1 || error "curl is required but not found on your \$PATH"
command -v tar  >/dev/null 2>&1 || error "tar is required but not found on your \$PATH"

# `lin build` shells out to a C linker. Warn (don't fail) if none is present.
if ! command -v cc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
  if [ "$os" = "Darwin" ]; then
    warn "no C linker found. Install Xcode Command Line Tools: xcode-select --install"
  else
    warn "no C linker found. Install one, e.g.: sudo apt-get install clang   (or gcc)"
  fi
fi

# ---- privilege escalation -------------------------------------------------
# Use sudo only for paths we cannot already write to.
SUDO=""
need_sudo_for() {
  dir="$1"
  # Walk up to the nearest existing ancestor and test writability there.
  while [ ! -d "$dir" ]; do dir="$(dirname "$dir")"; done
  [ -w "$dir" ] && return 1 || return 0
}
if need_sudo_for "$LIB_DIR" || need_sudo_for "$BIN_DIR"; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
    info "elevated permissions needed for $LIB_DIR / $BIN_DIR — you may be prompted for your password"
  else
    error "need write access to $LIB_DIR and $BIN_DIR but sudo is unavailable.
       Re-run with a writable location, e.g.:
         curl -fsSL .../install.sh | LIN_LIB_DIR=\$HOME/.local/lib/lin LIN_BIN_DIR=\$HOME/.local/bin sh"
  fi
fi

# ---- download -------------------------------------------------------------
url="https://github.com/${REPO}/releases/download/${VERSION}/${artifact}.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

info "downloading ${artifact} (${VERSION})"
if ! curl -fSL --progress-bar "$url" -o "$tmp/lin.tar.gz"; then
  error "download failed: $url
       (check the release exists and you have network access)"
fi

info "extracting"
tar -xzf "$tmp/lin.tar.gz" -C "$tmp"
for f in lin lin-lsp liblin_runtime.a; do
  [ -f "$tmp/$f" ] || error "archive is missing '$f' — it may be corrupt; try again"
done

# ---- install --------------------------------------------------------------
info "installing to $LIB_DIR"
$SUDO mkdir -p "$LIB_DIR"
$SUDO cp "$tmp/lin" "$tmp/lin-lsp" "$tmp/liblin_runtime.a" "$LIB_DIR/"
$SUDO chmod +x "$LIB_DIR/lin" "$LIB_DIR/lin-lsp"

info "linking $BIN_DIR/lin"
$SUDO mkdir -p "$BIN_DIR"
$SUDO ln -sf "$LIB_DIR/lin" "$BIN_DIR/lin"

# ---- verify ---------------------------------------------------------------
ver="$("$LIB_DIR/lin" --version 2>/dev/null || true)"
[ -n "$ver" ] || error "installed binary failed to run ($LIB_DIR/lin --version produced no output)"

printf '%s\n' "${GREEN}✓${RESET} installed ${BOLD}${ver}${RESET} → $BIN_DIR/lin"

if ! command -v lin >/dev/null 2>&1; then
  warn "$BIN_DIR is not on your \$PATH. Add it to your shell profile:"
  printf '    export PATH="%s:$PATH"\n' "$BIN_DIR"
else
  printf '  Get started:  %slin run <file.lin>%s\n' "$BOLD" "$RESET"
fi
