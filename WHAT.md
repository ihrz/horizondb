# HorizonDB — Project Brief

## What?

HorizonDB is a **JSON-oriented key-value database server** written in **Rust** (edition 2024). It exposes its API over **WebSocket** and persists to **PostgreSQL** through an in-memory **write-behind buffer**: writes land in memory first (reads are served from memory, including dot-nested keys), and a background task batch-flushes them to PostgreSQL every `flush_interval_secs`.

## Key characteristics

- **Language**: Rust edition 2024, async Tokio, `unsafe_code = "forbid"`
- **Protocol**: WebSocket JSON — responses shaped `{ id, type, data, error }`
- **Storage**: PostgreSQL via `sqlx` 0.8 — tables created on demand (`id VARCHAR(255)` PK, `value TEXT` holding JSON); batch flush via `UNNEST` upserts and `ANY` deletes
- **Write-behind**: every write updates the in-memory store immediately and is flushed in batches (default every 300 s, configurable via `database.flush_interval_secs`); keys loaded from PostgreSQL lazily on first access; graceful shutdown flushes everything
- **Documents are never cloned whole on the hot path**: a nested `get` copies only the addressed sub-tree, and `set`/`add`/`push`/`pull`/nested `delete` edit the stored document in place under a per-key lock. A `push` into a multi-megabyte guild document costs microseconds, not a deep copy of the document (which is what made production collapse under load)
- **Auth**: constant-time comparison (`subtle`) or **argon2id** hash; UUID sessions with automatic expiry
- **Bounded memory**: clean documents idle for `database.idle_eviction_secs` (default 900, `0` disables) are evicted and reloaded on next access; dirty and TTL entries are never evicted; jemalloc returns freed pages to the OS
- **Monitoring**: atomic counters + periodic stats (connections, success rate, memory hit rate, resident entries, evictions, allocator allocated/resident)

## Architecture (`src/`)

| Module | Role |
|---|---|
| `main.rs` | Bootstrap: tracing, config load, server start |
| `config.rs` | Typed config + validation (XOR password/password_hash…) |
| `state.rs` | Shared `AppState` (repo, sessions, metrics) + shutdown |
| `server.rs` | Accept loop, connection limit (`Semaphore`), signals, purge loops |
| `handlers.rs` | Operation dispatch + business logic |
| `db.rs` | Write-behind repository: in-memory store + dirty tracking + batched flush to `PostgreSQL` |
| `auth.rs` | Credential verification + session store |
| `values.rs` | Pure JSON operations (nested keys) + table validation |
| `models.rs` | Protocol request/response packets |
| `error.rs` | `AppError` (client-safe messages, details only in logs) |
| `metrics.rs` | `AtomicU64` counters |

## Security

- Table names validated against `[A-Za-z_][A-Za-z0-9_]{0,62}` before any SQL interpolation → injection eliminated
- Constant-time credential comparison; optional argon2id `password_hash` in config
- Configurable WebSocket max message size (DoS protection), frames ≤ message cap; a violation is logged at `warn` with the peer address
- Keys: length ≤ 255 bytes (column width), nesting depth ≤ 32; finite `amount` required; TTL ≤ 30 days
- Internal errors (SQL/serialization) are logged server-side; clients receive a generic message

## Performance

- `CREATE TABLE IF NOT EXISTS` executed once per table per process (single-flight + HashSet)
- Writes (set/add/sub/push/pull/nested set & delete) touch **only memory** — no PostgreSQL round trip in the hot path; the pool is used just for reads of not-yet-loaded keys and the periodic batch flush
- Read-modify-write operations are serialized per key by an in-process sharded mutex — no lost updates, no row locks, no retries (benchmark invariants verify this)
- Each stored document sits behind its own `RwLock`: readers copy only the projected sub-tree, writers mutate in place. The global store lock is only held for map lookups, never while a document is being read or copied
- Large JSON payloads (cold reads, table scans) are parsed on the blocking pool; the flush yields between documents so a big batch cannot monopolize a runtime worker
- Flush batches per table: `INSERT ... SELECT UNNEST(...) ... ON CONFLICT DO UPDATE` for writes (chunked: at most 500 rows / 32 MiB per statement), `DELETE ... WHERE id = ANY(...)` for deletes (at most 5 000 ids per statement), `DELETE FROM` for cleared tables. Dirty marks carry a generation number, so a write that lands while a flush is running stays dirty for the next cycle
- Sessions are refreshed without a global write lock (atomic timestamps under a read lock)
- At most 1 024 requests in flight per connection; beyond that the socket is not read until responses drain (TCP backpressure instead of unbounded task growth). A request that already waited longer than its client timeout (`timeoutMs` in the packet, default 10 s) is answered with `deadline exceeded` instead of being executed for a caller that gave up
- Cold loads from `PostgreSQL` run outside the per-key write lock and are single-flighted per key; `CREATE TABLE IF NOT EXISTS` runs without holding the table registry lock
- Connections that stay silent are pinged after 30 s and dropped after 120 s, so a half-open link releases its connection permit
- Lock-free metrics (`AtomicU64`)
- TTL entries (`cache` operation) self-expire in memory; the background sweep removes them every minute, together with the idle clean documents (eviction checks the dirty tracker under the per-key write lock, so a pending write is never dropped)
- Connections beyond `server.max_connections` rejected immediately

> **Durability note**: writes are only in memory until the next flush. If the process crashes or is killed, up to `flush_interval_secs` of writes are lost. A graceful shutdown (SIGINT/SIGTERM) flushes everything first.

## Diagnostics

Verbose per-request and per-call logging is driven by `RUST_LOG` (defaults to `info`):

```bash
RUST_LOG=debug cargo run   # every request (operation, table, key, id, duration) + flush batches
RUST_LOG=trace cargo run   # also every repository call (get/set/delete/mutate)
```

Internal failures are always logged at `error` with the table and key involved.

## Operations

| Operation | Description |
|---|---|
| `login` | Authenticate, returns a `sessionId` |
| `logout` | Destroys the session |
| `get` | Read (dot-nested keys `a.b.c`, optional default value) |
| `set` | Write (merges when path is nested) |
| `delete` | Delete leaf key or whole entry; answers `{ success, existed }` |
| `add` / `sub` | Numeric increment/decrement (default ±1, missing starts at 0); answers `{ success, value }` with the new number |
| `push` | Append to a JSON array; with `"returnValue": true` answers `{ success, value }` with the resulting array |
| `pull` | Remove **all** occurrences of an element; same `returnValue` option as `push` |
| `has` | Key existence check |
| `all` | Full table contents (server-limited to 10 000 rows) |
| `deleteAll` | Empty the table |
| `cache` | Persist + TTL in ms, auto-deleted at expiration |

## Configuration (`config.toml`, see `example.config.toml`)

- `[server]` host, port, max_connections, max_message_size (default 64 MiB, max 256 MiB; a message above the limit closes the client connection together with every request in flight on it, so size it for the largest document)
- `[database]` connection + pool sizing/timeouts + `flush_interval_secs` (write-behind flush period, default 300) + `idle_eviction_secs` (idle clean documents leave memory, default 900, `0` disables)
- `[auth]` username + `password` **or** `password_hash` (argon2id), session_timeout_secs

## Quality gates

```bash
./scripts/test.sh            # Rust tests + SDK typecheck + SDK tests
cargo clippy --all-targets   # 0 warnings (pedantic enabled)
cargo fmt --check
```

End-to-end smoke test validated against real PostgreSQL (Docker) through the TypeScript SDK.

## Ecosystem

- [`sdk/`](sdk/) — Bun-only TypeScript client ([guide](sdk/SDK_GUIDE.md))
- [`scripts/`](scripts/) — `dev-up.sh`, `dev-down.sh`, `test.sh`, `bench.sh`
- [`benchmarks/`](benchmarks/) — performance suite ([methodology & numbers](benchmarks/BENCHMARKS.md))

## Running

```bash
cp example.config.toml config.toml   # then edit
cargo run --release                  # default: ./config.toml
cargo run --release -- other.toml    # explicit path
```

Clean shutdown on SIGINT/SIGTERM (final stats logged).
