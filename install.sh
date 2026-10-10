#!/usr/bin/env bash
set -euo pipefail

echo "=== Installing ThermalAcuarium ==="

echo "==> Building release binary..."
cargo build --release

mkdir -p "$HOME/.local/bin"
# Remove first so a running instance doesn't block the copy ("text file busy").
rm -f "$HOME/.local/bin/thermalacuarium"
install -Dm755 target/release/thermalacuarium "$HOME/.local/bin/thermalacuarium"
echo "==> Binary installed in ~/.local/bin/thermalacuarium"

mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/dev.ina.thermalacuarium.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=ThermalAcuarium
Comment=An aquarium that is your computer: every process is a fish
Exec=thermalacuarium
Icon=utilities-system-monitor
Categories=System;Monitor;
StartupWMClass=dev.ina.thermalacuarium
EOF

echo "=== Done! Run: thermalacuarium ==="
