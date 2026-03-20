#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <vault-path> <query>" >&2
  exit 2
fi

VAULT_PATH=$1
QUERY=$2
BIN=${BIN:-./target/release/vulcan}

if [[ ! -x "$BIN" ]]; then
  cargo build --release -p vulcan-cli --bin vulcan
fi

if command -v hyperfine >/dev/null 2>&1; then
  hyperfine \
    --warmup 2 \
    "$BIN --vault \"$VAULT_PATH\" search \"$QUERY\"" \
    "$BIN --vault \"$VAULT_PATH\" search \"$QUERY\" --mode hybrid"
else
  echo "hyperfine not found; falling back to time(1)" >&2
  time "$BIN" --vault "$VAULT_PATH" search "$QUERY"
  time "$BIN" --vault "$VAULT_PATH" search "$QUERY" --mode hybrid
fi
