#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

CONTAINER="${HORIZON_PG_CONTAINER:-horizondb-pg}"
PG_PORT="${HORIZON_PG_PORT:-55432}"
SERVER_PORT="${HORIZON_PORT:-8080}"

if docker ps --format '{{.Names}}' | grep -qx "$CONTAINER"; then
  echo "[dev-up] postgres container '$CONTAINER' already running"
else
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$CONTAINER" \
    --network=host \
    -e POSTGRES_DB=horizon \
    -e POSTGRES_USER=horizon \
    -e POSTGRES_PASSWORD=horizon \
    docker.io/library/postgres:16-alpine \
    -c listen_addresses=127.0.0.1 -p "${PG_PORT}" \
    -c synchronous_commit=off >/dev/null
  echo "[dev-up] postgres container '$CONTAINER' started (host network, port ${PG_PORT}, synchronous_commit=off)"
fi

for _ in $(seq 1 60); do
  if docker exec "$CONTAINER" pg_isready -U horizon -h 127.0.0.1 -p "$PG_PORT" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
docker exec "$CONTAINER" pg_isready -U horizon -h 127.0.0.1 -p "$PG_PORT" >/dev/null

if [ ! -f config.toml ]; then
  sed -e "s/^port = 5432$/port = ${PG_PORT}/" \
    -e 's/^password = "change-me"$/password = "horizon"/' \
    example.config.toml > config.toml
  echo "[dev-up] generated config.toml from example (db port ${PG_PORT})"
fi

cargo build --quiet
sync
sleep 1
echo "[dev-up] binary built"

if [ -f .server.pid ] && kill -0 "$(cat .server.pid)" 2>/dev/null; then
  echo "[dev-up] server already running (pid $(cat .server.pid))"
else
  rm -f .server.log
  setsid ./target/debug/horizondb > .server.log 2>&1 < /dev/null &
  echo $! > .server.pid
fi

for _ in $(seq 1 100); do
  if grep -q "HorizonDB listening" .server.log 2>/dev/null; then
    echo "[dev-up] HorizonDB ready on ws://127.0.0.1:${SERVER_PORT} (pid $(cat .server.pid))"
    exit 0
  fi
  sleep 0.1
done

echo "[dev-up] server failed to start, log tail:" >&2
tail -n 30 .server.log >&2
exit 1
