#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <vault-path>" >&2
  echo "env: BIN=./target/release/vulcan RUN_MUTATING=1" >&2
  exit 2
fi

VAULT_PATH=$1
BIN=${BIN:-./target/release/vulcan}
RUN_MUTATING=${RUN_MUTATING:-0}

if [[ ! -x "$BIN" ]]; then
  cargo build --release -p vulcan-cli --bin vulcan
fi

commands=(
  "$BIN --vault \"$VAULT_PATH\" vectors queue status"
  "$BIN --vault \"$VAULT_PATH\" vectors index --dry-run"
  "$BIN --vault \"$VAULT_PATH\" vectors repair --dry-run"
  "$BIN --vault \"$VAULT_PATH\" vectors rebuild --dry-run"
)

if [[ "$RUN_MUTATING" == "1" ]]; then
  commands+=(
    "$BIN --vault \"$VAULT_PATH\" vectors index"
    "$BIN --vault \"$VAULT_PATH\" vectors repair"
    "$BIN --vault \"$VAULT_PATH\" vectors rebuild"
  )
fi

if command -v hyperfine >/dev/null 2>&1; then
  hyperfine --warmup 1 "${commands[@]}"
else
  echo "hyperfine not found; falling back to time(1)" >&2
  for command in "${commands[@]}"; do
    echo "==> $command" >&2
    time bash -lc "$command"
  done
fi
