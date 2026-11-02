#!/bin/sh
# aco installer: downloads a prebuilt aco from GitHub Releases, verifies its
# SHA-256 checksum, and installs it to ~/.local/bin (or $ACO_INSTALL_DIR).
# It never uses sudo and touches nothing outside the install directory and a
# temporary directory it removes on exit.
#
#   curl -fsSL https://raw.githubusercontent.com/ayaan-cs/aco/main/scripts/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version v0.1.0 --dir ~/bin
#
# Options: --version vX.Y.Z (default: latest release), --dir DIR, --quiet, --help
# Environment: ACO_VERSION, ACO_INSTALL_DIR, NO_COLOR.
#
# Everything runs inside main(), called on the last line, so a download cut
# short by the network cannot run half a script.

set -eu

REPO="${ACO_REPO:-ayaan-cs/aco}"

usage() {
    cat <<USAGE
aco installer

Usage: install.sh [--version vX.Y.Z] [--dir DIR] [--quiet]

  --version V   install release V (for example v0.1.0) instead of the latest
  --dir DIR     install into DIR instead of \$ACO_INSTALL_DIR or ~/.local/bin
  --quiet       print only errors and the installed path
  -h, --help    show this help

Supported: Linux x86_64/aarch64 (static musl builds), macOS x86_64/arm64.
Windows: use WSL2 and run this inside the Linux distribution.
USAGE
}

# ---------------------------------------------------------------- output

setup_colors() {
    if [ -t 2 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then
        B=$(printf '\033[1m'); D=$(printf '\033[2m'); G=$(printf '\033[32m')
        Y=$(printf '\033[33m'); R=$(printf '\033[31m'); C=$(printf '\033[36m')
        N=$(printf '\033[0m')
    else
        B=''; D=''; G=''; Y=''; R=''; C=''; N=''
    fi
}

say()  { [ "$QUIET" = 1 ] || printf '%s\n' "$*" >&2; }
step() { [ "$QUIET" = 1 ] || printf '  %s✓%s %s\n' "$G" "$N" "$*" >&2; }
info() { [ "$QUIET" = 1 ] || printf '  %s·%s %s\n' "$D" "$N" "$*" >&2; }
warn() { printf '  %s!%s %s\n' "$Y" "$N" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$R$B" "$N" "$*" >&2; exit 1; }

banner() {
    [ "$QUIET" = 1 ] && return 0
    printf '%s' "$C$B" >&2
    cat >&2 <<'ART'
    __ _  ___ ___
   / _` |/ __/ _ \
  | (_| | (_| (_) |
   \__,_|\___\___/
ART
    printf '%s  autonomous code optimizer · %s%s\n\n' "$N$D" "$1" "$N" >&2
}

# --------------------------------------------------------------- helpers

need() { command -v "$1" >/dev/null 2>&1; }

# fetch URL FILE [progress]: download with curl or wget, failing on HTTP errors.
fetch() {
    if need curl; then
        if [ "${3:-}" = progress ] && [ "$QUIET" = 0 ] && [ -t 2 ]; then
            curl -fL --proto '=https' --tlsv1.2 --retry 3 --progress-bar -o "$2" "$1"
        else
            curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$2" "$1"
        fi
    elif need wget; then
        if [ "${3:-}" = progress ] && [ "$QUIET" = 0 ] && [ -t 2 ]; then
            wget -q --show-progress --https-only -O "$2" "$1"
        else
            wget -q --https-only -O "$2" "$1"
        fi
    else
        die "neither curl nor wget is installed"
    fi
}

latest_version() {
    if need curl; then
        # The /releases/latest page redirects to /releases/tag/<tag>.
        url=$(curl -fsSLI --proto '=https' -o /dev/null -w '%{url_effective}' \
            "https://github.com/$REPO/releases/latest") || return 1
        tag=${url##*/}
    else
        tag=$(wget -qO- --https-only "https://api.github.com/repos/$REPO/releases/latest" |
            sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
    fi
    case "$tag" in v[0-9]*) printf '%s' "$tag" ;; *) return 1 ;; esac
}

sha256_of() {
    if need sha256sum; then sha256sum "$1" | cut -d ' ' -f 1
    elif need shasum; then shasum -a 256 "$1" | cut -d ' ' -f 1
    elif need openssl; then openssl dgst -sha256 "$1" | sed 's/.*= //'
    else die "no SHA-256 tool found (sha256sum, shasum or openssl); refusing to install unverified"
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) os_name=Linux; os_part=unknown-linux-musl ;;
        Darwin)
            os_name=macOS; os_part=apple-darwin
            # An x86_64 shell under Rosetta on Apple silicon: use the native build.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
                arch=arm64
            fi
            ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            die "Windows is not supported natively (aco's sandboxes are Docker and Linux namespaces). Install WSL2, open your Linux distribution, and run this installer there." ;;
        *) die "unsupported operating system: $os (prebuilt binaries: Linux and macOS; or build from source with cargo)" ;;
    esac
    case "$arch" in
        x86_64 | amd64) arch_part=x86_64 ;;
        aarch64 | arm64) arch_part=aarch64 ;;
        *) die "unsupported CPU architecture: $arch (prebuilt binaries: x86_64 and aarch64/arm64; or build from source with cargo)" ;;
    esac
    TARGET="$arch_part-$os_part"
    PLATFORM="$os_name $arch_part"
}

# ------------------------------------------------------------------ main

main() {
    QUIET=0
    VERSION="${ACO_VERSION:-}"
    DIR="${ACO_INSTALL_DIR:-}"
    while [ $# -gt 0 ]; do
        case "$1" in
            --version) [ $# -ge 2 ] || { setup_colors; die "--version needs a value, e.g. v0.1.0"; }; VERSION=$2; shift 2 ;;
            --version=*) VERSION=${1#*=}; shift ;;
            --dir) [ $# -ge 2 ] || { setup_colors; die "--dir needs a directory"; }; DIR=$2; shift 2 ;;
            --dir=*) DIR=${1#*=}; shift ;;
            -q | --quiet) QUIET=1; shift ;;
            -h | --help) usage; exit 0 ;;
            *) setup_colors; die "unknown option: $1 (see --help)" ;;
        esac
    done
    setup_colors
    [ -n "$DIR" ] || DIR="${HOME:?HOME is not set}/.local/bin"
    case "$VERSION" in "" | v*) ;; *) VERSION="v$VERSION" ;; esac

    detect_target
    if [ -z "$VERSION" ]; then
        VERSION=$(latest_version) || die "could not find the latest release of $REPO (network down, or GitHub rate limit); pass --version vX.Y.Z"
        label="$VERSION (latest)"
    else
        label="$VERSION"
    fi

    banner "$label"
    say "  ${D}platform${N}  $PLATFORM ${D}($TARGET)${N}"
    say "  ${D}install ${N}  $DIR/aco"
    say ""

    asset="aco-$VERSION-$TARGET.tar.gz"
    base="https://github.com/$REPO/releases/download/$VERSION"
    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t aco-install)
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 130' INT TERM HUP

    fetch "$base/$asset" "$tmp/$asset" progress ||
        die "download failed: $base/$asset (does release $VERSION exist and have a $TARGET build?)"
    size=$(wc -c <"$tmp/$asset" | tr -d ' ')
    step "downloaded $asset ${D}($((size / 1024)) KiB)${N}"

    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download $base/SHA256SUMS"
    want=$(awk -v a="$asset" '$2 == a || $2 == "*" a { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$want" ] || die "SHA256SUMS has no entry for $asset"
    got=$(sha256_of "$tmp/$asset")
    [ "$got" = "$want" ] || die "checksum mismatch for $asset: expected $want, got $got. Nothing was installed."
    step "checksum verified ${D}(sha256 ${got%"${got#????????????}"}…)${N}"

    tar -xzf "$tmp/$asset" -C "$tmp" || die "could not unpack $asset"
    src="$tmp/aco-$VERSION-$TARGET/aco"
    [ -f "$src" ] || die "$asset does not contain aco-$VERSION-$TARGET/aco"
    mkdir -p "$DIR" || die "cannot create $DIR (choose another with --dir)"
    [ -w "$DIR" ] || die "$DIR is not writable (choose another with --dir; this installer never uses sudo)"
    # Copy then rename, so a running aco is never left half-written.
    cp "$src" "$DIR/.aco.$$"
    chmod 755 "$DIR/.aco.$$"
    mv -f "$DIR/.aco.$$" "$DIR/aco"
    installed=$("$DIR/aco" --version 2>/dev/null) || die "installed $DIR/aco but it does not run on this machine"
    step "installed $installed ${D}→ $DIR/aco${N}"

    if [ "$QUIET" = 1 ]; then
        printf '%s\n' "$DIR/aco"
        return 0
    fi

    say ""
    case ":${PATH:-}:" in
        *":$DIR:"*) ;;
        *)
            warn "$DIR is not on your PATH. Add it (and put the line in your shell profile, e.g. ~/.profile or ~/.zshrc):"
            say "      ${B}export PATH=\"$DIR:\$PATH\"${N}"
            say ""
            ;;
    esac

    # What aco needs at run time. Detection only: aco itself checks the
    # sandbox properly on every run and says exactly what is missing.
    if [ "$os_name" = macOS ]; then
        if need docker; then info "docker CLI found; aco also needs the daemon running (Docker Desktop, OrbStack or colima)"
        else warn "macOS needs Docker for aco's sandbox (Docker Desktop, OrbStack or colima); without it aco refuses to run code"
        fi
    else
        if need docker; then info "docker CLI found (preferred sandbox)"
        elif need unshare; then info "no docker; aco will use the unshare sandbox if unprivileged user namespaces are enabled"
        else warn "neither docker nor unshare found; aco needs one of them as its sandbox"
        fi
    fi
    need python3 || warn "python3 not found; aco runs Python and checks syntax with it"

    say ""
    say "  ${B}try it:${N}  aco --help"
    say "           aco path/to/slow.py"
    say "  ${D}uninstall: rm \"$DIR/aco\"  (cache: ~/.cache/aco; Docker images: aco-deps:*)${N}"
}

main "$@"
