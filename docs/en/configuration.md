---
title: Configuration
description: Every option of config.toml, its type, default and validation rule.
---

# Configuration

HorizonDB reads one TOML file. Its path is the first command-line argument; the
default is `config.toml` in the current working directory.

```bash
horizondb                        # ./config.toml
horizondb /etc/horizondb/config.toml
```

The file is validated before anything else happens. A missing field, a wrong
type or a rule violation stops the process with exit status 1 and a log line
such as:

```text
ERROR loading configuration file config.toml failed: configuration error: database.flush_interval_secs must be greater than zero
```

!!! warning "Unknown keys are ignored"

    The parser does not reject keys it does not know. A typo such as
    `idle_evicton_secs = 1800` is silently dropped and the option keeps its
    default. Double-check spelling against the tables below.

## `[server]` { #server }

The WebSocket listener.

| Option | Type | Default | Validation | Meaning |
| --- | --- | --- | --- | --- |
| `host` | string | *required* | not empty | Address to bind, e.g. `127.0.0.1` or `0.0.0.0`. |
| `port` | integer (0–65535) | *required* | | TCP port of the WebSocket listener. |
| `max_connections` | integer | `1024` | > 0 | Concurrent client connections. A connection above the limit is accepted at the TCP level and dropped immediately (logged as `connection limit reached, rejecting`). |
| `max_message_size` | integer, bytes | `67108864` (64 MiB) | 1 KiB – 256 MiB | Largest WebSocket message **and** frame accepted from a client. A larger message closes that client's connection, together with every request it has in flight. Size it above your largest document. |

## `[database]` { #database }

The PostgreSQL connection, the pool, and the write-behind and memory policies.

### Connection and pool

| Option | Type | Default | Validation | Meaning |
| --- | --- | --- | --- | --- |
| `host` | string | *required* | | PostgreSQL host. |
| `port` | integer | *required* | | PostgreSQL port. |
| `database` | string | *required* | | Database name. |
| `username` | string | *required* | | Role used by the server (URL-encoded for you). |
| `password` | string | *required* | | Role password (URL-encoded for you, redacted in debug output). |
| `max_connections` | integer | `32` | ≥ `min_connections` | Upper bound of the pool. The pool serves reads of keys not in memory yet (cold loads, `has` / `delete` checks on unknown keys, `all` scans), table creation and the flush. Size it for the number of clients doing cold reads at the same time, typically right after a restart. |
| `min_connections` | integer | `0` | ≤ `max_connections` | Connections kept open even when idle. |
| `acquire_timeout_secs` | integer, s | `5` | > 0 | How long a request waits for a free pool connection. On expiry the request fails with `internal server error`. |
| `idle_timeout_secs` | integer, s | `30` | > 0 | Idle pool connections are closed after this long. |
| `max_lifetime_secs` | integer, s | `1800` | > 0 | Pool connections are recycled after this long. |

### Write-behind and memory

| Option | Type | Default | Validation | Meaning |
| --- | --- | --- | --- | --- |
| `flush_interval_secs` | integer, s | `300` | > 0 | Period of the batch flush of dirty keys to PostgreSQL. It is also the **maximum window of writes lost on a crash**. See [durability](architecture.md#durability). |
| `pack_idle_secs` | integer, s | `120` | `0` disables | A document neither read nor written for this long is packed to compact JSON text in memory (about 10x smaller than the parsed tree). Its next access parses it back in place, without PostgreSQL. |
| `idle_eviction_secs` | integer, s | `900` | `0` disables | A **clean** document (already flushed) neither read nor written for this long leaves memory. Its next access reloads it with one `SELECT`. Dirty documents and documents with a TTL are never evicted. |

Both memory options act on the sweep that runs **once a minute**, so their
effective resolution is one minute. How to choose the values is covered in the
[memory tuning guide](operations.md#memory-tuning).

## `[auth]` { #auth }

A single account protects the server.

| Option | Type | Default | Validation | Meaning |
| --- | --- | --- | --- | --- |
| `username` | string | *required* | not blank | Login name (surrounding whitespace is trimmed). |
| `password` | string | none | exactly one of `password` / `password_hash` | Plaintext secret, compared in constant time. |
| `password_hash` | string | none | exactly one of `password` / `password_hash` | Argon2 hash in PHC string format. Recommended in production: the secret never sits in the file. |
| `session_timeout_secs` | integer, s | `3600` | > 0 | A session expires after this long **without activity**. Each authenticated request refreshes it. |

Setting both `password` and `password_hash`, or neither (an empty or blank
string counts as unset), fails validation with
`auth requires exactly one of password or password_hash`.

### Generating a `password_hash` { #password-hash }

The server parses the hash as a PHC string and verifies it with the
[`argon2`](https://docs.rs/argon2) crate. The algorithm, version and cost
parameters are read from the string itself, so any valid Argon2 PHC string
works. Use **argon2id**:

```text
$argon2id$v=19$m=19456,t=2,p=1$<salt, base64>$<hash, base64>
```

Any tool that outputs a standard PHC string will do. Two common ones:

=== "argon2 CLI"

    The reference implementation's CLI (package `argon2` on most
    distributions). `-k` is the memory cost in KiB, `-e` prints the PHC
    string:

    ```bash
    printf '%s' 'my-long-secret' | argon2 "$(openssl rand -base64 16)" -id -t 2 -k 19456 -p 1 -e
    ```

=== "Python (argon2-cffi)"

    ```bash
    python3 -c 'from argon2 import PasswordHasher; print(PasswordHasher().hash("my-long-secret"))'
    ```

Then put the result in the file, in single quotes so TOML keeps it literally:

```toml
[auth]
username = "admin"
password_hash = '$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$…'
```

!!! warning "A malformed hash is only noticed at login"

    Startup checks that the field is present and not blank, not that it parses.
    With a truncated or mistyped hash the server starts normally and **every**
    login answers `invalid credentials`. Test a login right after changing it.

!!! tip "Cost parameters and login bursts"

    Argon2 verification is CPU-heavy by design. The server runs it on Tokio's
    blocking pool so a burst of logins (every client reconnecting after a
    restart) does not stall other requests, but each login still costs one
    verification. The SDK logs in once per connection, not per request.

## Full annotated example { #example }

This is `example.config.toml` from the repository, with every option spelled
out:

```toml title="config.toml"
[server]
host = "0.0.0.0"               # (1)!
port = 8080
max_connections = 1000         # default 1024
# Largest WebSocket message (request or frame) accepted from a client, in
# bytes. A message above it closes that client's connection together with
# every request it has in flight. 1 KiB .. 256 MiB, default 64 MiB.
max_message_size = 67108864

[database]
host = "127.0.0.1"
port = 5432
database = "horizon"
username = "horizon"
password = "change-me"
# Pool: cold reads and the periodic flush use it; size it for the number of
# clients (default 32).
max_connections = 32
min_connections = 0
acquire_timeout_secs = 5
idle_timeout_secs = 30
max_lifetime_secs = 1800
# How often buffered writes are batch-flushed to PostgreSQL (seconds).
flush_interval_secs = 300      # (2)!
# A flushed document untouched for this long leaves memory and is reloaded
# on its next access. 0 keeps every loaded document forever. Default 900.
idle_eviction_secs = 900
# A document untouched for this long is packed to compact JSON text in
# memory, parsed back on its next access. 0 keeps every document parsed.
# Default 120.
pack_idle_secs = 120

[auth]
username = "admin"
# Exactly one of the two:
#   password      : plaintext secret (constant-time comparison)
#   password_hash : argon2id PHC hash (recommended in production)
password = "secret"
# password_hash = "$argon2id$v=19$m=19456,t=2,p=1$..."
session_timeout_secs = 3600
```

1. The server speaks plain `ws://` and has no TLS of its own. Bind to
   `127.0.0.1` or a private interface, or put a TLS-terminating reverse proxy
   in front.
2. Writes since the last flush live only in memory. A crash loses up to this
   many seconds of them.

## Logging { #logging }

Logs go to standard output and follow the `RUST_LOG` environment variable
(`info` when unset):

| Level | What you get |
| --- | --- |
| `info` | Startup, the [stats lines](operations.md#stats) every minute, each flush, shutdown. |
| `debug` | Also every request (operation, table, key, id, duration), rejected requests and sweep results. |
| `trace` | Also every repository call (`read`, `set`, `delete`, `mutate`…). |

```bash
RUST_LOG=debug ./target/release/horizondb
```

Internal failures (SQL, serialization) are always logged at `error` with
their details; clients only ever receive `internal server error`.
