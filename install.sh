#!/bin/sh
# Bridge CLI installer
# Usage: curl -fsSL https://raw.githubusercontent.com/usebridgeai/cli/main/install.sh | sh
set -e

REPO="usebridgeai/cli"
INSTALL_DIR="$HOME/.bridge/bin"
BINARY_NAME="bridge"

# Detect OS
OS="$(uname -s)"
case "$OS" in
    Linux)  OS="unknown-linux-gnu" ;;
    Darwin) OS="apple-darwin" ;;
    *)
        echo "Error: Unsupported operating system: $OS"
        echo "Bridge supports macOS and Linux. For Windows, use install.ps1."
        exit 1
        ;;
esac

# Detect architecture
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64)   ARCH="x86_64" ;;
    arm64|aarch64)
        if [ "$OS" = "apple-darwin" ]; then
            ARCH="aarch64"
        else
            echo "Error: Linux arm64 is not yet supported. See https://github.com/$REPO/issues"
            exit 1
        fi
        ;;
    *)
        echo "Error: Unsupported architecture: $ARCH"
        exit 1
        ;;
esac

TARGET="${ARCH}-${OS}"

# Get latest release tag
echo "Fetching latest Bridge release..."
LATEST=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
if [ -z "$LATEST" ]; then
    echo "Error: Could not determine latest release. Check https://github.com/$REPO/releases"
    exit 1
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/download/$LATEST/bridge-${TARGET}.tar.gz"
CHECKSUMS_URL="https://github.com/$REPO/releases/download/$LATEST/checksums.txt"

# Download binary and checksums
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading Bridge $LATEST for ${TARGET}..."
curl -fsSL "$DOWNLOAD_URL" -o "$TMPDIR/bridge.tar.gz" || {
    echo "Error: Download failed. Check that a release exists for your platform: $TARGET"
    exit 1
}

# Verify checksum (fail hard — never install an unverified binary)
echo "Verifying checksum..."
curl -fsSL "$CHECKSUMS_URL" -o "$TMPDIR/checksums.txt" || {
    echo "Error: Could not download checksums file. Aborting for security."
    echo "  URL: $CHECKSUMS_URL"
    echo "  To skip verification: download the binary manually from GitHub Releases."
    exit 1
}

EXPECTED=$(grep "bridge-${TARGET}.tar.gz" "$TMPDIR/checksums.txt" | awk '{print $1}')
if [ -z "$EXPECTED" ]; then
    echo "Error: No checksum found for bridge-${TARGET}.tar.gz in checksums.txt. Aborting."
    exit 1
fi

# Detect available hash utility
if command -v shasum >/dev/null 2>&1; then
    ACTUAL=$(shasum -a 256 "$TMPDIR/bridge.tar.gz" | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
    ACTUAL=$(sha256sum "$TMPDIR/bridge.tar.gz" | awk '{print $1}')
else
    echo "Error: Neither shasum nor sha256sum found. Cannot verify download integrity."
    echo "  Install coreutils or manually verify the checksum."
    exit 1
fi

if [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "Error: Checksum mismatch! The download may be corrupted or tampered with."
    echo "  Expected: $EXPECTED"
    echo "  Got:      $ACTUAL"
    exit 1
fi
echo "Checksum verified."

# Extract and install
echo "Installing to $INSTALL_DIR..."
mkdir -p "$INSTALL_DIR"
tar xzf "$TMPDIR/bridge.tar.gz" -C "$TMPDIR"
mv "$TMPDIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
chmod +x "$INSTALL_DIR/$BINARY_NAME"

# Add to PATH (idempotent)
add_to_path() {
    local shell_profile="$1"
    if [ -f "$shell_profile" ]; then
        if ! grep -q "$INSTALL_DIR" "$shell_profile" 2>/dev/null; then
            echo "" >> "$shell_profile"
            echo "# Bridge CLI" >> "$shell_profile"
            echo "export PATH=\"$INSTALL_DIR:\$PATH\"" >> "$shell_profile"
            echo "Added $INSTALL_DIR to PATH in $shell_profile"
        fi
    fi
}

case "$(basename "$SHELL")" in
    zsh)
        # macOS defaults to .zprofile for login shells
        if [ "$(uname -s)" = "Darwin" ] && [ -f "$HOME/.zprofile" ]; then
            add_to_path "$HOME/.zprofile"
        fi
        add_to_path "$HOME/.zshrc"
        ;;
    bash)
        if [ "$(uname -s)" = "Darwin" ]; then
            add_to_path "$HOME/.bash_profile"
        else
            add_to_path "$HOME/.bashrc"
        fi
        ;;
    fish)
        FISH_CONFIG="$HOME/.config/fish/config.fish"
        if [ -f "$FISH_CONFIG" ] && ! grep -q "$INSTALL_DIR" "$FISH_CONFIG" 2>/dev/null; then
            echo "" >> "$FISH_CONFIG"
            echo "# Bridge CLI" >> "$FISH_CONFIG"
            echo "fish_add_path $INSTALL_DIR" >> "$FISH_CONFIG"
            echo "Added $INSTALL_DIR to PATH in $FISH_CONFIG"
        fi
        ;;
    *)
        echo ""
        echo "Note: Could not detect your shell. Add this to your shell profile manually:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

# Install shell completions
install_completions() {
    local bridge="$INSTALL_DIR/$BINARY_NAME"
    case "$(basename "$SHELL")" in
        zsh)
            local comp_dir="$HOME/.zfunc"
            mkdir -p "$comp_dir"
            "$bridge" completions zsh > "$comp_dir/_bridge" 2>/dev/null
            # Ensure fpath includes .zfunc
            if [ -f "$HOME/.zshrc" ] && ! grep -q '.zfunc' "$HOME/.zshrc" 2>/dev/null; then
                echo 'fpath=(~/.zfunc $fpath)' >> "$HOME/.zshrc"
            fi
            ;;
        bash)
            local comp_dir="$HOME/.local/share/bash-completion/completions"
            mkdir -p "$comp_dir"
            "$bridge" completions bash > "$comp_dir/bridge" 2>/dev/null
            ;;
        fish)
            local comp_dir="$HOME/.config/fish/completions"
            mkdir -p "$comp_dir"
            "$bridge" completions fish > "$comp_dir/bridge.fish" 2>/dev/null
            ;;
    esac
}
install_completions 2>/dev/null || true

echo ""
echo "Bridge $LATEST installed successfully!"
echo ""
echo "  Location: $INSTALL_DIR/$BINARY_NAME"
echo ""
echo "  Get started:"
echo "    bridge init"
echo "    bridge connect file://./data --as files"
echo "    bridge ls --from files"
echo ""
echo "  Restart your shell or run:"
echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
echo ""
echo "  Uninstall:"
echo "    rm -rf $INSTALL_DIR"
