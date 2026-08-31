#!/bin/sh
set -eu

version=${VULCAN_VERSION:-}
prefix=${VULCAN_INSTALL_PREFIX:-"$HOME/.local"}
base_url=${VULCAN_RELEASE_BASE_URL:-}
dry_run=false

usage() {
    cat <<'EOF'
Usage: install.sh --version <version> [--prefix <directory>] [--base-url <url>] [--dry-run]

Installs a checksummed Vulcan release archive without registering a wiki or enabling the daemon.
Set VULCAN_VERSION, VULCAN_INSTALL_PREFIX, or VULCAN_RELEASE_BASE_URL for non-interactive use.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) version=${2-}; shift 2 ;;
        --prefix) prefix=${2-}; shift 2 ;;
        --base-url) base_url=${2-}; shift 2 ;;
        --dry-run) dry_run=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$version" in
    ''|*[!0-9A-Za-z.+-]*) printf '%s\n' 'A valid --version is required.' >&2; exit 2 ;;
esac

case "$(uname -s)" in
    Linux) os=unknown-linux-gnu ;;
    Darwin) os=apple-darwin ;;
    *) printf 'Unsupported operating system: %s\n' "$(uname -s)" >&2; exit 2 ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) printf 'Unsupported architecture: %s\n' "$(uname -m)" >&2; exit 2 ;;
esac

target="$arch-$os"
archive="vulcan-$version-$target.tar.gz"
if [ -z "$base_url" ]; then
    base_url="https://github.com/tionis/vulcan/releases/download/v$version"
fi

printf 'Version: %s\nTarget: %s\nPrefix: %s\nArchive: %s/%s\n' \
    "$version" "$target" "$prefix" "$base_url" "$archive"
if [ "$dry_run" = true ]; then
    printf '%s\n' 'Dry run: no files were downloaded or installed.'
    exit 0
fi

temporary=$(mktemp -d "${TMPDIR:-/tmp}/vulcan-install.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

download() {
    source_url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$source_url" -o "$destination"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$source_url" -O "$destination"
    else
        printf '%s\n' 'curl or wget is required to download Vulcan.' >&2
        exit 1
    fi
}

download "$base_url/$archive" "$temporary/$archive"
download "$base_url/SHA256SUMS" "$temporary/SHA256SUMS"
expected=$(awk -v name="$archive" '$2 == name { print $1 }' "$temporary/SHA256SUMS")
if [ -z "$expected" ]; then
    printf 'No checksum was published for %s.\n' "$archive" >&2
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$archive" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temporary/$archive" | awk '{ print $1 }')
else
    printf '%s\n' 'sha256sum or shasum is required to verify Vulcan.' >&2
    exit 1
fi
if [ "$actual" != "$expected" ]; then
    printf 'Checksum mismatch for %s.\n' "$archive" >&2
    exit 1
fi

tar -xzf "$temporary/$archive" -C "$temporary"
root="$temporary/vulcan-$version-$target"
test -x "$root/vulcan"
mkdir -p "$prefix/bin" "$prefix/share/man/man1" \
    "$prefix/share/bash-completion/completions" "$prefix/share/fish/vendor_completions.d" \
    "$prefix/share/zsh/site-functions"
install -m 0755 "$root/vulcan" "$prefix/bin/vulcan.new"
mv -f "$prefix/bin/vulcan.new" "$prefix/bin/vulcan"
install -m 0644 "$root/vulcan.1" "$prefix/share/man/man1/vulcan.1"
install -m 0644 "$root/completions/vulcan.bash" "$prefix/share/bash-completion/completions/vulcan"
install -m 0644 "$root/completions/vulcan.fish" "$prefix/share/fish/vendor_completions.d/vulcan.fish"
install -m 0644 "$root/completions/_vulcan" "$prefix/share/zsh/site-functions/_vulcan"

printf 'Installed Vulcan %s at %s/bin/vulcan.\n' "$version" "$prefix"
printf '%s\n' 'The daemon was not enabled. Run vulcan daemon install --dry-run to review it.'
