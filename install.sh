#!/usr/bin/env sh
set -eu

REPOSITORY=${FINDEX_REPOSITORY:-siddarthan007/findex}
API="https://api.github.com/repos/$REPOSITORY/releases/latest"
TMP_ROOT=${TMPDIR:-/tmp}
STAGING=$(mktemp -d "$TMP_ROOT/findex-install.XXXXXX")
trap 'rm -rf "$STAGING"' EXIT HUP INT TERM

fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL -H 'User-Agent: findex-installer' "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -q --user-agent='findex-installer' "$1" -O "$2"
  else echo "Install curl or wget first." >&2; exit 1
  fi
}

fetch "$API" "$STAGING/release.json"
os=$(uname -s)
arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch_pattern='x86_64|amd64|x64' ;;
  arm64|aarch64) arch_pattern='aarch64|arm64' ;;
  *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
esac

case "$os" in
  Darwin) asset_pattern="(${arch_pattern}).*\.dmg" ;;
  Linux)
    if command -v dpkg >/dev/null 2>&1; then asset_pattern="(${arch_pattern}).*\.deb"
    elif command -v rpm >/dev/null 2>&1; then asset_pattern="(${arch_pattern}).*\.rpm"
    else asset_pattern="(${arch_pattern}).*\.AppImage"
    fi
    ;;
  *) echo "Unsupported operating system: $os" >&2; exit 1 ;;
esac

urls=$(sed -n 's/.*"browser_download_url": *"\([^"]*\)".*/\1/p' "$STAGING/release.json")
asset_url=$(printf '%s\n' "$urls" | grep -Ei "$asset_pattern" | head -n 1 || true)
checksum_url=$(printf '%s\n' "$urls" | grep '/SHA256SUMS$' | head -n 1 || true)
[ -n "$asset_url" ] || { echo "No matching desktop installer in the latest release." >&2; exit 1; }
[ -n "$checksum_url" ] || { echo "Release has no SHA256SUMS; refusing an unverified install." >&2; exit 1; }
asset_name=${asset_url##*/}
fetch "$asset_url" "$STAGING/$asset_name"
fetch "$checksum_url" "$STAGING/SHA256SUMS"
(cd "$STAGING" && grep "  $asset_name\$" SHA256SUMS > selected.sha256)
if command -v sha256sum >/dev/null 2>&1; then (cd "$STAGING" && sha256sum -c selected.sha256)
else (cd "$STAGING" && shasum -a 256 -c selected.sha256)
fi

case "$asset_name" in
  *.deb) sudo dpkg -i "$STAGING/$asset_name" ;;
  *.rpm) sudo rpm -U "$STAGING/$asset_name" ;;
  *.AppImage)
    install_dir=${FINDEX_INSTALL_DIR:-"$HOME/.local/lib/findex"}
    bin_dir=${FINDEX_BIN_DIR:-"$HOME/.local/bin"}
    mkdir -p "$install_dir" "$bin_dir"
    install -m 755 "$STAGING/$asset_name" "$install_dir/Findex.AppImage"
    ln -sf "$install_dir/Findex.AppImage" "$bin_dir/findex-desktop"
    cli_url=$(printf '%s\n' "$urls" | grep -Ei "findex-linux-(${arch_pattern})\.zip" | head -n 1 || true)
    [ -n "$cli_url" ] || { echo "The AppImage fallback requires the matching CLI archive." >&2; exit 1; }
    cli_name=${cli_url##*/}
    fetch "$cli_url" "$STAGING/$cli_name"
    (cd "$STAGING" && grep "  $cli_name\$" SHA256SUMS > cli.sha256)
    if command -v sha256sum >/dev/null 2>&1; then (cd "$STAGING" && sha256sum -c cli.sha256)
    else (cd "$STAGING" && shasum -a 256 -c cli.sha256)
    fi
    command -v unzip >/dev/null 2>&1 || { echo "Install unzip to install the CLI/TUI." >&2; exit 1; }
    unzip -p "$STAGING/$cli_name" findex > "$bin_dir/findex"
    chmod 755 "$bin_dir/findex"
    ;;
  *.dmg)
    mount=$(hdiutil attach -nobrowse -readonly "$STAGING/$asset_name" | tail -n 1 | awk '{$1=$2=""; sub(/^  */, ""); print}')
    mkdir -p "$HOME/Applications" "$HOME/.local/bin"
    cp -R "$mount/Findex.app" "$HOME/Applications/Findex.app"
    hdiutil detach "$mount" >/dev/null
    ln -sf "$HOME/Applications/Findex.app/Contents/MacOS/findex" "$HOME/.local/bin/findex"
    ;;
esac

if [ "${FINDEX_SETUP_AGENT:-none}" != "none" ] && command -v findex >/dev/null 2>&1; then
  findex setup-agent "$FINDEX_SETUP_AGENT"
fi
echo "Findex installed and SHA-256 verified. Restart the shell if findex is not yet on PATH."
