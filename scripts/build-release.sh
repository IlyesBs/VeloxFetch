#!/usr/bin/env bash
# VeloxFetch: build the full release for this system and run the security gate.
#
#   Windows (Git Bash), macOS, Linux:   bash scripts/build-release.sh [options]
#
#   --skip-install   do not run `npm ci` (the node_modules folders are already there)
#   --skip-gate      build and package only, no security gate
#   --quick-gate     run the gate's quick mode (lint, policy, audits) instead of the full one
#
# Produces in release/:
#   Windows  VeloxFetch_<v>_x64-setup.exe, VeloxFetch-portable.zip
#   macOS    VeloxFetch_<v>_<arch>.dmg, VeloxFetch-portable-macos-<arch>.zip (the .app)
#   Linux    .deb, .rpm, .AppImage, VeloxFetch-portable-linux-x64.zip
#   all      VeloxFetch-extension-{chromium,chrome-store,firefox}.zip, SHA256SUMS.txt,
#            sbom/*.cdx.json and security-gate-report.json (from the gate)
#
# Sets itself up when something is missing:
#   Node.js 20+ / npm   found in the usual places (nvm, volta, fnm, Homebrew, ~/.local/node), else the official
#                       build is downloaded into ~/.local/node (checksum verified, no admin rights)
#   Rust (cargo)        installed with rustup into ~/.cargo
#   Linux libraries     installed with the system's package manager (apt, dnf, pacman, zypper; asks for the
#                       admin password), or taken from a user-space sysroot (~/.local/sysroot/env.sh) if present
#   macOS               the Xcode command line tools are requested when missing
#   gate tools          cargo-audit, cargo-deny, cargo-cyclonedx (cargo install --locked)
#   --no-auto-setup     only report what is missing
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

install=1
gate=full
auto=1
for a in "$@"; do
  case "$a" in
    --skip-install) install=0 ;;
    --skip-gate) gate=none ;;
    --quick-gate) gate=quick ;;
    --no-auto-setup) auto=0 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown option: $a (see --help)" >&2; exit 2 ;;
  esac
done

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) os=windows; conf=tauri.release.conf.json ;;
  Darwin) os=macos; conf=tauri.release.macos.conf.json ;;
  Linux) os=linux; conf=tauri.release.linux.conf.json ;;
  *) echo "unsupported system: $(uname -s)" >&2; exit 1 ;;
esac

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
note() { printf '    %s\n' "$*"; }
die() { echo "ERROR: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
prepend_path() { case ":$PATH:" in *":$1:"*) ;; *) if [ -d "$1" ]; then PATH="$1:$PATH"; fi ;; esac; }
fetch() { # fetch URL FILE
  if have curl; then curl -fsSL --retry 3 -o "$2" "$1"; elif have wget; then wget -q -O "$2" "$1"; else die "need curl or wget to download $1"; fi
}
sha256() { if have sha256sum; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
# Run as admin (asks for the password once); already root: run directly.
as_root() { if [ "$(id -u)" = 0 ]; then "$@"; elif have sudo; then sudo "$@"; else die "need admin rights (sudo) to run: $*"; fi; }

# A broken shell profile can drop the standard folders; tools installed per user live outside them.
for d in /usr/bin /bin /usr/local/bin; do case ":$PATH:" in *":$d:"*) ;; *) PATH="$PATH:$d" ;; esac; done
if [ "$os" = macos ]; then prepend_path /usr/local/bin; prepend_path /opt/homebrew/bin; fi
prepend_path "$HOME/.cargo/bin"
prepend_path "$HOME/.volta/bin"
prepend_path "$HOME/.local/share/fnm/aliases/default/bin"
prepend_path "$HOME/.local/node/bin"
if [ -s "${NVM_DIR:-$HOME/.nvm}/nvm.sh" ] && ! have npm; then
  # shellcheck disable=SC1091
  . "${NVM_DIR:-$HOME/.nvm}/nvm.sh" >/dev/null 2>&1 && nvm use --silent default >/dev/null 2>&1 || true
fi
export PATH

node_ok() { have node && have npm && node -e 'process.exit(+process.versions.node.split(".")[0] >= 20 ? 0 : 1)' 2>/dev/null; }

# Official Node.js LTS build into ~/.local/node (Linux, macOS), checksum verified.
install_node() {
  local plat arch tmp file want
  case "$os" in linux) plat=linux ;; macos) plat=darwin ;; *) die "install Node.js 20+ from https://nodejs.org, then run again" ;; esac
  case "$(uname -m)" in x86_64|amd64) arch=x64 ;; aarch64|arm64) arch=arm64 ;; *) die "no Node.js build for $(uname -m): install Node.js 20+ yourself" ;; esac
  tmp="$(mktemp -d)"
  fetch "https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt" "$tmp/SHASUMS256.txt"
  file="$(grep -o "node-v[0-9.]*-$plat-$arch\.tar\.gz" "$tmp/SHASUMS256.txt" | head -1)"
  [ -n "$file" ] || die "could not find the Node.js download for $plat-$arch"
  note "downloading $file"
  fetch "https://nodejs.org/dist/latest-v22.x/$file" "$tmp/$file"
  want="$(grep " $file\$" "$tmp/SHASUMS256.txt" | cut -d' ' -f1)"
  [ "$(sha256 "$tmp/$file")" = "$want" ] || die "the Node.js download is damaged (checksum mismatch)"
  rm -rf "$HOME/.local/node.new" && mkdir -p "$HOME/.local/node.new"
  tar -xzf "$tmp/$file" -C "$HOME/.local/node.new" --strip-components=1
  rm -rf "$HOME/.local/node" && mv "$HOME/.local/node.new" "$HOME/.local/node"
  rm -rf "$tmp"
  prepend_path "$HOME/.local/node/bin"; export PATH
}

install_rust() {
  [ "$os" = windows ] && die "install Rust from https://rustup.rs, then run again"
  local tmp; tmp="$(mktemp -d)"
  fetch "https://sh.rustup.rs" "$tmp/rustup-init.sh"
  sh "$tmp/rustup-init.sh" -y --profile minimal --component clippy,rustfmt --no-modify-path
  rm -rf "$tmp"
  prepend_path "$HOME/.cargo/bin"; export PATH
}

# What Tauri needs on Linux, as pkg-config modules.
LINUX_MODULES="webkit2gtk-4.1 gtk+-3.0 librsvg-2.0 libsoup-3.0 javascriptcoregtk-4.1"
linux_missing() {
  local m out=""
  have pkg-config || { echo "pkg-config"; return; }
  for m in $LINUX_MODULES; do pkg-config --exists "$m" 2>/dev/null || out="$out $m"; done
  pkg-config --exists ayatana-appindicator3-0.1 2>/dev/null || pkg-config --exists appindicator3-0.1 2>/dev/null || out="$out appindicator"
  have cc || have gcc || out="$out c-compiler"
  echo "$out"
}

install_linux_libs() {
  if have apt-get; then
    as_root apt-get update
    as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y build-essential pkg-config curl wget file patchelf \
      libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libxdo-dev libssl-dev
    as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y libayatana-appindicator3-dev ||
      as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y libappindicator3-dev
  elif have dnf; then
    as_root dnf install -y gcc gcc-c++ make glibc-static pkgconf-pkg-config curl wget file patchelf openssl-devel libxdo-devel \
      'pkgconfig(webkit2gtk-4.1)' 'pkgconfig(gtk+-3.0)' 'pkgconfig(librsvg-2.0)' 'pkgconfig(libsoup-3.0)' 'pkgconfig(javascriptcoregtk-4.1)'
    as_root dnf install -y 'pkgconfig(ayatana-appindicator3-0.1)' || as_root dnf install -y libappindicator-gtk3-devel
  elif have pacman; then
    as_root pacman -S --needed --noconfirm base-devel pkgconf curl wget file patchelf openssl xdotool \
      webkit2gtk-4.1 gtk3 librsvg libsoup3 libappindicator-gtk3
  elif have zypper; then
    as_root zypper --non-interactive install -t pattern devel_basis
    as_root zypper --non-interactive install glibc-devel-static pkg-config curl wget file patchelf libopenssl-devel \
      'pkgconfig(webkit2gtk-4.1)' 'pkgconfig(gtk+-3.0)' 'pkgconfig(librsvg-2.0)' 'pkgconfig(libsoup-3.0)' 'pkgconfig(javascriptcoregtk-4.1)'
    as_root zypper --non-interactive install 'pkgconfig(ayatana-appindicator3-0.1)' || as_root zypper --non-interactive install libappindicator3-devel
  else
    return 1
  fi
}

step "Checking the build tools"
if ! node_ok; then
  [ "$auto" = 1 ] || die "Node.js 20+ with npm is required (https://nodejs.org)"
  note "Node.js 20+ with npm not found: installing it into ~/.local/node"
  install_node
  node_ok || die "Node.js could not be set up: install Node.js 20+ from https://nodejs.org"
fi
if ! have cargo; then
  [ "$auto" = 1 ] || die "Rust is required (https://rustup.rs)"
  note "Rust not found: installing it with rustup"
  install_rust
  have cargo || die "Rust could not be set up: install it from https://rustup.rs"
fi

case "$os" in
  linux)
    missing="$(linux_missing)"
    if [ -n "$missing" ] && [ -f "$HOME/.local/sysroot/env.sh" ]; then
      note "using the libraries in ~/.local/sysroot"
      # shellcheck disable=SC1091
      . "$HOME/.local/sysroot/env.sh"
      missing="$(linux_missing)"
    fi
    if [ -n "$missing" ]; then
      [ "$auto" = 1 ] || die "missing system libraries:$missing"
      note "missing system libraries:$missing; installing them (the admin password may be asked)"
      install_linux_libs || die "unknown package manager: install the Tauri libraries (https://v2.tauri.app/start/prerequisites/)"
      missing="$(linux_missing)"
      [ -z "$missing" ] || die "still missing after the install:$missing"
    fi
    # linuxdeploy (AppImage) is itself an AppImage: run it without needing FUSE.
    export APPIMAGE_EXTRACT_AND_RUN=1
    ;;
  macos)
    if ! xcode-select -p >/dev/null 2>&1; then
      [ "$auto" = 1 ] && xcode-select --install 2>/dev/null || true
      die "the Xcode command line tools are needed: finish their install (a window was opened), then run again"
    fi
    ;;
esac

if [ "$gate" != none ]; then
  for t in audit deny cyclonedx; do
    if ! cargo "$t" --version >/dev/null 2>&1; then
      [ "$auto" = 1 ] || { note "cargo-$t missing: the gate will report it"; continue; }
      note "installing cargo-$t"
      cargo install --locked "cargo-$t"
    fi
  done
fi

started=$(date +%s)
echo "VeloxFetch release build: $os ($(uname -m)), node $(node --version), $(cargo --version)"

if [ "$install" = 1 ]; then
  step "Installing exact dependencies (npm ci)"
  npm ci --prefix apps/desktop --no-audit --no-fund
  npm ci --prefix apps/browser-extension --no-audit --no-fund
fi

step "Building the browser extension (Chromium, Chrome Web Store, Firefox)"
npm run build --prefix apps/browser-extension

step "Building the browser helper and the command-line tool (release)"
cargo build --release --locked -p velox-native-host -p velox-cli
if [ "$os" = linux ]; then
  # The helper also runs inside snap / Flatpak browsers, on their own libraries: build it self-contained.
  triple="$(rustc -vV | sed -n 's/^host: //p')"
  if RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --locked -p velox-native-host --target "$triple"; then
    cp "target/$triple/release/velox-native-host" target/release/velox-native-host
  else
    echo "WARNING: the self-contained browser helper could not be built (static C library missing);" >&2
    echo "         browsers installed as snap / Flatpak may not reach the app" >&2
  fi
fi

step "Building the app and its installer ($conf)"
# runs the UI build (tsc + vite) first, then compiles velox-desktop and bundles it
npm run --prefix apps/desktop tauri -- build --config "src-tauri/$conf"

step "Packaging release/ (installer, portable zip, extension zips, SHA256SUMS.txt)"
node scripts/package-release.mjs

step "Verifying the checksums"
if command -v sha256sum >/dev/null 2>&1; then
  (cd release && sha256sum -c SHA256SUMS.txt)
else
  (cd release && shasum -a 256 -c SHA256SUMS.txt)
fi

status=0
case "$gate" in
  full) step "Security gate (full)"; node scripts/security-gate.mjs || status=$? ;;
  quick) step "Security gate (quick)"; node scripts/security-gate.mjs --quick || status=$? ;;
  none) echo "security gate skipped (--skip-gate)" ;;
esac

elapsed=$(( $(date +%s) - started ))
step "Done in $((elapsed / 60)) min $((elapsed % 60)) s"
ls -la release
if [ "$status" != 0 ]; then
  echo "The build finished but the SECURITY GATE FAILED: read release/security-gate-report.json" >&2
  exit "$status"
fi
