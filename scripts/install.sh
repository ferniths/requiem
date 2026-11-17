#!/usr/bin/env bash
# REQUIEM installer for Linux / macOS
# Builds the release binary and installs it to ~/.local/bin/requiem
# Adds that directory to PATH if it is not already there.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

echo "REQUIEM — Linux/macOS installer"
echo "  project root: $ROOT_DIR"
echo ""

# ── 0. Prerequisites ─────────────────────────────────────────────────
if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found on PATH — install Rust from https://rustup.rs first" >&2
    exit 1
fi

# ── 1. Build release ─────────────────────────────────────────────────
echo "Building release binary…"
(cd "$ROOT_DIR" && cargo build --release)
echo "  ✓ build complete"

# ── 2. Locate the built binary ───────────────────────────────────────
TARGET="$ROOT_DIR/target/release/requiem"
if [[ ! -f "$TARGET" ]]; then
    echo "Cannot find $TARGET — did the build succeed?" >&2
    exit 1
fi

# ── 3. Install to ~/.local/bin ───────────────────────────────────────
INSTALL_DIR="$HOME/.local/bin"
mkdir -p "$INSTALL_DIR"
cp "$TARGET" "$INSTALL_DIR/requiem"
chmod +x "$INSTALL_DIR/requiem"
echo "  ✓ installed to $INSTALL_DIR/requiem"

# ── 4. Verify the installed binary actually runs ─────────────────────
VERSION_OUT="$("$INSTALL_DIR/requiem" --version)" || {
    echo "Installed binary failed '--version' — installation is broken" >&2
    exit 1
}
echo "  ✓ verified: $VERSION_OUT"

# ── 5. Add to PATH ───────────────────────────────────────────────────
SHELL_RC=""
if [[ -n "${ZSH_VERSION:-}" ]]; then
    SHELL_RC="$HOME/.zshrc"
elif [[ -n "${BASH_VERSION:-}" ]]; then
    SHELL_RC="$HOME/.bashrc"
fi

if [[ -n "$SHELL_RC" ]] && ! grep -q "$INSTALL_DIR" "$SHELL_RC" 2>/dev/null; then
    echo "" >> "$SHELL_RC"
    echo "# REQUIEM" >> "$SHELL_RC"
    echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$SHELL_RC"
    echo "  ✓ added $INSTALL_DIR to $SHELL_RC (run 'source $SHELL_RC' to activate)"
else
    echo "  ✓ $INSTALL_DIR already on PATH or shell rc not detected"
fi

echo ""
echo "Done. Run 'requiem setup' to configure your model endpoint."
