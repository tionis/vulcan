#!/usr/bin/env bash
set -euo pipefail

out_dir="${1:-target/feature-matrix}"
mkdir -p "$out_dir"

write_tree() {
  local label="$1"
  shift
  echo "writing $out_dir/$label.tree"
  cargo tree "$@" >"$out_dir/$label.tree"
}

write_tree core-no-default -p vulcan-core --no-default-features
write_tree core-js-runtime -p vulcan-core --no-default-features --features js_runtime
write_tree core-vectors -p vulcan-core --no-default-features --features vectors
write_tree core-web -p vulcan-core --no-default-features --features web
write_tree core-oauth -p vulcan-core --no-default-features --features oauth
write_tree app-no-default -p vulcan-app --no-default-features
write_tree cli-no-default -p vulcan-cli --no-default-features
write_tree cli-default -p vulcan-cli

summary="$out_dir/summary.txt"
{
  echo "Vulcan feature matrix dependency tree summary"
  echo
  for tree in "$out_dir"/*.tree; do
    printf "%-28s %5d crates\n" "$(basename "$tree" .tree)" "$(sed '/^[[:space:]]*$/d' "$tree" | wc -l)"
  done
  echo
  echo "Optional backend dependency presence:"
  for dep in vulcan-embed sqlite-vec reqwest rs-trafilatura jsonwebtoken rquickjs; do
    printf "%s\n" "$dep"
    for tree in "$out_dir"/*.tree; do
      if rg -q "(^|[[:space:]])${dep}( |$| v)" "$tree"; then
        printf "  %-26s present\n" "$(basename "$tree" .tree)"
      fi
    done
  done
} >"$summary"

cat "$summary"
