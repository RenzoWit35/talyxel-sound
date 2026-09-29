#!/bin/sh
# Installs the latest Talyxel Sound release on macOS or Linux, no Rust toolchain needed:
#
#   curl -fsSL https://raw.githubusercontent.com/RenzoWit35/talyxel-sound/master/install.sh | sh
#
# TALYXEL_INSTALL_DIR  where the `talyxel` binary goes (default: ~/.local/bin)
# TALYXEL_REPO         GitHub repo to install from (default: RenzoWit35/talyxel-sound)
set -eu

REPO="${TALYXEL_REPO:-RenzoWit35/talyxel-sound}"
INSTALL_DIR="${TALYXEL_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    echo "error: $*" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

os=$(uname -s)
arch=$(uname -m)
case "$os" in
Darwin)
    # uname reports x86_64 inside Rosetta, so ask the hardware directly.
    if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = "1" ]; then
        arch=arm64
    fi
    case "$arch" in
    arm64) target=aarch64-apple-darwin ;;
    x86_64) target=x86_64-apple-darwin ;;
    *) fail "unsupported Mac architecture: $arch" ;;
    esac
    ;;
Linux)
    case "$arch" in
    x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
    *) fail "there is no prebuilt Linux binary for $arch yet; build from source (see README)" ;;
    esac
    ;;
*) fail "unsupported OS: $os (on Windows, use install.ps1)" ;;
esac

# /releases/latest redirects to /releases/tag/<tag>, which avoids the rate-limited API.
latest=$(curl -sSLI -o /dev/null -w '%{http_code} %{url_effective}' "https://github.com/$REPO/releases/latest") ||
    fail "could not reach GitHub"
case "$latest" in
"200 "*/releases/tag/v*) tag=${latest##*/} ;;
*) fail "no release found at https://github.com/$REPO/releases (none published yet, or the repository is private)" ;;
esac

asset="talyxel-sound-$tag-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading Talyxel Sound $tag for $target..."
curl -fsSL "https://github.com/$REPO/releases/download/$tag/$asset" -o "$tmp/$asset" ||
    fail "download failed: $asset"
tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/talyxel" ] || fail "$asset does not contain the talyxel binary"

# Copy next to the destination, then rename, so a running copy is replaced cleanly.
mkdir -p "$INSTALL_DIR"
cp "$tmp/talyxel" "$INSTALL_DIR/.talyxel.new"
chmod 755 "$INSTALL_DIR/.talyxel.new"
mv -f "$INSTALL_DIR/.talyxel.new" "$INSTALL_DIR/talyxel"
echo "Installed talyxel $tag to $INSTALL_DIR/talyxel"

if [ "$os" = Linux ] && ! { ldconfig -p 2>/dev/null || /sbin/ldconfig -p 2>/dev/null; } | grep -q 'libasound\.so\.2'; then
    echo "warning: could not find the ALSA sound library (libasound.so.2), which Talyxel Sound needs."
    echo "         Install it with your package manager, e.g. 'sudo apt install libasound2'."
fi

case ":$PATH:" in
*":$INSTALL_DIR:"*) echo "Run 'talyxel' to start." ;;
*)
    echo
    echo "$INSTALL_DIR is not on your PATH. Add this line to your ~/.zshrc or ~/.bashrc:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    echo "Then open a new terminal and run 'talyxel'."
    ;;
esac
