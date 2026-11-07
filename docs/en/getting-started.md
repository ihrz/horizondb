---
title: Getting started
description: Run HorizonDB locally, build it from source and send a first request.
---

# Getting started

This page takes you from a fresh clone to a first request: a local PostgreSQL,
a running server, and the TypeScript SDK talking to it.

## Requirements { #requirements }

| What | Why |
| --- | --- |
| **Rust**, current stable toolchain | The server uses edition 2024. |
| A **C toolchain** (`cc`, `make`) | The jemalloc allocator is compiled from source by `tikv-jemalloc-sys`. |
| **PostgreSQL** | Durable storage. The development script runs PostgreSQL 16 in a container. |
| **Docker** (or a `docker`-compatible CLI) | Only for `scripts/dev-up.sh`. |
| **Bun** ≥ 1.1 | Only for the TypeScript SDK (`sdk/package.json` engines). The benchmark suite asks for Bun ≥ 1.4. |

## Run it with the development script { #dev-script }

The fastest way to a working server is the helper script at the root of the
repository. It brings up PostgreSQL and the server together:

```bash
git clone https://gitlab.com/ihrz/db.git horizondb
cd horizondb
./scripts/dev-up.sh      # postgres container + config.toml + build + start
./scripts/dev-down.sh    # stop the server (--all also removes the container)
```

What `dev-up.sh` does, step by step:

1. Starts a container named `horizondb-pg` (`postgres:16-alpine`) on the host
   network, listening on `127.0.0.1:55432`, with database, user and password
   all set to `horizon`, and `synchronous_commit=off`.
2. Waits for `pg_isready`.
3. If there is no `config.toml` yet, generates one from `example.config.toml`
   with the database port replaced by `55432`.
4. Builds a **debug** binary (`cargo build`) and starts it detached, logging to
   `.server.log` and writing its pid to `.server.pid`.
5. Waits for the `HorizonDB listening` log line and prints
   `HorizonDB ready on ws://127.0.0.1:8080`.

The script reads three environment variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `HORIZON_PG_CONTAINER` | `horizondb-pg` | Container name (also used by `dev-down.sh --all`). |
| `HORIZON_PG_PORT` | `55432` | Port PostgreSQL listens on. |
| `HORIZON_PORT` | `8080` | Port printed in the "ready" message. It does not change the port the server binds: that comes from `config.toml`. |

!!! note "`synchronous_commit=off` is for development only"

    The container turns off the commit fsync so benchmarks measure the
    application rather than the disk. Do not copy that setting to a production
    PostgreSQL without understanding what it gives up.

## Build from source { #build }

```bash
cp example.config.toml config.toml   # set the PostgreSQL credentials and the auth secret
cargo build --release                # target/release/horizondb
./target/release/horizondb           # reads ./config.toml
./target/release/horizondb /etc/horizondb/config.toml   # explicit path
```

`cargo run --release` and `cargo run --release -- other.toml` work too. The
only command-line argument is the path of the configuration file; without it
the server reads `config.toml` from the current working directory.

The release profile enables LTO, a single codegen unit and `panic = "abort"`.

On startup the server:

1. loads and validates the configuration (any error exits with status 1 and a
   `loading configuration file … failed` log line);
2. connects the PostgreSQL pool and runs `SELECT 1`;
3. binds the WebSocket listener (retrying for up to 15 s if the port is still
   held by a previous instance) and logs `HorizonDB listening`.

Tables are not created up front: each one is created with
`CREATE TABLE IF NOT EXISTS` the first time a request names it.

## Your first request { #first-request }

### With the SDK

The Bun client has no runtime dependencies and the repository root exports it,
so a git dependency is enough:

```json title="package.json"
{
  "dependencies": {
    "horizondb": "git+https://gitlab.com/ihrz/db.git#main"
  }
}
```

```bash
bun install
```

Pin a tag instead of `main` for reproducible builds (`#v1.0.0`). For a private
checkout use SSH (`git+ssh://git@gitlab.com/ihrz/db.git#main`) or a deploy
token in the URL.

```ts title="hello.ts"
import { HorizonDB } from "horizondb";

const db = await HorizonDB.connect({
  url: "ws://127.0.0.1:8080",
  username: "admin",     // [auth] username
  password: "secret",    // [auth] password
});

await db.set("user.name", "Alice");
console.log(await db.get("user.name")); // "Alice"
console.log(await db.add("user.visits")); // 1

db.close();
```

```bash
bun run hello.ts
```

### By hand

Any WebSocket client that sends text frames works. With
[`websocat`](https://github.com/vi/websocat), for example, paste one JSON
packet per line:

```text
$ websocat ws://127.0.0.1:8080
{"id":"1","operation":"login","login":"admin","password":"secret"}
{"id":"1","type":"response","data":{"success":true,"sessionId":"…"},"error":null}
{"id":"2","operation":"set","sessionId":"…","key":"user.name","value":"Alice"}
{"id":"2","type":"response","data":{"success":true},"error":null}
```

The [protocol reference](protocol.md) lists every operation.

## The configuration file { #config-file }

`config.toml` has three sections. A minimal file only needs the fields that
have no default:

```toml title="config.toml"
[server]
host = "127.0.0.1"
port = 8080

[database]
host = "127.0.0.1"
port = 5432
database = "horizon"
username = "horizon"
password = "change-me"

[auth]
username = "admin"
password = "secret"
```

Everything else (pool sizing, flush interval, memory policy, session timeout,
message size) has a default. See [Configuration](configuration.md) for every
option.

## Next steps

- Read [Architecture](architecture.md) to understand what "write-behind" means
  for durability before you put real data in.
- Deploy with the [systemd unit](operations.md#systemd).
- Run the [benchmark suite](benchmarks.md#reproduce) against your own machine.
