#!/bin/sh
set -eu

binary=${1:-}
if [ -z "$binary" ]; then
    binary=$(command -v vulcan || true)
fi
if [ -z "$binary" ] || [ ! -x "$binary" ]; then
    printf '%s\n' 'Usage: smoke_android_termux.sh <vulcan-binary>' >&2
    exit 2
fi

termux=false
case "${PREFIX:-}" in
    */com.termux/files/usr) termux=true ;;
esac
if [ -n "${TERMUX_VERSION:-}" ]; then
    termux=true
fi
if [ "$termux" != true ]; then
    printf '%s\n' 'Android runtime smoke must run inside Termux.' >&2
    exit 2
fi
if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != aarch64 ]; then
    printf 'Android runtime smoke requires aarch64 Termux; found %s/%s.\n' \
        "$(uname -s)" "$(uname -m)" >&2
    exit 2
fi
if ! command -v git >/dev/null 2>&1; then
    printf '%s\n' 'Git is required; install it with: pkg install git' >&2
    exit 2
fi

reported=$("$binary" --version)
case "$reported" in
    'vulcan '*) ;;
    *) printf 'Unexpected version output: %s\n' "$reported" >&2; exit 1 ;;
esac

temporary=$(mktemp -d "${TMPDIR:-${PREFIX}/tmp}/vulcan-android-smoke.XXXXXX")
cleanup() {
    case "$temporary" in
        */vulcan-android-smoke.*) rm -rf "$temporary" ;;
    esac
}
trap cleanup EXIT HUP INT TERM
vault="$temporary/vault"
mkdir -p "$vault"

"$binary" --vault "$vault" --quiet init --no-import >/dev/null
"$binary" --vault "$vault" --quiet scan >/dev/null
quickjs=$("$binary" --vault "$vault" run --no-startup -e '1 + 1')
if [ "$quickjs" != 2 ]; then
    printf 'QuickJS smoke returned %s instead of 2.\n' "$quickjs" >&2
    exit 1
fi
"$binary" --vault "$vault" --output json vectors models >/dev/null
git -C "$vault" init -q
"$binary" --vault "$vault" --output json sync doctor >/dev/null

printf 'Android runtime smoke passed: %s; QuickJS, vectors, and sync doctor are available.\n' \
    "$reported"
