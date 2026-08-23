# HorizonDB — Project Brief

## What?

HorizonDB is a **JSON-oriented key-value database server** written in **Rust** (edition 2024). It exposes its API over **WebSocket**, persists to **PostgreSQL**, and serves reads through a bounded in-memory TTL cache.

## Key characteristics

- **Language**: Rust edition 2024, async Tokio, `unsafe_code = "forbid"`
- **Protocol**: WebSocket JSON — responses shaped `{ id, type, data, error }`
- **Storage**: PostgreSQL via `sqlx` 0.8 — tables created on demand (`id VARCHAR(255)` PK, `value TEXT` holding JSON)
- **Cache**: capacity-bounded TTL cache (expired first, then oldest)
- **Auth**: constant-time comparison (`subtle`) or **argon2id** hash; UUID sessions with automatic expiry
- **Monitoring**: atomic counters + periodic stats (connections, success rate, cache hit rate)

## Architecture (`src/`)

| Module | Role |
|---|---|
| `main.rs` | Bootstrap: tracing, config load, server start |
| `config.rs` | Typed config + validation (XOR password/password_hash…) |
| `state.rs` | Shared `AppState` (repo, cache, sessions, metrics) + shutdown |
| `server.rs` | Accept loop, connection limit (`Semaphore`), signals |
| `handlers.rs` | Operation dispatch + business logic |
| `db.rs` | sqlx repository: DDL ensured once per table, CAS mutations |
| `cache.rs` | Bounded per-table TTL cache |
| `auth.rs` | Credential verification + session store |
| `values.rs` | Pure JSON operations (nested keys) + table validation |
| `models.rs` | Protocol request/response packets |
| `error.rs` | `AppError` (client-safe messages, details only in logs) |
| `metrics.rs` | `AtomicU64` counters |

## Security

- Table names validated against `[A-Za-z_][A-Za-z0-9_]{0,62}` before any SQL interpolation → injection eliminated
- Constant-time credential comparison; optional argon2id `password_hash` in config
- Configurable WebSocket max message size (DoS protection), frames ≤ message cap
- Keys: length ≤ 255 bytes (column width), nesting depth ≤ 32; finite `amount` required; TTL ≤ 30 days
- Internal errors (SQL/serialization) are logged server-side; clients receive a generic message

## Performance

- `CREATE TABLE IF NOT EXISTS` executed once per table per process (single-flight + HashSet)
- Read-modify-write operations (add/sub/push/pull/nested set & delete) are serialized per key by an in-process sharded mutex plus a Postgres row lock (`SELECT ... FOR UPDATE` inside a transaction) — writers queue instead of retry-storming, so no lost updates and no livelock under hot-key contention (benchmark invariants verify this)
- Lock-free metrics (`AtomicU64`)
- Bounded cache (`capacity`) with periodic expired-entry purge
- Connections beyond `server.max_connections` rejected immediately

## Diagnostics

Verbose per-request and per-call logging is driven by `RUST_LOG` (defaults to `info`):

```bash
RUST_LOG=debug cargo run   # every request (operation, table, key, id, duration) + mutate retries
RUST_LOG=trace cargo run   # also every repository call (get/set/delete/mutate) + slow-call details
```

Internal failures are always logged at `error` with the table and key involved.

## Operations

| Operation | Description |
|---|---|
| `login` | Authenticate, returns a `sessionId` |
| `logout` | Destroys the session |
| `get` | Read (dot-nested keys `a.b.c`, optional default value) |
| `set` | Write (merges when path is nested) |
| `delete` | Delete leaf key or whole entry |
| `add` / `sub` | Numeric increment/decrement (default ±1, missing starts at 0) |
| `push` | Append to a JSON array |
| `pull` | Remove **all** occurrences of an element |
| `has` | Key existence check |
| `all` | Full table contents (server-limited to 10 000 rows) |
| `deleteAll` | Empty the table |
| `cache` | Persist + TTL in ms, auto-deleted at expiration |

## Configuration (`config.toml`, see `example.config.toml`)

- `[server]` host, port, max_connections, max_message_size
- `[database]` connection + pool sizing/timeouts
- `[auth]` username + `password` **or** `password_hash` (argon2id), session_timeout_secs
- `[cache]` capacity

## Quality gates

```bash
./scripts/test.sh            # Rust tests (28) + SDK typecheck + SDK tests (34)
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
