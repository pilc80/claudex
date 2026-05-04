#!/bin/sh
# Claudex installer for macOS and Linux.
# Windows users should use install.ps1 from PowerShell.
set -eu

REPO="${CLAUDEX_REPO:-pilc80/claudex}"
INSTALL_DIR="${CLAUDEX_INSTALL_DIR:-$HOME/.local/bin}"
EXPLICIT_INSTALL_DIR="${CLAUDEX_INSTALL_DIR:+1}"
PROFILE_NAME="${CLAUDEX_PROFILE:-codex-sub}"
INSTALLED_BIN=""
INSTALLED_CONFIG_BIN=""
EXPECTED_VERSION=""
ASSUME_YES="${CLAUDEX_ASSUME_YES:-}"
SKIP_SETUP="${CLAUDEX_SKIP_SETUP:-}"
DRY_RUN="${CLAUDEX_DRY_RUN:-}"
ALLOW_SOURCE_FALLBACK="${CLAUDEX_SOURCE_FALLBACK:-1}"

say() {
    printf '%s\n' "$*"
}

err() {
    printf 'Error: %s\n' "$*" >&2
}

has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

resolve_command_path() {
    command -v "$1" 2>/dev/null || true
}

select_install_dir() {
    if [ -n "$EXPLICIT_INSTALL_DIR" ]; then
        return
    fi

    existing_config="$(resolve_command_path claudex-config)"
    if [ -n "$existing_config" ]; then
        INSTALL_DIR="$(dirname "$existing_config")"
        say "Replacing existing claudex-config found in PATH: $existing_config"
        return
    fi

    existing_claudex="$(resolve_command_path claudex)"
    if [ -n "$existing_claudex" ]; then
        INSTALL_DIR="$(dirname "$existing_claudex")"
        say "Replacing existing claudex found in PATH: $existing_claudex"
    fi
}

is_yes() {
    case "${1:-}" in
        y|Y|yes|YES|Yes|true|TRUE|1) return 0 ;;
        *) return 1 ;;
    esac
}

prompt_yes_no() {
    prompt="$1"
    default="${2:-n}"

    if is_yes "$ASSUME_YES"; then
        return 0
    fi

    if [ ! -t 0 ]; then
        [ "$default" = "y" ]
        return
    fi

    if [ "$default" = "y" ]; then
        suffix="[Y/n]"
    else
        suffix="[y/N]"
    fi

    printf '%s %s ' "$prompt" "$suffix"
    read -r answer || answer=""
    if [ -z "$answer" ]; then
        answer="$default"
    fi
    is_yes "$answer"
}

usage() {
    cat <<'EOF'
Usage: install.sh [options]

Options:
  --install-dir DIR      Install directory (default: ~/.local/bin)
  --repo OWNER/REPO      GitHub repository (default: pilc80/claudex)
  --profile NAME        Setup profile name (default: codex-sub)
  --yes                 Accept installer prompts
  --no-setup            Skip ChatGPT/Codex setup prompts
  --no-source-fallback  Do not fall back to cargo install
  --dry-run             Print actions without installing
  -h, --help            Show this help
EOF
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --install-dir)
                [ "$#" -ge 2 ] || { err "--install-dir requires a value"; exit 2; }
                INSTALL_DIR="$2"
                EXPLICIT_INSTALL_DIR=1
                shift 2
                ;;
            --repo)
                [ "$#" -ge 2 ] || { err "--repo requires a value"; exit 2; }
                REPO="$2"
                shift 2
                ;;
            --profile)
                [ "$#" -ge 2 ] || { err "--profile requires a value"; exit 2; }
                PROFILE_NAME="$2"
                shift 2
                ;;
            --yes)
                ASSUME_YES=1
                shift
                ;;
            --no-setup)
                SKIP_SETUP=1
                shift
                ;;
            --no-source-fallback)
                ALLOW_SOURCE_FALLBACK=0
                shift
                ;;
            --dry-run)
                DRY_RUN=1
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                err "unknown option: $1"
                usage >&2
                exit 2
                ;;
        esac
    done
}

download_file() {
    url="$1"
    output="$2"
    mode="${3:-quiet}"

    if has_cmd curl; then
        if [ "$mode" = "progress" ]; then
            curl --fail --location --show-error \
                --connect-timeout 20 --retry 3 --retry-delay 2 \
                -H "User-Agent: claudex-installer" "$url" -o "$output"
        else
            curl --fail --location --silent --show-error \
                --connect-timeout 20 --retry 3 --retry-delay 2 \
                -H "User-Agent: claudex-installer" "$url" -o "$output"
        fi
    elif has_cmd wget; then
        if [ "$mode" = "progress" ]; then
            wget --tries=3 --timeout=20 -O "$output" "$url"
        else
            wget --quiet --tries=3 --timeout=20 -O "$output" "$url"
        fi
    else
        err "curl or wget is required"
        return 1
    fi
}

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            libc="gnu"
            if has_cmd ldd; then
                case "$(ldd --version 2>&1 || true)" in
                    *musl*) libc="musl" ;;
                esac
            elif [ -f /etc/alpine-release ]; then
                libc="musl"
            fi

            case "$arch" in
                x86_64|amd64) echo "x86_64-unknown-linux-${libc}" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-${libc}" ;;
                *) err "unsupported Linux architecture: $arch"; exit 1 ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                *) err "unsupported macOS architecture: $arch"; exit 1 ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            err "use install.ps1 from Windows PowerShell"
            exit 1
            ;;
        *)
            err "unsupported OS: $os"
            exit 1
            ;;
    esac
}

check_deps() {
    missing=""
    for cmd in uname tar; do
        if ! has_cmd "$cmd"; then
            missing="$missing $cmd"
        fi
    done
    if ! has_cmd curl && ! has_cmd wget; then
        missing="$missing curl-or-wget"
    fi
    if [ -n "$missing" ]; then
        err "missing required tools:$missing"
        exit 1
    fi

    if ! has_cmd claude; then
        say "Warning: Claude Code was not found in PATH."
    fi
}

sha256_file() {
    file="$1"
    if has_cmd sha256sum; then
        sha256sum "$file" | awk '{print $1}'
    elif has_cmd shasum; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif has_cmd openssl; then
        openssl dgst -sha256 "$file" | awk '{print $NF}'
    else
        err "sha256sum, shasum, or openssl is required to verify release archives"
        return 1
    fi
}

verify_checksum() {
    file="$1"
    checksum_file="$2"

    expected="$(awk '{print $1}' "$checksum_file")"
    actual="$(sha256_file "$file")"

    if [ "$expected" != "$actual" ]; then
        err "checksum mismatch for $(basename "$file")"
        err "expected: $expected"
        err "actual:   $actual"
        return 1
    fi
    say "Verified SHA256: $actual"
}

write_expected_checksum() {
    expected="$1"
    name="$2"
    output="$3"
    printf '%s  %s\n' "$expected" "$name" > "$output"
}

get_latest_version() {
    tmp="${TMPDIR:-/tmp}/claudex-release-$$.json"
    if download_file "https://api.github.com/repos/$REPO/releases/latest" "$tmp" >/dev/null; then
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmp" | head -n 1
        rm -f "$tmp"
        return 0
    fi
    rm -f "$tmp"
    return 1
}

parse_manifest_version() {
    manifest="$1"
    sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$manifest" | head -n 1
}

parse_manifest_for_target() {
    manifest="$1"
    target="$2"
    awk -v target="$target" '
        $0 ~ "\"target\": \"" target "\"" { found = 1; next }
        found && /"name":/ {
            name = $0
            sub(/^.*"name": "/, "", name)
            sub(/".*$/, "", name)
            next
        }
        found && /"sha256":/ {
            sha = $0
            sub(/^.*"sha256": "/, "", sha)
            sub(/".*$/, "", sha)
            next
        }
        found && /"url":/ {
            url = $0
            sub(/^.*"url": "/, "", url)
            sub(/".*$/, "", url)
            print name
            print sha
            print url
            exit
        }
    ' "$manifest"
}

backup_existing() {
    backup_path="$1"
    if [ -e "$backup_path" ] || [ -L "$backup_path" ]; then
        backup="${backup_path}.backup.$(date +%Y%m%d%H%M%S)"
        cp -p "$backup_path" "$backup" 2>/dev/null || true
        say "Backed up existing binary to $backup"
    fi
}

install_binary() {
    src="$1"
    binary_dest="$INSTALL_DIR/claudex"
    config_dest="$INSTALL_DIR/claudex-config"

    mkdir -p "$INSTALL_DIR"
    if is_yes "$DRY_RUN"; then
        say "Dry run: would install $src to $binary_dest"
        say "Dry run: would install claudex-config to $config_dest"
        INSTALLED_BIN="$binary_dest"
        INSTALLED_CONFIG_BIN="$config_dest"
        return 0
    fi
    backup_existing "$binary_dest"
    backup_existing "$config_dest"
    rm -f "$binary_dest"
    mv "$src" "$binary_dest"
    chmod +x "$binary_dest"
    rm -f "$config_dest"
    ln -s "claudex" "$config_dest"
    INSTALLED_BIN="$binary_dest"
    INSTALLED_CONFIG_BIN="$config_dest"
}

install_from_release() {
    target="$(detect_target)"
    say "Detected target: $target"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT
    manifest_url="https://github.com/$REPO/releases/latest/download/claudex-release-manifest.json"
    archive_name=""
    expected_sha=""
    url=""
    checksum_url=""

    say "Downloading release manifest: $manifest_url"
    if download_file "$manifest_url" "$tmpdir/manifest.json" >/dev/null 2>&1; then
        say "Downloaded release manifest"
        EXPECTED_VERSION="$(parse_manifest_version "$tmpdir/manifest.json")"
        if [ -z "$EXPECTED_VERSION" ]; then
            err "release manifest does not contain a version"
            return 1
        fi
        artifact_info="$(parse_manifest_for_target "$tmpdir/manifest.json" "$target")"
        archive_name="$(printf '%s\n' "$artifact_info" | sed -n '1p')"
        expected_sha="$(printf '%s\n' "$artifact_info" | sed -n '2p')"
        url="$(printf '%s\n' "$artifact_info" | sed -n '3p')"
        if [ -z "$archive_name" ] || [ -z "$expected_sha" ] || [ -z "$url" ]; then
            err "release manifest does not contain target $target"
            return 1
        fi
        say "Using release manifest: $manifest_url"
    else
        if is_yes "$DRY_RUN"; then
            say "Dry run: release manifest was not available; would query GitHub Releases API as fallback"
            say "Dry run: would download, verify, unpack, and install to $INSTALL_DIR/claudex"
            say "Dry run: would install claudex-config to $INSTALL_DIR/claudex-config"
            INSTALLED_BIN="$INSTALL_DIR/claudex"
            INSTALLED_CONFIG_BIN="$INSTALL_DIR/claudex-config"
            return 0
        fi
        version="$(get_latest_version)"
        EXPECTED_VERSION="$version"
        if [ -z "$version" ]; then
            err "failed to determine latest release"
            return 1
        fi
        say "Latest release: $version"
        archive_name="claudex-${version}-${target}.tar.gz"
        url="https://github.com/$REPO/releases/download/$version/$archive_name"
        checksum_url="${url}.sha256"
    fi

    if [ -n "$expected_sha" ]; then
        say "Expected SHA256: $expected_sha"
    else
        say "Checksum:   $checksum_url"
    fi

    if is_yes "$DRY_RUN"; then
        say "Dry run: would download, verify, unpack, and install to $INSTALL_DIR/claudex"
        say "Dry run: would install claudex-config to $INSTALL_DIR/claudex-config"
        INSTALLED_BIN="$INSTALL_DIR/claudex"
        INSTALLED_CONFIG_BIN="$INSTALL_DIR/claudex-config"
        return 0
    fi

    archive="$tmpdir/$archive_name"
    checksum_file="$tmpdir/$archive_name.sha256"
    say "Downloading release archive: $url"
    download_file "$url" "$archive" progress
    say "Downloaded release archive"
    if [ -n "$expected_sha" ]; then
        write_expected_checksum "$expected_sha" "$archive_name" "$checksum_file"
    else
        say "Downloading checksum: $checksum_url"
        download_file "$checksum_url" "$checksum_file"
        say "Downloaded checksum"
    fi
    verify_checksum "$archive" "$checksum_file"
    tar xzf "$archive" -C "$tmpdir"
    install_binary "$tmpdir/claudex"
}

install_from_source() {
    if ! has_cmd cargo || ! has_cmd git; then
        err "cargo and git are required for source install fallback"
        return 1
    fi
    say "Installing from source with cargo..."
    if is_yes "$DRY_RUN"; then
        say "Dry run: would run cargo install --git https://github.com/$REPO --force"
        INSTALLED_BIN="$HOME/.cargo/bin/claudex"
        INSTALLED_CONFIG_BIN="$HOME/.cargo/bin/claudex-config"
        return 0
    fi
    cargo install --git "https://github.com/$REPO" --force
    INSTALLED_BIN="$HOME/.cargo/bin/claudex"
    INSTALLED_CONFIG_BIN="$HOME/.cargo/bin/claudex-config"
}

ensure_path_notice() {
    dir="$(dirname "$INSTALLED_BIN")"
    case ":$PATH:" in
        *":$dir:"*) ;;
        *)
            say ""
            say "Add this directory to PATH:"
            say "  export PATH=\"$dir:\$PATH\""
            ;;
    esac
}

verify_installed_latest() {
    if [ -z "$EXPECTED_VERSION" ] || [ -z "$INSTALLED_CONFIG_BIN" ] || [ ! -x "$INSTALLED_CONFIG_BIN" ]; then
        return
    fi

    installed_version="$($INSTALLED_CONFIG_BIN --version 2>/dev/null | awk '{print $NF}' | head -n 1 || true)"
    expected="${EXPECTED_VERSION#v}"
    if [ "$installed_version" != "$expected" ]; then
        err "installed claudex-config version is $installed_version, expected $expected"
        exit 1
    fi

    path_config="$(resolve_command_path claudex-config)"
    if [ -n "$path_config" ] && [ "$path_config" != "$INSTALLED_CONFIG_BIN" ]; then
        path_version="$($path_config --version 2>/dev/null | awk '{print $NF}' | head -n 1 || true)"
        if [ "$path_version" != "$expected" ]; then
            err "PATH resolves claudex-config to $path_config ($path_version), not latest $expected"
            err "Move $INSTALLED_CONFIG_BIN earlier in PATH or reinstall with --install-dir $(dirname "$path_config")"
            exit 1
        fi
    fi

    say "Installed latest claudex $expected"
}

maybe_stop_proxy() {
    if [ -z "$INSTALLED_BIN" ] || [ ! -x "$INSTALLED_BIN" ]; then
        return
    fi

    if [ -n "$INSTALLED_CONFIG_BIN" ] && [ -x "$INSTALLED_CONFIG_BIN" ]; then
        status="$("$INSTALLED_CONFIG_BIN" proxy status 2>/dev/null || true)"
    else
        status="$("$INSTALLED_BIN" proxy status 2>/dev/null || true)"
    fi
    case "$status" in
        "Proxy is running"*)
            say "$status"
            if prompt_yes_no "Stop the running claudex proxy so the new binary is used?" n; then
                if [ -n "$INSTALLED_CONFIG_BIN" ] && [ -x "$INSTALLED_CONFIG_BIN" ]; then
                    "$INSTALLED_CONFIG_BIN" proxy stop || true
                else
                    "$INSTALLED_BIN" proxy stop || true
                fi
            else
                say ""
                say "Action needed:"
                say "The old proxy is still running and may keep using the previous binary."
                say "Restart it when convenient to use the newly installed version:"
                say "  claudex-config proxy stop"
                say "  claudex"
            fi
            ;;
    esac
}

maybe_setup_chatgpt() {
    if is_yes "$SKIP_SETUP" || is_yes "$DRY_RUN"; then
        return
    fi
    if [ -z "$INSTALLED_CONFIG_BIN" ] || [ ! -x "$INSTALLED_CONFIG_BIN" ]; then
        return
    fi
    if ! prompt_yes_no "Set up a ChatGPT/Codex OAuth profile now?" n; then
        return
    fi

    if [ -t 0 ]; then
        printf 'Profile name [%s]: ' "$PROFILE_NAME"
        read -r chosen || chosen=""
        if [ -n "$chosen" ]; then
            PROFILE_NAME="$chosen"
        fi
    fi

    args=""
    if prompt_yes_no "Use headless device-code login?" n; then
        args="$args --headless"
    fi
    if prompt_yes_no "Force browser/device login instead of reusing existing credentials?" n; then
        args="$args --force"
    fi

    # shellcheck disable=SC2086
    "$INSTALLED_CONFIG_BIN" auth login chatgpt --profile "$PROFILE_NAME" $args

    say ""
    say "Run Claude Code through this profile with:"
    say "  CLAUDEX_PROFILE=$PROFILE_NAME claudex"
}

main() {
    parse_args "$@"
    select_install_dir

    say "Claudex Installer"
    say "================="
    say "Repository: $REPO"
    say "Install dir: $INSTALL_DIR"
    say ""

    check_deps

    if ! install_from_release; then
        say ""
        say "Release install failed."
        if [ "$ALLOW_SOURCE_FALLBACK" != "1" ]; then
            exit 1
        fi
        if prompt_yes_no "Try source install with cargo instead?" y; then
            install_from_source
        else
            exit 1
        fi
    fi

    say ""
    if is_yes "$DRY_RUN"; then
        say "Dry run complete."
        exit 0
    fi

    say "Installation complete."
    say "Installed claudex to $INSTALLED_BIN"
    say "Installed claudex-config to $INSTALLED_CONFIG_BIN"
    "$INSTALLED_CONFIG_BIN" --version 2>/dev/null || true
    verify_installed_latest
    ensure_path_notice
    maybe_stop_proxy
    maybe_setup_chatgpt
}

main "$@"
