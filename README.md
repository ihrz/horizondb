<p align="center">
  <img src="assets/banner.svg" alt="HorizonDB — JSON key-value over WebSocket. Memory above, PostgreSQL below." width="100%">
</p>

<p align="center">
  <b>The key-value server behind iHorizon.</b><br>
  Reads and writes at memory speed, batched into PostgreSQL behind the scenes,<br>
  spoken over one WebSocket from a zero-dependency Bun client.
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#the-protocol">Protocol</a> ·
  <a href="sdk/SDK_GUIDE.md">SDK guide</a> ·
  <a href="benchmarks/BENCHMARKS.md">Benchmarks</a> ·
  <a href="README.fr.md">Français</a>
</p>

<p align="center">
  <img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-FF7A45?style=flat-square&logo=rust&logoColor=white&labelColor=11153A">
  <img alt="unsafe forbidden" src="https://img.shields.io/badge/unsafe-forbidden-FF5A7A?style=flat-square&labelColor=11153A">
  <img alt="PostgreSQL write-behind" src="https://img.shields.io/badge/PostgreSQL-write--behind-FFB347?style=flat-square&logo=postgresql&logoColor=white&labelColor=11153A">
  <img alt="Bun SDK, zero dependencies" src="https://img.shields.io/badge/SDK-Bun%20·%200%20deps-FFE7A3?style=flat-square&logo=bun&logoColor=white&labelColor=11153A">
</p>

---

```ts
import { HorizonDB } from "horizondb";

const db = await HorizonDB.connect({ url: "ws://127.0.0.1:8080", username: "admin", password: "secret" });

await db.set("guild.42.settings.prefix", "!");                 // dot keys merge into one document
await db.add("guild.42.stats.messages");                       // → 1, the new value, no extra read
await db.push("guild.42.mods", "1234", { returnValue: true }); // → ["1234"]
await db.cache("cooldown.42.daily", true, 86_400_000);         // gone in 24 h

console.log(await db.get("guild.42"));
// { settings: { prefix: "!" }, stats: { messages: 1 }, mods: ["1234"] }
```

## Why HorizonDB

A Discord bot hammers a handful of large JSON documents (a guild, a member, a
counter) from many shards at once. A SQL round trip on every `add` or `push`
makes that slow; a pure in-memory store forgets everything on restart.
HorizonDB sits on the line between the two.

|  |  |
| --- | --- |
| ☀️ **Memory above** | Every read and every write is served from memory. A `push` into a multi-megabyte guild document edits it in place in microseconds: documents are never deep-copied on the hot path. |
| 🌅 **The horizon** | A background task batch-flushes dirty keys to PostgreSQL (`UNNEST` upserts, `ANY` deletes) every `flush_interval_secs`. A graceful shutdown flushes everything first. |
| 🌊 **PostgreSQL below** | Your data ends up in plain tables (`id`, JSON `value`) you can query, back up and restore with the tools you already have. Keys load lazily on first access. |
| 🔒 **No lost updates** | Read-modify-write operations are serialized per key. Sixteen workers hammering one counter end at the exact sum, checked on every benchmark run. |
| 🧭 **Nested keys** | `user.profile.name` addresses inside one document per base key, up to 32 levels deep. Reads copy only the sub-tree you asked for. |
| ⚡ **Answers, not acks** | Mutations return their result: `add`/`sub` the new number, `delete` whether it existed, `push`/`pull` the resulting array on request. No follow-up `get`. |
| 🛡️ **Hardened by default** | `unsafe` forbidden, clippy pedantic clean, argon2id or constant-time auth, validated table names, bounded messages, per-connection backpressure, deadlines, idle-link pings. |

## How it works

```
  client ──ws──▶ ┌───────────────────────────────────────────────┐
                 │  memory      ☀  documents, sessions, TTL keys │  every get / set / add / push
  client ──ws──▶ │              ≡  per-key locks, dirty marks    │  lands here and answers here
                 ├───────── flush every N s, and on shutdown ────┤  ← the horizon
                 │  PostgreSQL  ▭  one table per namespace       │  batched upserts and deletes
                 └───────────────────────────────────────────────┘
```

1. A client logs in and gets a `sessionId`. Every request is a JSON packet
   correlated by `id`, so one socket pipelines hundreds of requests.
2. The request runs against the in-memory store. A key that is not loaded yet
   is fetched once from PostgreSQL (single-flighted, parsed off the async
   runtime), then stays hot.
3. A write marks the key dirty and answers immediately.
4. Every `flush_interval_secs` (default 300), dirty keys go down in batches:
   at most 500 rows or 32 MiB per upsert, 5 000 ids per delete. A write that
   lands during a flush stays dirty for the next one.
5. Every minute a sweep keeps memory proportional to the working set:
   - documents idle for `pack_idle_secs` (default 120) are packed to compact
     JSON text, about 10x smaller than the parsed tree, and parsed back in
     place on their next access (microseconds, no PostgreSQL round trip);
   - clean documents idle for `idle_eviction_secs` (default 900) leave memory
     and reload from PostgreSQL when needed. A dirty key never leaves memory
     before it is flushed;
   - expired TTL keys are purged.

   The allocator (jemalloc) hands freed pages back to the OS, and the stats
   line logs `memory_entries`, `memory_packed`, `memory_evicted`, `allocated`
   and `resident`.

> [!WARNING]
> **Durability is a trade you choose.** Until the next flush, writes live only
> in memory: a crash or `kill -9` loses up to `flush_interval_secs` of them.
> SIGINT and SIGTERM flush everything before exiting. Lower the interval if
> that window is too wide for your data.

## Quick start

You need Rust (stable) and a PostgreSQL instance. The helper script brings up
the database container and the server together:

```bash
./scripts/dev-up.sh      # postgres container + config.toml + build + start (logs in .server.log)
./scripts/dev-down.sh    # stop (--all also removes the container)
```

Or by hand:

```bash
cp example.config.toml config.toml   # set the PostgreSQL credentials and the auth secret
cargo run --release                  # listens on ws://127.0.0.1:8080 by default
cargo run --release -- other.toml    # explicit config path
```

For production, [`horizondb.service`](horizondb.service) is a hardened systemd
unit that leaves the final flush time to finish. With pm2,
`pm2 start pm2.config.cjs` pulls, builds and runs the server on every start,
and `node scripts/deploy.cjs` builds while it keeps serving, then restarts it.

### Install the SDK

The Bun client lives in [`sdk/`](sdk/) and has **no runtime dependencies**.
The repository root already exports it, so a git dependency is enough:

```json
{
  "dependencies": {
    "horizondb": "git+https://gitlab.com/ihrz/db.git#main"
  }
}
```

```bash
bun install
```

Bun runs the TypeScript directly. Pin a tag instead of `main` for reproducible
builds (`#v1.0.0`); for a private checkout use SSH
(`git+ssh://git@gitlab.com/ihrz/db.git#main`) or a deploy token in the URL.

The client re-authenticates once on an expired session, reconnects with
backoff when a socket goes silent, shares one connection between `table()`
views and maps every failure to a typed error. All of it is in the
[SDK guide](sdk/SDK_GUIDE.md).

## The protocol

Any language that speaks WebSocket and JSON can be a client. Requests carry an
`id` and an `operation`; answers carry the same `id` and
`type: "response" | "error"`.

```jsonc
→ { "id": "1", "operation": "login", "login": "admin", "password": "secret" }
← { "id": "1", "type": "response", "data": { "success": true, "sessionId": "9f1c…" } }

→ { "id": "2", "operation": "add", "sessionId": "9f1c…", "table": "json", "key": "guild.42.stats.messages", "amount": 1 }
← { "id": "2", "type": "response", "data": { "success": true, "value": 2 } }
```

| Operation | What it does | Answers with |
| --- | --- | --- |
| `login` / `logout` | open or close a session | `sessionId` |
| `get` | read a key or a nested path, with an optional `defaultValue` | the value |
| `set` | write; a nested path merges into the document | `success` |
| `delete` | remove a leaf or a whole entry | `existed` |
| `add` / `sub` | numeric increment (default ±1, a missing key starts at 0) | the new `value` |
| `push` / `pull` | append to an array / remove every occurrence | the array, with `"returnValue": true` |
| `has` | existence check (a stored `null` counts as absent) | boolean |
| `all` | full table contents, at most 10 000 rows | `[{ id, value }]` |
| `deleteAll` | empty the table | `success` |
| `cache` | write with a TTL in ms (at most 30 days), deleted when it expires | `success` |

A request may carry `"timeoutMs"`: if it already waited longer than that
before it could run, the server answers `deadline exceeded` instead of doing
work for a caller that gave up. Full semantics in [WHAT.md](WHAT.md).

## Configuration

`config.toml`, annotated in [`example.config.toml`](example.config.toml):

| Section | Keys | Notes |
| --- | --- | --- |
| `[server]` | `host`, `port`, `max_connections`, `max_message_size` | Size `max_message_size` above your largest document (default 64 MiB, max 256 MiB): an oversized message closes that connection and everything in flight on it. |
| `[database]` | connection, pool sizing and timeouts, `flush_interval_secs`, `pack_idle_secs`, `idle_eviction_secs` | `max_connections` should follow the number of clients doing cold reads at startup. `0` disables packing or eviction. |
| `[auth]` | `username`, `password` **or** `password_hash`, `session_timeout_secs` | Prefer an argon2id `password_hash` in production. |

Logs follow `RUST_LOG`: `debug` traces every request (operation, table, key,
duration) and every flush batch, `trace` every repository call.

## Performance

Measured end to end through the SDK against a real server and PostgreSQL,
network included, on a laptop with a *debug* build
([method and full table](benchmarks/BENCHMARKS.md)):

| Path | Throughput | p50 |
| --- | --- | --- |
| hot `get`, one connection | 18 k ops/s | 42 µs |
| `has`, one connection | 20 k ops/s | 39 µs |
| login + logout | 17 k ops/s | 43 µs |
| raw WebSocket client, same server | 48 k ops/s | — |

Run your own with `./scripts/bench.sh --quick`. The suite is bounded in time
and fails on any error or broken invariant, so it fits in CI.

## Development

```bash
./scripts/test.sh            # Rust tests + SDK typecheck + SDK tests (hermetic, no server needed)
cargo clippy --all-targets   # pedantic, expected clean
cargo fmt --check
./scripts/bench.sh --quick   # bounded performance suite
```

Design notes live in [WHAT.md](WHAT.md); the landing page in [`site/`](site/);
brand assets and their rules in [`assets/`](assets/README.md).

<p align="center">
  <img src="assets/logo.svg" alt="" width="56"><br>
  <sub>HorizonDB is part of the <b>iHorizon</b> project. Private, work in progress: the wire protocol may still change.</sub>
</p>
