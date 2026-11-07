---
title: FAQ and troubleshooting
description: Answers to the questions and surprises that come up when running HorizonDB.
---

# FAQ and troubleshooting

## Startup and restarts

??? question "The restart logs `port in use, retrying…` or fails with `EADDRINUSE`"

    On startup the server retries a busy port every 500 ms for **15 s**. That
    covers a restart where the previous instance is still finishing its final
    flush: the old process closes its listener first, then flushes for at
    most 10 s, so the new one gets the port within the window.

    If it still fails after 15 s (`port still in use after retry window, giving up`,
    then `failed to bind`), something else holds the port. Find it with
    `ss -ltnp 'sport = :8080'`: often a second instance started by hand, or a
    second supervisor (systemd **and** pm2) managing the same binary.

??? question "The server exits right after starting"

    Look at the last log line:

    - `loading configuration file … failed`: a validation error, named in the
      message (see [Configuration](configuration.md)).
    - `HorizonDB initialization failed: database connection failed`:
      PostgreSQL is unreachable or refused the credentials. The server
      connects and runs `SELECT 1` before it listens.
    - With `scripts/dev-up.sh`, check the password in the generated
      `config.toml` (see [Getting started](getting-started.md#dev-script)).

??? question "After a restart every request answers `invalid or expired session`"

    Sessions live in memory and do not survive a restart. Log in again. The
    SDK does it automatically, once, when it sees a session error.

## Performance

??? question "The first requests after a restart are slow"

    That is the cold start: memory starts empty and each key is loaded from
    PostgreSQL on its first access (one `SELECT` per base key, then a parse,
    on a blocking thread for documents of 256 KiB or more). Concurrent misses
    on the same key share one load. To shorten it:

    - give the pool enough connections (`database.max_connections`) for the
      number of clients reading at the same time;
    - keep base documents reasonably small: the whole document is loaded,
      even for a nested `get`.

    The same cost applies to a document reloaded after
    [eviction](architecture.md#memory-tiers); `memory_hit_rate` tells you how
    often it happens.

??? question "Requests answer `deadline exceeded`"

    The request waited longer than its `timeoutMs` (10 s by default) before the
    server could start it, typically queued behind 1 024 other requests on the
    same connection. The server skipped it because the client had already
    given up. Spread the load over more connections, lower the pipeline depth,
    or look for slow operations (`all` scans, cold loads) in the debug logs.

??? question "Writes from 16 workers to one counter are slower than to different keys"

    By design: read-modify-write operations on one key are serialized so no
    update is lost. Different keys proceed in parallel (unless they hash to the
    same of the 256 lock shards). Split hot counters across keys when the
    contention matters.

## Memory

??? question "Why is RSS much larger than my data?"

    Several reasons, from the biggest:

    1. **A parsed JSON tree is much bigger than its text**, about ten times
       (~11.2 KB in memory for a ~940-byte document). Packing idle documents
       (`pack_idle_secs`) brings them back close to their JSON size.
    2. **Everything read is kept** until it goes idle for
       `idle_eviction_secs`; with `0`, until restart.
    3. **Dirty documents cannot leave** before the next flush, and TTL keys
       never leave before they expire.
    4. **The allocator returns memory gradually.** jemalloc gives freed pages
       back to the OS on a decay timer, so RSS falls some time after the
       sweep.

    Compare `allocated` and `resident` on the [`memory stats`](operations.md#stats)
    line, and see the [tuning guide](operations.md#memory-tuning).

??? question "`memory_entries` does not move right after a burst of traffic"

    `memory_entries` and `memory_packed` are updated by the sweep, once a
    minute. `memory_evicted` is a total since startup.

## Connections and messages

??? question "My connection closes when I write a large document"

    The message was larger than `server.max_message_size` (64 MiB by default).
    The server closes the whole connection, and every request in flight on it
    fails (the SDK raises `ConnectionClosedError` and reconnects). The server
    log shows `connection ended with error` with a capacity error. Raise the
    limit above your largest document (at most 256 MiB), or split the
    document across several base keys.

??? question "Connections are dropped after two minutes"

    The server pings a connection after 30 s without traffic and drops it after
    120 s. A standard WebSocket client answers pings automatically, so this
    only hits clients that are stuck or behind a proxy that swallows control
    frames.

??? question "New connections are refused under load"

    `server.max_connections` (default 1 024) is reached; the log says
    `connection limit reached, rejecting`. Share connections (one SDK instance
    and its `table()` views use a single socket) or raise the limit.

## Data

??? question "I edited a row in PostgreSQL and the server ignores it"

    Documents already in memory are served from memory, and a dirty one
    overwrites the row at the next flush. Change data through the server, or
    stop it, edit the tables, and start it again.

??? question "I lost writes after a crash"

    Writes are kept in memory until the next flush, every
    `flush_interval_secs` (300 s by default). A crash or `kill -9` loses what
    was not flushed yet; SIGINT and SIGTERM flush first. Lower the interval if
    the window is too wide. See [durability](architecture.md#durability).

??? question "`add` returns `5.0` instead of `5`"

    Numbers updated by `add` and `sub` are 64-bit floats, written as such in
    JSON. JavaScript reads both the same way. Integers above 2^53 lose
    precision, so do not use `add` on snowflake-sized ids.

??? question "A key written with `cache` came back after a restart"

    The TTL deadline is kept in memory only. If a flush wrote the key to
    PostgreSQL and the server restarted before it expired, it is reloaded as a
    permanent key. Delete it explicitly if that matters.

??? question "`all` does not return every row"

    It is capped at 10 000 rows, sorted by id. It is meant for small tables,
    not as a pagination API.

??? question "`get` returns `undefined` for a value I stored as `null`"

    The SDK maps a `null` answer without a default to `undefined`, and the
    server's `has` treats a stored `null` as absent. Store a different
    sentinel if you need to tell them apart.

## Authentication

??? question "Every login answers `invalid credentials` with a `password_hash`"

    The hash is only parsed at login time. A truncated, mistyped or non-PHC
    string makes every verification fail without any error at startup. Paste
    the full PHC string (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`) in single
    quotes, see [Generating a password_hash](configuration.md#password-hash).

??? question "Is there TLS?"

    Not in the server: it speaks plain `ws://`. Keep it on a private network,
    or terminate TLS in a reverse proxy in front of it and connect with
    `wss://`.
