#!/usr/bin/env bash
# ThermalAcuarium - lanzador portátil. No hace falta instalar nada: ejecuta la app desde esta
# carpeta y, la primera vez, consigue el binario (descarga de la release con sha256, o compila).
#
#   ./thermalacuarium.sh [opciones de la app]      p. ej. ./thermalacuarium.sh --style real
#
# Orden de búsqueda: bin/ (descargado, de la versión de este checkout) → target/release/ (compilado
# aquí) → `thermalacuarium` en el PATH → descargar → compilar con cargo.
# THERMALACUARIUM_BUILD=1 fuerza compilar desde el código en vez de descargar.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO="IAnMove/ThermalAcuarium"
ASSET="thermalacuarium-x86_64-linux"
BIN_DIR="$HERE/bin"
BIN="$BIN_DIR/thermalacuarium"
VERSION=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$HERE/manifest.json" 2>/dev/null | head -1)

say() {
    echo "ThermalAcuarium: $1" >&2
    command -v notify-send >/dev/null 2>&1 && notify-send -a ThermalAcuarium "ThermalAcuarium" "$1" >/dev/null 2>&1 || true
}

# ¿Hay un binario portátil de la versión de este checkout? (si no hay marca de versión, se respeta
# el que haya puesto el usuario).
portable_ok() {
    [ -x "$BIN" ] || return 1
    [ ! -f "$BIN_DIR/.version" ] || [ "$(cat "$BIN_DIR/.version")" = "$VERSION" ]
}

download() {
    command -v curl >/dev/null 2>&1 || return 1
    [ "$(uname -m)" = "x86_64" ] || return 1
    [ -n "$VERSION" ] || return 1
    local base="https://github.com/$REPO/releases/download/v$VERSION" tmp
    tmp=$(mktemp -d)
    say "descargando ThermalAcuarium v$VERSION (solo la primera vez)…"
    if ! curl -fsSL --retry 2 -o "$tmp/$ASSET" "$base/$ASSET" || ! curl -fsSL --retry 2 -o "$tmp/$ASSET.sha256" "$base/$ASSET.sha256"; then
        rm -rf "$tmp"
        return 1
    fi
    if ! (cd "$tmp" && sha256sum -c "$ASSET.sha256" >/dev/null 2>&1); then
        say "la suma sha256 no coincide: descarga descartada"
        rm -rf "$tmp"
        return 1
    fi
    mkdir -p "$BIN_DIR"
    install -m755 "$tmp/$ASSET" "$BIN.new" && mv -f "$BIN.new" "$BIN"
    echo "$VERSION" >"$BIN_DIR/.version"
    rm -rf "$tmp"
}

build() {
    command -v cargo >/dev/null 2>&1 && [ -f "$HERE/Cargo.toml" ] || return 1
    say "compilando ThermalAcuarium con cargo (puede tardar un par de minutos)…"
    cargo build --release --locked --manifest-path "$HERE/Cargo.toml" >&2
}

run=""
if portable_ok; then
    run="$BIN"
elif [ -x "$HERE/target/release/thermalacuarium" ]; then
    run="$HERE/target/release/thermalacuarium"
elif command -v thermalacuarium >/dev/null 2>&1; then
    run=$(command -v thermalacuarium)
else
    # Un solo proceso a la vez consigue el binario: si se hace clic dos veces, el segundo espera.
    exec 9>"$HERE/.fetch.lock"
    flock 9 2>/dev/null || true
    if portable_ok; then
        run="$BIN"
    elif [ "${THERMALACUARIUM_BUILD:-0}" != "1" ] && download; then
        run="$BIN"
    elif build; then
        run="$HERE/target/release/thermalacuarium"
    else
        say "no se pudo conseguir el binario. Instala GTK 4 y libadwaita y vuelve a intentarlo, o ejecuta: cargo build --release"
        exit 1
    fi
    exec 9>&-
fi

exec "$run" "$@"
