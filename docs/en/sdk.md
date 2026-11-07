---
title: TypeScript SDK
description: The Bun client for HorizonDB, its options, methods, errors and reconnection behaviour.
---

# TypeScript SDK

The official client lives in [`sdk/`](https://gitlab.com/ihrz/db/-/tree/main/sdk)
of the repository. It is a **Bun-only**, ESM TypeScript library with **no
runtime dependency**: it uses Bun's global `WebSocket` and
`crypto.randomUUID`. Bun runs the TypeScript sources directly, there is no
build step.

## Install { #install }

The repository root exports the SDK, so a git dependency is enough:

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

## Connect { #connect }

```ts
import { HorizonDB } from "horizondb";

// Recommended: resolves once the socket is open and the login succeeded.
const db = await HorizonDB.connect({
  url: "ws://127.0.0.1:8080",
  username: "admin",
  password: "secret",
});
```

The constructor works without `await`: the connection starts immediately and
every operation waits for the handshake (and the login) to finish.

```ts
const db = new HorizonDB("ws://127.0.0.1:8080"); // bare URL, no credentials
const db2 = new HorizonDB({
  url: "ws://127.0.0.1:8080",
  username: "admin",
  password: "secret",
  table: "json",
  requestTimeoutMs: 10_000,
});
await db2.ready();
```

Without credentials the socket still opens, and operations fail with
`invalid or expired session` until a `login()` succeeds.

### Options { #options }

| Option | Type | Default | Meaning |
| --- | --- | --- | --- |
| `url` | `string` | `ws://127.0.0.1:8080` | Server address. |
| `username` | `string` | none | `[auth] username`. With `password`, the client logs in on connect and after every reconnect. |
| `password` | `string` | none | `[auth]` secret (the plaintext, also when the server stores a hash). |
| `table` | `string` | `json` | Default table of this instance. Validated locally. |
| `requestTimeoutMs` | `number` | `10000` | Per-request timeout. Also sent to the server as `timeoutMs`. `0` disables timers and the server-side deadline hint. |

!!! warning "When the first connection fails"

    If the server is unreachable, `connect()` / `ready()` reject with
    `unable to connect to <url>`, but the underlying connection keeps
    retrying in the background. With `HorizonDB.connect()` you have no handle
    to stop it; if you need to give up cleanly, use the constructor and call
    `close()` when `ready()` rejects:

    ```ts
    const db = new HorizonDB({ url, username, password });
    try {
      await db.ready();
    } catch (error) {
      db.close();
      throw error;
    }
    ```

## Methods { #methods }

All data methods are asynchronous, accept dotted keys (`"a.b.c"`, resolved by
the server, see [keys](protocol.md#keys)) and reject an empty key locally with
a `TypeError` before anything is sent.

| Method | Resolves to | Notes |
| --- | --- | --- |
| `HorizonDB.connect(options)` | `HorizonDB` | Opens and logs in when credentials are given. |
| `ready()` | `void` | Waits for the connection and the initial login. |
| `table(name)` | `HorizonDB` (sync) | A view on another table sharing the same connection. |
| `get<T>(key, defaultValue?)` | `T \| undefined` | `undefined` when absent and no default. |
| `set(key, value)` | `void` | Creates or replaces; a nested key merges. |
| `delete(key)` | `boolean \| undefined` | Whether the key existed. |
| `add(key, amount = 1)` | `number \| undefined` | The new value. A missing key starts at 0. |
| `sub(key, amount = 1)` | `number \| undefined` | The new value. |
| `push(key, element, { returnValue? })` | `Json[] \| undefined` | The resulting array when `returnValue: true`. |
| `pull(key, element, { returnValue? })` | `Json[] \| undefined` | Removes every occurrence. |
| `has(key)` | `boolean` | A stored `null` counts as absent. |
| `all()` | `{ id, value }[]` | Whole table, at most 10 000 rows. |
| `deleteAll()` | `void` | Empties the current table. |
| `cache(key, value, ttlMs = 60000)` | `void` | Writes, then deleted after the TTL. |
| `login(username, password)` | `void` | Manual session management. |
| `logout()` | `void` | Revokes the current session. |
| `close()` | `void` (sync) | Closes the socket and rejects requests in flight. |
| `connectionCount` | `number` (getter) | Sockets opened so far; `1` means never reconnected. |

### Reading { #reading }

```ts
await db.set("user", { name: "Alice", tags: ["a"], profile: { age: 30 } });

await db.get("user.name");               // "Alice"
await db.get<number>("user.profile.age"); // 30
await db.get("user.missing");            // undefined
await db.get("user.missing", "n/a");     // "n/a"
await db.has("user.tags");               // true
```

!!! note "`null` reads as `undefined`"

    `get` returns `undefined` whenever the server answers `null` and no default
    was given, so a stored `null` and a missing key look the same. `has()`
    also returns `false` for both. Store a sentinel value if you need to tell
    them apart.

### Writing { #writing }

```ts
await db.set("guild.42.settings.prefix", "!");   // merges into "guild"
await db.add("guild.42.stats.messages");          // 1
await db.add("guild.42.stats.messages", 10);      // 11
await db.sub("guild.42.stats.messages", 2);       // 9
await db.delete("guild.42.settings.prefix");      // true
await db.delete("guild.42.settings.prefix");      // false
```

### Arrays { #arrays }

```ts
await db.push("guild.42.mods", "1234");                        // undefined
await db.push("guild.42.mods", "5678", { returnValue: true }); // ["1234", "5678"]
await db.pull("guild.42.mods", "1234", { returnValue: true }); // ["5678"]
```

`returnValue` is off by default because the array can be large.

### TTL keys { #ttl }

```ts
await db.cache("cooldown.42.daily", true, 86_400_000); // 24 h
```

The TTL applies to the **base document** (`cooldown` here): see the
[`cache` operation](protocol.md#op-cache) for the details.

### Tables { #tables }

`table()` returns a new instance bound to another table. It shares the
WebSocket connection and the session, nothing is reopened:

```ts
const users = db.table("users");
await users.set("42.name", "Bob");
await db.get("42.name");     // undefined: different table
await users.get("42.name");  // "Bob"
```

Table names are checked locally against `[A-Za-z_][A-Za-z0-9_]{0,62}`
(`InvalidTableNameError`) before anything is sent, on top of the server's own
check.

### Scans and cleanup { #scans }

```ts
const rows = await db.table("users").all(); // [{ id: "42", value: { name: "Bob" } }]
await db.table("users").deleteAll();
```

## Errors { #errors }

| Class | Extends | Thrown when |
| --- | --- | --- |
| `HorizonDBOpError` | `TypeError` | The server answered `type: "error"`. The message is the server's message (see the [error list](protocol.md#errors)). Session errors use this class with `name = "HorizonDBSessionError"`. |
| `HorizonDBAuthError` | `Error` | `login` was refused (`invalid credentials`). |
| `HorizonDBTimeoutError` | `Error` | No answer within `requestTimeoutMs`. Has an `operation` property. The message ends with `(connection not ready)` when the wait was for a reconnect. |
| `ConnectionClosedError` | `Error` | The socket closed, or the SDK reset it, with the request in flight; or the instance was closed. |
| `InvalidTableNameError` | `TypeError` | A table name was rejected locally. |

```ts
import { HorizonDBOpError, HorizonDBTimeoutError } from "horizondb";

try {
  await db.add("user.name", 5); // "Alice" is not a number
} catch (error) {
  if (error instanceof HorizonDBOpError) {
    console.error(error.message); // "cannot add to non-numeric value"
  } else if (error instanceof HorizonDBTimeoutError) {
    console.error("timed out:", error.operation);
  } else {
    throw error;
  }
}
```

## Resilience { #resilience }

The connection handles the usual failures on its own:

- **Expired session.** When an operation fails with a session error and the
  credentials are known, the client logs in again **once** and replays the
  request. Concurrent callers share one login round trip.
- **Closed socket.** In-flight requests are rejected with
  `ConnectionClosedError`, and the client reconnects with exponential backoff
  (250 ms doubling up to 5 s). After reconnecting it logs in **before**
  releasing the requests that were waiting, so they never race the login.
  Requests issued meanwhile wait for the reconnect, within their own
  `requestTimeoutMs`.
- **Silent server.** If a request times out and **nothing at all** was
  received on the socket during its whole window, the server is considered
  unreachable on that link (stuck process, half-open TCP): the client drops
  the socket and reconnects instead of letting every following request time
  out. A server that is merely slow, still sending responses, never triggers
  this.
- **Deadline hint.** Every packet carries `timeoutMs` so the server can skip a
  request it could not start in time (`deadline exceeded`).

`logout()` and `close()` are not retried.

## Exports { #exports }

```ts
import {
  HorizonDB,                       // the client
  Connection,                      // the transport (advanced use)
  HorizonDBOpError, HorizonDBAuthError, HorizonDBTimeoutError,
  ConnectionClosedError, InvalidTableNameError,
  DEFAULT_TABLE,                   // "json"
  DEFAULT_REQUEST_TIMEOUT_MS,      // 10000
  isValidTableName, resolveTable, requireKey,
  type HorizonDBOptions, type Json, type KeyValueEntry,
  type MutationResponse, type ResponsePacket,
  type ArrayMutationOptions, type ResolvedHorizonDBOptions, type ConnectionOptions,
} from "horizondb";
```

`Json` is the recursive JSON type (`null | boolean | number | string | Json[] |
{ [key: string]: Json }`), `KeyValueEntry` is `{ id: string; value: Json }`.

## Developing the SDK { #development }

```bash
cd sdk
bun install        # dev dependencies only (@types/bun, typescript)
bun test           # hermetic: an in-memory mock of the protocol, no server needed
bun run typecheck  # tsc --noEmit, strict
```

`sdk/tests/helpers/mock-server.ts` mirrors the Rust server's semantics,
quirks included. When the server's behaviour changes, update the mock first.
