#!/bin/sh
set -eu

REPO="carlosarraes/pyllow"
BINARY="pyllow"
INSTALL_DIR="$HOME/.local/bin"

main() {
    os=$(detect_os)
    arch=$(detect_arch)
    version=$(resolve_version)
    target="${arch}-${os}"

    echo "Installing pyllow ${version} (${target})..."

    url="https://github.com/${REPO}/releases/download/${version}/pyllow-${target}.tar.gz"
    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    echo "Downloading ${url}..."
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL "$url" -o "$tmpdir/pyllow.tar.gz"
    elif command -v wget > /dev/null 2>&1; then
        wget -qO "$tmpdir/pyllow.tar.gz" "$url"
    else
        echo "Error: curl or wget is required" >&2
        exit 1
    fi

    tar xzf "$tmpdir/pyllow.tar.gz" -C "$tmpdir"

    mkdir -p "$INSTALL_DIR"
    mv "$tmpdir/$BINARY" "$INSTALL_DIR/$BINARY"
    chmod +x "$INSTALL_DIR/$BINARY"

    echo "Installed pyllow to ${INSTALL_DIR}/pyllow"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            echo ""
            echo "NOTE: ${INSTALL_DIR} is not in your PATH."
            echo "Add it by appending this to your shell config (~/.bashrc, ~/.zshrc, etc.):"
            echo ""
            echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
            echo ""
            ;;
    esac
}

detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "unknown-linux-gnu" ;;
        Darwin*) echo "apple-darwin" ;;
        *)
            echo "Error: unsupported OS '$(uname -s)'. Download manually from:" >&2
            echo "  https://github.com/${REPO}/releases" >&2
            exit 1
            ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *)
            echo "Error: unsupported architecture '$(uname -m)'. Download manually from:" >&2
            echo "  https://github.com/${REPO}/releases" >&2
            exit 1
            ;;
    esac
}

resolve_version() {
    if [ -n "${PYLLOW_VERSION:-}" ]; then
        echo "$PYLLOW_VERSION"
        return
    fi

    if command -v curl > /dev/null 2>&1; then
        curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" |
            sed 's|.*/||'
    elif command -v wget > /dev/null 2>&1; then
        wget --spider -S "https://github.com/${REPO}/releases/latest" 2>&1 |
            grep -i 'Location:' | sed 's|.*/||' | tr -d '\r'
    else
        echo "Error: curl or wget is required" >&2
        exit 1
    fi
}

main
