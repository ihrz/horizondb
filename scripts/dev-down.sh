#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f .server.pid ]; then
  PID="$(cat .server.pid)"
  if kill -0 "$PID" 2>/dev/null; then
    kill "$PID"
    for _ in $(seq 1 50); do
      kill -0 "$PID" 2>/dev/null || break
      sleep 0.1
    done
    echo "[dev-down] server stopped (pid $PID)"
  fi
  rm -f .server.pid
fi

if [ "${1:-}" = "--all" ]; then
  CONTAINER="${HORIZON_PG_CONTAINER:-horizondb-pg}"
  if docker rm -f "$CONTAINER" >/dev/null 2>&1; then
    echo "[dev-down] postgres container removed"
  fi
fi
