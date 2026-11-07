---
title: Operations
description: Deploying, stopping, monitoring, tuning memory, sizing PostgreSQL, upgrading and backing up HorizonDB.
---

# Operations

## Deploy { #deploy }

Build a release binary on the target (or on a machine with the same libc):

```bash
cargo build --release      # target/release/horizondb
```

The process needs: read access to its `config.toml`, network access to
PostgreSQL, and its listening port. It writes nothing to disk besides its logs
on standard output.

### systemd { #systemd }

The repository ships a hardened unit, [`horizondb.service`](https://gitlab.com/ihrz/db/-/blob/main/horizondb.service):

```ini title="/etc/systemd/system/horizondb.service"
[Unit]
Description=HorizonDB key-value database server
Documentation=https://gitlab.com/ihrz/db
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=ihrz
Group=ihrz
WorkingDirectory=/home/ihrz/db
ExecStart=/home/ihrz/db/target/release/horizondb /home/ihrz/db/config.toml
Restart=on-failure
RestartSec=2
# The final flush serializes every dirty document; give it time on busy
# deployments instead of letting systemd SIGKILL it (writes since the last
# flush would be lost).
TimeoutStopSec=300

# SIGTERM triggers a graceful shutdown that flushes pending writes to PostgreSQL.
# Logging defaults to "info"; use RUST_LOG=debug|trace for more detail.
# Environment=RUST_LOG=info

# Hardening (safe for a network-only server: reads config, writes to PostgreSQL).
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=read-only

[Install]
WantedBy=multi-user.target
```

Adapt `User`, `Group`, `WorkingDirectory` and the paths, then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now horizondb
journalctl -u horizondb -f
```

`ProtectHome=read-only` is enough because the server only reads under the home
directory. If PostgreSQL runs on the same host, add `After=postgresql.service`
so the database is up before the first connection attempt (the server exits if
it cannot connect at startup, and `Restart=on-failure` retries every 2 s).

### pm2 { #pm2 }

pm2 can supervise the binary directly. Give it a kill timeout above the
server's 10 s shutdown budget, otherwise pm2's default (1.6 s) sends SIGKILL in
the middle of the final flush:

```js title="ecosystem.config.cjs"
module.exports = {
  apps: [
    {
      name: "horizondb",
      script: "./target/release/horizondb",
      args: "config.toml",
      cwd: "/home/ihrz/db",
      interpreter: "none",
      kill_timeout: 15000,
      env: { RUST_LOG: "info" },
    },
  ],
};
```

```bash
pm2 start ecosystem.config.cjs
pm2 logs horizondb
```

## Graceful shutdown { #shutdown }

SIGINT and SIGTERM trigger the same sequence:

1. Log `shutdown signal received` and **close the listening socket**, so the
   port is free for the next instance right away, even if the flush is slow.
2. Stop the background loops (session purge, memory sweep, stats).
3. Close every client connection, so no new write races the final flush.
4. Log the final stats lines.
5. Stop the flush loop, run a **final flush** of every dirty key, close the
   PostgreSQL pool.
6. Log `HorizonDB stopped` and exit.

Steps 4 and 5 run within a **10-second** budget. If PostgreSQL is hung or the
final flush is too large, the server logs
`graceful shutdown timed out; buffered writes may not have reached PostgreSQL`
and exits anyway, so a stuck database can never keep the process alive.

!!! warning "The 10 s budget is the real limit"

    `TimeoutStopSec=300` in the unit only prevents systemd from killing the
    process early; the server itself gives up after 10 s. On a busy instance,
    keep the amount of dirty data small enough to flush in that time: a
    shorter `flush_interval_secs` leaves less to write at shutdown. Watch the
    `elapsed` field of the `repository flush complete` lines to know where
    you stand.

The server then exits hard instead of unwinding the runtime, so a stuck task
cannot leave a half-dead process holding the port. On startup it retries a
busy port for 15 s (`port in use, retrying…`), which covers the overlap with
a previous instance during `systemctl restart` or `pm2 restart`.

## Monitoring { #monitoring }

### The stats lines { #stats }

Every minute (and once more at shutdown) the server logs two `info` lines:

```text
INFO server stats active_connections=… total_connections=… active_sessions=… total_requests=… success_rate="…%" memory_hit_rate="…%" memory_entries=… memory_evicted=… memory_packed=…
INFO memory stats allocated="… MiB" resident="… MiB"
```

| Field | Meaning |
| --- | --- |
| `active_connections` | WebSocket connections open right now. |
| `total_connections` | TCP connections accepted since startup, including the ones rejected by `max_connections`. |
| `active_sessions` | Sessions in the store. Expired ones count until the purge that runs every 5 minutes. |
| `total_requests` | Requests dispatched since startup, including the ones answered `deadline exceeded`. Unparseable packets are not counted. |
| `success_rate` | Share of those requests that did not answer an error. Validation errors, refused logins, expired sessions and deadlines all count as failures. `100.00%` before the first request. |
| `memory_hit_rate` | Share of in-memory lookups that found the key, since startup. A miss means a key not in memory: a cold load, a reload after eviction, or a key that does not exist. Each mutation performs two lookups. `0.00%` before the first lookup. |
| `memory_entries` | Documents in memory (hot and packed) after the last sweep. |
| `memory_packed` | Of those, documents held packed after the last sweep. |
| `memory_evicted` | Documents evicted since startup (cumulative). |
| `allocated` | Bytes the allocator currently hands out to the server (jemalloc). |
| `resident` | Physical memory mapped by the allocator, the closest figure to the process RSS. |

The `memory stats` line is absent on MSVC (Windows) builds, which do not use
jemalloc.

### Other lines worth alerting on { #log-lines }

| Level | Message | Meaning |
| --- | --- | --- |
| `info` | `repository flush complete` | One flush, with `cleared`, `deletes`, `upserts`, `statements` and `elapsed`. |
| `error` | `repository flush failed, will retry next cycle` | PostgreSQL rejected or lost a flush statement. Data stays dirty in memory. |
| `error` | `internal request failure` | A request failed on PostgreSQL or serialization; the client got `internal server error`. |
| `warn` | `connection limit reached, rejecting` | `server.max_connections` is full. |
| `warn` | `connection ended with error` | A client link broke: oversized message (`Capacity` error), idle timeout, network error. |
| `warn` | `request dropped: deadline exceeded before dispatch` | A request waited past its `timeoutMs`. Overload or a too-short client timeout. |
| `error` | `graceful shutdown timed out; …` | The final flush did not finish in 10 s. |

## Memory tuning { #memory-tuning }

Two options decide how much memory idle documents cost, both acted on by the
sweep that runs once a minute:

- `pack_idle_secs` (default 120) packs idle documents to compact JSON text, about
  ten times smaller than the parsed tree, without leaving memory. Parsing one
  back costs microseconds (about 4 µs for a 1 KB document, 7 ms for an 870 KB
  one) and never touches PostgreSQL. Keep it on.
- `idle_eviction_secs` (default 900) drops **clean** idle documents from memory;
  the next access pays one `SELECT`. This is what bounds memory to the working
  set.

A recipe for `idle_eviction_secs`:

1. **Start at `1800`.**
2. Let the server run through a normal day and read `memory_hit_rate` on the
   stats line.
3. If it stays **above 97–98 %**, almost everything is served from memory:
   lower to **900** to save more memory.
4. If it falls **below 95 %**, too many accesses wait for PostgreSQL: raise to
   **3600**.
5. Watch `memory_entries`, `memory_evicted` and `resident` move accordingly.

Keep in mind:

- Values **below `flush_interval_secs`** (300 s by default) do not evict
  sooner: a written document stays dirty, hence resident, until the next
  flush. Only documents that are merely read leave earlier.
- The sweep runs every minute, so finer values than a minute make no
  difference.
- `0` disables a step. `idle_eviction_secs = 0` brings back the old behaviour:
  every document ever read stays in memory until restart.
- Keys written with `cache` (TTL) are never evicted, only packed and purged
  when they expire.

## PostgreSQL sizing { #postgresql }

- **Connections.** The server opens at most `database.max_connections`
  (default 32). PostgreSQL's own `max_connections` must leave room for it plus
  your other clients. The flush uses one connection at a time; the pool is
  mostly sized for **cold reads**, which peak right after a restart when every
  client reads keys that are not in memory yet. A request that waits more than
  `acquire_timeout_secs` for a connection fails with `internal server error`.
- **Write volume.** Each flush rewrites the **whole document** of every dirty
  key (`value` is replaced, not patched). A large document written once per
  interval costs its full size per flush. Large rows are TOASTed by PostgreSQL,
  and rewritten rows leave dead tuples behind: keep autovacuum enabled and
  check it keeps up on the busiest tables.
- **Commit latency.** The development container runs with
  `synchronous_commit=off`; on production, keep PostgreSQL's durability settings
  and let the flush batching absorb the cost (one commit per statement of up
  to 500 rows).
- **TLS.** The configuration has no TLS option; the driver's default
  (`prefer`: TLS when the server offers it) applies.

## Upgrading { #upgrading }

1. Build the new binary.
2. Restart the service (`systemctl restart horizondb` or `pm2 restart horizondb`).
   The old process flushes and exits, the new one binds the port as soon as it
   is free.
3. Clients see their connections close and their sessions disappear; the SDK
   reconnects and logs in again on its own.

The protocol is still evolving: upgrade the SDK in your applications together
with the server, ideally pinned to the same git tag.

## Backups { #backups }

The data is in plain PostgreSQL tables, one per HorizonDB table:

```sql
CREATE TABLE "<table>" (id VARCHAR(255) PRIMARY KEY, value TEXT);
```

`value` is JSON text, so it can be queried with a cast:

```sql
SELECT id, value::jsonb -> '42' -> 'settings' AS settings FROM "json" WHERE id = 'guild';
```

Back up with your usual tools (`pg_dump`, base backups, WAL archiving). Two
things to keep in mind:

- **A backup holds the state of the last flush.** Writes from the last
  `flush_interval_secs` are still in memory. For a backup that contains
  everything, stop the server gracefully first (it flushes), then dump.
- **Restore with the server stopped.** A running server keeps serving its
  in-memory copies and would overwrite restored rows with dirty ones at the
  next flush. Stop it, restore, start it: memory starts empty and documents
  load lazily from the restored tables.
