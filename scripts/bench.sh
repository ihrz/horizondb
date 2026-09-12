#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

./scripts/dev-up.sh

PROFILE="standard"
EXTRA_ARGS=()
for arg in "$@"; do
  case "$arg" in
    --quick|--full) PROFILE="$arg";;
    *) EXTRA_ARGS+=("$arg");;
  esac
done

echo ""
echo "=== HorizonDB benchmark suite ($PROFILE profile) ==="
bun benchmarks/bench.ts --profile "$PROFILE" ${EXTRA_ARGS+"${EXTRA_ARGS[@]}"}
