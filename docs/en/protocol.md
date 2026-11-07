---
title: Protocol reference
description: The HorizonDB WebSocket protocol, packet shapes, every operation, errors and limits.
---

# Protocol reference

Any language that speaks WebSocket and JSON can be a HorizonDB client. This
page is the complete wire contract, taken from the server source.

!!! note "Work in progress"

    The protocol may still change between versions. Keep the server and your
    client in step.

## Transport { #transport }

- Plain WebSocket (`ws://host:port`). The server has **no TLS** of its own:
  keep it on a private network or behind a TLS-terminating proxy for `wss://`.
- One JSON object per **text** frame. Binary frames are ignored.
- Requests on one connection are handled **concurrently**. Responses carry the
  request's `id` and can arrive in any order.
- Up to 1 024 requests can be in flight per connection. Beyond that the server
  stops reading the socket until responses drain (TCP backpressure).
- When nothing has been received for 30 s the server sends a Ping; after
  120 s of silence it drops the connection. Any frame from the client,
  including the Pong a WebSocket library sends automatically, counts as
  activity.

## Request packet { #request }

```json
{
  "id": "42",
  "operation": "get",
  "sessionId": "0b7c2a4e-…",
  "table": "json",
  "key": "guild.42.settings.prefix",
  "defaultValue": "!",
  "timeoutMs": 10000
}
```

| Field | Type | Used by | Notes |
| --- | --- | --- | --- |
| `id` | **string**, required | all | Echoed in the response. Must be a string: a number fails to parse. |
| `operation` | string, required | all | One of the [operations](#operations). |
| `sessionId` | string | all but `login` | From `login`. |
| `table` | string | data operations | Default `json`. Must match `[A-Za-z_][A-Za-z0-9_]{0,62}`. |
| `key` | string | all but `all` and `deleteAll` | Base key plus optional dotted path, see [keys](#keys). |
| `value` | any JSON | `set`, `cache` | Missing means `null`. |
| `amount` | number | `add`, `sub` | Default `1`. Must be finite. |
| `element` | any JSON | `push`, `pull` | Missing means `null`. |
| `defaultValue` | any JSON | `get` | Returned when the key or path does not resolve. |
| `time` | integer, ms | `cache` | TTL. Default `60000`, from 1 ms to 30 days. |
| `returnValue` | boolean | `push`, `pull` | When `true`, the response carries the resulting array. |
| `login`, `password` | string | `login` | Credentials from `[auth]`. |
| `timeoutMs` | integer, ms | all | Client timeout, see [deadlines](#deadlines). |

Unknown fields are ignored. A field with the wrong type (for example a
negative `time`) makes the whole packet invalid.

## Response packet { #response }

Every response has the same four fields; the unused ones are `null`.

=== "Success"

    ```json
    { "id": "42", "type": "response", "data": "!", "error": null }
    ```

=== "Error"

    ```json
    { "id": "42", "type": "error", "data": null, "error": "invalid or expired session" }
    ```

=== "Unparseable request"

    ```json
    { "id": "unknown", "type": "error", "data": null, "error": "Invalid JSON: missing field `operation` at line 1 column …" }
    ```

    When the packet itself cannot be parsed (not JSON, missing `id` or
    `operation`, wrong field type) the server cannot know the `id`, so it
    answers with `"id": "unknown"`. Such a request is not counted in the stats.

## Sessions { #sessions }

1. Send `login` with the credentials. The answer carries a `sessionId`.
2. Put that `sessionId` in every other request.
3. The session stays valid while it is used: each authenticated request
   refreshes it, and it expires after `auth.session_timeout_secs` (3 600 s by
   default) **without** activity.
4. A session is not tied to the connection it was created on.
5. Sessions live in memory only: after a server restart every request answers
   `invalid or expired session` until the client logs in again.

## Keys and nesting { #keys }

A key is split on `.`: the first segment is the **base key** (one stored
document), the rest is a **path** inside that document.

| Rule | Error when broken |
| --- | --- |
| Present and not empty | `key is required` |
| No empty segment (`.a`, `a.`, `a..b`) | `malformed key` |
| At most 32 segments | `key nesting too deep` |
| At most 255 bytes, whole dotted key | `key too long` |

Path segments are always object keys: `list.0` addresses the property `"0"`,
not the first element of an array.

How nested writes treat what is already there:

- Missing intermediate objects are created.
- If the value **directly above** the leaf exists and is not an object, the
  write fails with `cannot set nested value on non-object`.
- A non-object found **higher** up the path is replaced by an object.

```text
document {"a": 5}
set   a.b    →  error: cannot set nested value on non-object
set   a.b.c  →  {"a": {"b": {"c": …}}}
```

## Operations { #operations }

The examples omit `sessionId` and `table` for readability.

### `login` { #op-login }

Opens a session. No `sessionId` needed.

```js
/* → */ { "id": "1", "operation": "login", "login": "admin", "password": "secret" }
/* ← */ { "id": "1", "type": "response", "data": { "success": true, "sessionId": "0b7c2a4e-…" }, "error": null }
```

Wrong credentials answer `invalid credentials`.

### `logout` { #op-logout }

Revokes the given `sessionId`. Always succeeds, even without a session.

```js
/* → */ { "id": "2", "operation": "logout", "sessionId": "0b7c2a4e-…" }
/* ← */ { "id": "2", "type": "response", "data": { "success": true }, "error": null }
```

### `get` { #op-get }

Reads a whole document or the sub-tree at a path. Only the requested part is
copied.

```js
/* → */ { "id": "3", "operation": "get", "key": "guild.42.settings" }
/* ← */ { "id": "3", "type": "response", "data": { "prefix": "!" }, "error": null }

/* → */ { "id": "4", "operation": "get", "key": "guild.42.missing", "defaultValue": 0 }
/* ← */ { "id": "4", "type": "response", "data": 0, "error": null }
```

`data` is the value, `defaultValue` when the key or path does not resolve, or
`null` when there is no default.

### `set` { #op-set }

Writes a value.

- Top-level key: replaces the whole document (and removes any TTL).
- Nested key: merges the value at the path, creating intermediate objects.

```js
/* → */ { "id": "5", "operation": "set", "key": "guild.42.settings.prefix", "value": "?" }
/* ← */ { "id": "5", "type": "response", "data": { "success": true }, "error": null }
```

### `delete` { #op-delete }

Removes a whole document (top-level key) or one leaf (nested key). `existed`
tells whether there was something to remove.

```js
/* → */ { "id": "6", "operation": "delete", "key": "guild.42.settings.prefix" }
/* ← */ { "id": "6", "type": "response", "data": { "success": true, "existed": true }, "error": null }
```

When a nested delete leaves the document as an empty object, the whole
document is deleted.

### `add` / `sub` { #op-add }

Adds or subtracts `amount` (default 1) and answers with the new number. A
missing key or path starts at 0, so `sub` on a missing key gives `-amount`.

```js
/* → */ { "id": "7", "operation": "add", "key": "guild.42.stats.messages", "amount": 5 }
/* ← */ { "id": "7", "type": "response", "data": { "success": true, "value": 5.0 }, "error": null }
```

- The target must be a number: anything else (including a stored `null`)
  fails with `cannot add to non-numeric value`.
- `amount` must be finite (`amount must be finite`).
- Numbers are handled as 64-bit floats: the result is written as a float
  (`5.0`), and integers above 2^53 lose precision.

### `push` { #op-push }

Appends `element` to an array.

- Top-level key: the document must be an array; a missing one starts as `[]`.
- Nested key: a missing document starts as `{}` and a missing array at the
  path is created.
- An existing non-array target fails with `target value is not an array`.

```js
/* → */ { "id": "8", "operation": "push", "key": "guild.42.mods", "element": "1234", "returnValue": true }
/* ← */ { "id": "8", "type": "response", "data": { "success": true, "value": ["1234"] }, "error": null }
```

Without `returnValue`, `data` is `{ "success": true }`.

### `pull` { #op-pull }

Removes **every** element equal to `element` (deep JSON equality). Same
`returnValue` option and same rules as `push`.

```js
/* → */ { "id": "9", "operation": "pull", "key": "guild.42.mods", "element": "1234", "returnValue": true }
/* ← */ { "id": "9", "type": "response", "data": { "success": true, "value": [] }, "error": null }
```

!!! note "Pulling from a missing array creates it"

    Like `push`, `pull` creates the array when it does not exist: pulling from
    a missing key stores an empty array.

### `has` { #op-has }

Tells whether the key or path holds a value. A stored `null` counts as absent.
`data` is a bare boolean.

```js
/* → */ { "id": "10", "operation": "has", "key": "guild.42.mods" }
/* ← */ { "id": "10", "type": "response", "data": true, "error": null }
```

### `all` { #op-all }

Returns the documents of the table as `{ id, value }` pairs sorted by `id`.
`key` is ignored.

```js
/* → */ { "id": "11", "operation": "all", "table": "json" }
/* ← */ { "id": "11", "type": "response", "data": [ { "id": "guild", "value": { "42": { "settings": { "prefix": "!" } } } } ], "error": null }
```

The result merges the PostgreSQL rows with the writes not flushed yet, and is
capped at **10 000 rows**. It is not a pagination API: a larger table is
truncated.

### `deleteAll` { #op-deleteall }

Empties the table: memory immediately, PostgreSQL (`DELETE FROM`) at the next
flush. The table itself is kept.

```js
/* → */ { "id": "12", "operation": "deleteAll", "table": "sessions_cache" }
/* ← */ { "id": "12", "type": "response", "data": { "success": true }, "error": null }
```

### `cache` { #op-cache }

Writes a value with a time to live, `time` in milliseconds (default 60 000,
at most 30 days). An expired key reads as absent immediately and is removed
from memory, then from PostgreSQL at the next flush.

```js
/* → */ { "id": "13", "operation": "cache", "key": "cooldown.42.daily", "value": true, "time": 86400000 }
/* ← */ { "id": "13", "type": "response", "data": { "success": true }, "error": null }
```

!!! warning "A TTL applies to the whole base document"

    With a nested key, the value is merged at the path but the TTL is set on
    the **base document**: `cache("temp.token", …)` deletes all of `temp` when
    it expires. Any later regular write anywhere in that document (`set`,
    `add`, `push`…) makes it permanent again. The deadline is kept in memory
    only: a cached key that was flushed comes back without a TTL if the server
    restarts before it expires.

## Deadlines { #deadlines }

A packet may carry `timeoutMs`, the client's own timeout. Before executing a
request the server compares it with the time the request has been waiting
since it was read (queued behind the in-flight cap, for example). If it waited
longer, the server answers:

```json
{ "id": "14", "type": "error", "data": null, "error": "deadline exceeded" }
```

and does no work for a caller that already gave up. Without `timeoutMs` (or
with `0`) the deadline is 10 s. The check only happens before execution: a
request that started always runs to completion.

## Errors { #errors }

Error messages are stable strings meant for programs as much as for people.

| Message | Cause |
| --- | --- |
| `invalid credentials` | `login` with a wrong user name or password. |
| `invalid or expired session` | Missing, unknown, revoked or expired `sessionId` (also after a server restart). |
| `Unknown operation: <name>` | `operation` is not one of the above (checked after the session). |
| `key is required` | `key` missing or empty. |
| `malformed key` | Empty segment in the key. |
| `key nesting too deep` | More than 32 segments. |
| `key too long` | Key longer than 255 bytes. |
| `table name must match [A-Za-z_][A-Za-z0-9_]{0,62}` | Invalid `table`. |
| `amount must be finite` | `amount` is NaN or infinite. |
| `cannot add to non-numeric value` | `add` / `sub` on a value that is not a number. |
| `cannot set nested value on non-object` | Nested write under a value that is not an object. |
| `target value is not an array` | `push` / `pull` on something that is not an array. |
| `ttl must be between 1ms and 30 days` | `cache` with `time` = 0 or above 30 days. |
| `deadline exceeded` | The request waited longer than its `timeoutMs` before running. |
| `Invalid JSON: <detail>` | The packet could not be parsed (answered with `id: "unknown"`). |
| `internal server error` | PostgreSQL or serialization failure. Details are only in the server log, at `error`. |

## Limits { #limits }

| Limit | Value | What happens beyond it |
| --- | --- | --- |
| Message and frame size | `server.max_message_size`, default 64 MiB, at most 256 MiB | The connection is closed, with every request in flight on it. Logged at `warn`. |
| Connections | `server.max_connections`, default 1 024 | New connections are dropped. |
| In-flight requests per connection | 1 024 | The server stops reading the socket (backpressure). |
| Request deadline | `timeoutMs`, default 10 s | `deadline exceeded`. |
| Key length | 255 bytes | `key too long`. |
| Key depth | 32 segments | `key nesting too deep`. |
| Table name | 63 bytes, `[A-Za-z_][A-Za-z0-9_]*` | Validation error. |
| `all` result | 10 000 rows (`MAX_LIST_ROWS`) | Truncated. |
| TTL | 1 ms to 30 days | Validation error. |
| Silent connection | Ping after 30 s, closed after 120 s | Connection closed. |
| Session | `auth.session_timeout_secs` of inactivity | `invalid or expired session`. |
