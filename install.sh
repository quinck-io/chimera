#!/bin/sh
set -eu

# Install chimera from GitHub releases.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/quinck-io/chimera/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/quinck-io/chimera/main/install.sh | sh -s -- v0.2.0

REPO="quinck-io/chimera"
INSTALL_DIR="/usr/local/bin"
BINARY="chimera"

main() {
    version="${1:-latest}"
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os_target="unknown-linux-gnu" ;;
        Darwin) os_target="apple-darwin" ;;
        *)      err "Unsupported OS: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch_target="x86_64" ;;
        aarch64|arm64) arch_target="aarch64" ;;
        *)             err "Unsupported architecture: $arch" ;;
    esac

    target="${arch_target}-${os_target}"

    if [ "$version" = "latest" ]; then
        download_url="https://github.com/${REPO}/releases/latest/download/chimera-${target}.tar.gz"
    else
        download_url="https://github.com/${REPO}/releases/download/${version}/chimera-${target}.tar.gz"
    fi

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    log "Downloading chimera for ${target}..."
    curl -fsSL "$download_url" -o "${tmpdir}/chimera.tar.gz"

    log "Extracting..."
    tar xzf "${tmpdir}/chimera.tar.gz" -C "$tmpdir"

    log "Installing to ${INSTALL_DIR}/${BINARY}..."
    if [ -w "$INSTALL_DIR" ]; then
        mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    else
        sudo mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi
    chmod +x "${INSTALL_DIR}/${BINARY}"

    log "Installed chimera $(${INSTALL_DIR}/${BINARY} --version 2>/dev/null || echo "${version}")"
}

log() { printf '  %s\n' "$*"; }
err() { printf 'Error: %s\n' "$*" >&2; exit 1; }

main "$@"
