#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <vault-path>" >&2
  exit 2
fi

VAULT_PATH=$1
BIN=${BIN:-./target/release/vulcan}

if [[ ! -x "$BIN" ]]; then
  cargo build --release -p vulcan-cli --bin vulcan
fi

if command -v hyperfine >/dev/null 2>&1; then
  hyperfine \
    --warmup 1 \
    "$BIN --vault \"$VAULT_PATH\" scan --full" \
    "$BIN --vault \"$VAULT_PATH\" scan"
else
  echo "hyperfine not found; falling back to time(1)" >&2
  time "$BIN" --vault "$VAULT_PATH" scan --full
  time "$BIN" --vault "$VAULT_PATH" scan
fi
