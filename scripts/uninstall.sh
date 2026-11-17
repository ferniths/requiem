#!/usr/bin/env bash
# REQUIEM uninstaller for Linux / macOS
# Removes the installed binary from ~/.local/bin and optionally purges
# the config directory (~/.requiem).

set -euo pipefail

INSTALL_DIR="$HOME/.local/bin"

echo "REQUIEM — Linux/macOS uninstaller"

if [[ -f "$INSTALL_DIR/requiem" ]]; then
    rm -f "$INSTALL_DIR/requiem"
    echo "  ✓ removed $INSTALL_DIR/requiem"
else
    echo "  · $INSTALL_DIR/requiem not found — nothing to remove"
fi

# Ask whether to also purge the config directory.
echo ""
read -r -p "Also remove your configuration and sessions (~/.requiem)? [Y/n] " ans
ans="${ans:-Y}"
if [[ "$ans" =~ ^[yY] ]]; then
    if [[ -d "$HOME/.requiem" ]]; then
        rm -rf "$HOME/.requiem"
        echo "  ✓ removed $HOME/.requiem"
    else
        echo "  · $HOME/.requiem not found"
    fi
else
    echo "  · config kept"
fi

echo ""
echo "Done. (The PATH entry added by install.sh is left intact.)"
