# SDK Guide — `@horizondb/sdk`

A **Bun-only** TypeScript (ESM) client for HorizonDB. One class, `HorizonDB`, speaking the server's WebSocket protocol (JSON request/response correlated by `id`) and exposing a simple key-value API inspired by "SteganoDB-style" SDKs but **asynchronous** (WebSocket, after all).

---

## 1. Installation & structure

```bash
cd sdk
bun install        # installs @types/bun + typescript (dev only, zero runtime dependencies)
bun test           # 40 tests, hermetic: no Rust server or PostgreSQL required
bun run typecheck  # tsc --noEmit, strict
```

```
sdk/
├── src/
│   ├── index.ts       # public export barrel
│   ├── types.ts       # Json, HorizonDBOptions, Packet/ResponsePacket, constants
│   ├── utils.ts       # table/key validation (pure, tested)
│   ├── connection.ts  # transport: WS, id↔promise correlation, timeout, re-login
│   └── client.ts      # public HorizonDB facade
└── tests/
    ├── helpers/mock-server.ts  # in-memory protocol server (Bun.serve)
    ├── client.test.ts          # full behavioral suite
    └── utils.test.ts           # pure validator & error tests
```

**Runtime dependency: none.** Uses Bun's global `WebSocket`, `crypto.randomUUID` and `Bun.serve` (tests only).

---

## 2. Quick start

```ts
import { HorizonDB } from "./sdk/src";

// Recommended factory: connects + authenticates before resolving.
const db = await HorizonDB.connect({
  url: "ws://127.0.0.1:8080",
  username: "admin",
  password: "secret",
});

await db.set("user.name", "Alice");
console.log(await db.get("user.name")); // "Alice"

db.close();
```

The constructor also works without `await` (the connection starts immediately;
every operation waits for the handshake to finish):

```ts
const db = new HorizonDB("ws://127.0.0.1:8080"); // bare-URL shorthand
// or with full options:
const db2 = new HorizonDB({
  url: "ws://127.0.0.1:8080",
  username: "admin",
  password: "secret",
  table: "json",          // default table for operations
  requestTimeoutMs: 10_000,
});
```

Without credentials the connection still opens; operations fail with
`invalid or expired session` until a `login()` succeeds.

---

## 3. API

All methods are asynchronous. Keys support dot nesting (`"a.b.c"`), resolved
server-side.

| Method | Signature | Notes |
|---|---|---|
| `connect` | `static connect(options): Promise<HorizonDB>` | opens + logs in when credentials given |
| `ready` | `ready(): Promise<void>` | awaits connection/login completion |
| `table` | `table(name): HorizonDB` | isolated view sharing the connection |
| `get` | `get<T>(key, defaultValue?): Promise<T \| undefined>` | `undefined` when absent and no default |
| `set` | `set(key, value): Promise<void>` | creates/replaces, nested merge |
| `add` / `sub` | `add(key, amount = 1): Promise<number \| undefined>` | missing ⇒ starts at 0; non-numeric ⇒ error; resolves to the new value |
| `delete` | `delete(key): Promise<boolean \| undefined>` | removes leaf or whole entry; resolves to whether it existed |
| `push` / `pull` | `push(key, element, { returnValue? })` | array; missing ⇒ `[...]` created; with `returnValue: true` resolves to the resulting array |
| `connectionCount` | getter | sockets opened so far (1 = never reconnected) |
| `has` | `has(key): Promise<boolean>` | stored `null` counts as absent |
| `all` | `all(): Promise<{ id, value }[]>` | full table contents |
| `deleteAll` | `deleteAll(): Promise<void>` | empties the current table |
| `cache` | `cache(key, value, ttlMs = 60000)` | persists then deleted after TTL |
| `login` / `logout` | — | manual session management |
| `close` | `close(): void` | rejects in-flight requests |

### Multiple tables

`table()` returns a new view that **shares the WebSocket connection** (no
reconnection) with its own default table:

```ts
await db.table("users").set("42.name", "Bob");
await db.get("42.name");                    // undefined — different table
await db.table("users").get("42.name");     // "Bob"
```

### Default values and `undefined`

- absent key, no default → `undefined`
- absent key, default provided → the default
- explicitly stored `null`, no default → `undefined`, like an absent key (`get` maps a `null` answer to `undefined`; `has` returns `false`)

### Errors

Application errors inherit from `TypeError`, so
`.rejects.toThrow(TypeError)` works:

```ts
try {
  await db.add("user.name", 5); // "Alice" is not a number
} catch (e) {
  // HorizonDBOpError extends TypeError, message = server's message
}
```

Exported hierarchy:

| Class | Inherits | When |
|---|---|---|
| `HorizonDBOpError` | `TypeError` | server answered `type:"error"` |
| `HorizonDBAuthError` | `Error` | login refused |
| `HorizonDBTimeoutError` | `Error` | no answer within `requestTimeoutMs`; the message carries `(connection not ready)` when the wait was for a reconnect |
| `ConnectionClosedError` | `Error` | socket closed (or reset by the SDK, see below) with requests in flight |
| `InvalidTableNameError` | `TypeError` | table name rejected locally |

Set `requestTimeoutMs: 0` to disable per-request timers entirely (useful for
long-running benchmark pipelines that manage their own deadlines). When a
timeout is set, every packet also carries it as `timeoutMs`, so the server can
drop a request that waited longer than that before being handled instead of
executing it for a caller that already gave up (`deadline exceeded`).

### Session and connection resilience

If an operation fails with a *session* error while credentials are known, the
connection **re-authenticates once automatically** then replays the request.
Concurrent callers share a single login round trip (one session), and after a
reconnect the SDK re-authenticates **before** releasing waiting requests, so
they never race the login.

If the socket is silently dead (stuck server, half-open TCP link), a request
that times out while **nothing at all** was received during its whole window
makes the SDK drop the socket and reconnect with backoff (250 ms to 5 s);
in-flight requests are rejected with `ConnectionClosedError` instead of every
following request timing out. A merely slow server (responses still arrive)
never triggers this. Covered by the "server went silent" and "slow but
answering" tests.

---

## 4. Client-side security

- **Upstream SQL-injection defense**: table names are validated on the client
  (`[A-Za-z_][A-Za-z0-9_]{0,62}`) before anything is sent — defense in depth on
  top of server-side validation.
- Empty keys rejected immediately (local `TypeError`, zero round trips).
- Per-request timeout (default 10 s): never a ghost promise if the server dies;
  timers are cleaned up on resolution.

---

## 5. Tests (`bun test`)

Two levels, both **without external infrastructure**:

1. **Pure unit** (`utils.test.ts`): table/key validators, error `instanceof`
   chains.

2. **Behavioral against a mock** (`client.test.ts`):
   `MockHorizonServer` implements the protocol in memory on a random port
   (login/sessions, nested keys, TTL, exact Rust-server semantics including its
   quirks — e.g. `cache` deletes the root key after expiration).
   Coverage: full mirror of the reference suite (set/get/add/sub/delete/
   cache/push/pull/all/deleteAll/table), mutation return values, edge cases
   (creation via negative `sub`, deep nesting), 50 concurrent pipelined
   operations, bad credentials, timeout ("blackhole" server), mid-flight
   close, server restart (single re-login for many waiters), silent server
   (forced reconnect) versus slow server (no reconnect).

Maintenance notes:

- Request→response correlation via `crypto.randomUUID()`; unknown responses
  are silently ignored.
- Bun pitfall fixed in the mock: `fetch` must call `server.upgrade(request)`
  to accept the WebSocket handshake, otherwise connections fail.
- The mock uses `timer.unref()` so TTLs never block process exit.

---

## 6. Server-inherited behaviors (good to know)

These choices faithfully mirror the current Rust server:

- `cache(key, value, ttl)` deletes **the root key** after expiry
  (`temp.token` ⇒ all of `temp` disappears).
- `pull` removes **all** occurrences of the element, not just the first.
- `has` over an explicitly `null` value returns `false`.
- Arithmetic through a scalar intermediate is rejected by the server
  (`cannot set nested value on non-object`) — the mock mirrors this exactly.
- A nested `push`/`pull` on a key that has no document yet creates the
  object (`push("list.items", 1)` gives `{ "items": [1] }`); a top-level one
  creates the array.

If any of these semantics change on the Rust side, update
`tests/helpers/mock-server.ts` first: it is the executable specification of the
protocol.

---

## 7. Validation status

| Check | Result |
|---|---|
| `cd sdk && bun test` | 40 pass, 0 fail (~1.9 s) |
| `cd sdk && bun run typecheck` | clean (strict) |
| Real smoke against `ws://127.0.0.1:8080` (Rust binary) | set/get/add/table/cache-TTL/session-guard ✓ |
