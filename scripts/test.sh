#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== Rust unit tests ==="
cargo test --quiet

echo ""
echo "=== SDK typecheck ==="
(cd sdk && bun run typecheck)

echo ""
echo "=== SDK unit tests (mock protocol server) ==="
(cd sdk && bun test)

echo ""
echo "ALL TESTS PASSED"
