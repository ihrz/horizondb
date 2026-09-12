# ihrzdb / HorizonDB

Key-value database server in Rust. Clients talk JSON over WebSocket, data lands in PostgreSQL, hot reads come from an in-memory TTL cache.

Status: work in progress, private project. The wire protocol may still change.

## Build and run

You need a PostgreSQL instance reachable from `config.toml`:

```bash
cp example.config.toml config.toml   # edit credentials before starting
cargo run --release
```

Or let the helper script handle the database container and the server:

```bash
./scripts/dev-up.sh      # postgres + build + start (logs in .server.log)
./scripts/dev-down.sh    # stop (--all also removes the container)
```

Default listen address is `ws://127.0.0.1:8080`. Shutdown on Ctrl-C or SIGTERM.

## Protocol

Requests are JSON packets correlated by `id`; answers carry `type: "response" | "error"`.
After `login`, every operation must include the returned `sessionId`.

Operations: `login`, `logout`, `get`, `set`, `delete`, `add`, `sub`, `push`,
`pull`, `has`, `all`, `deleteAll`, `cache`.

Keys are dot-nested (`user.profile.name`) and merge into one row per base key.
Mutations report their result so clients do not need a follow-up read:
`add`/`sub` answer `{ "success": true, "value": <new number> }`, `delete`
answers `{ "success": true, "existed": <bool> }`, and `push`/`pull` answer
with the resulting array under `value` when the request carries
`"returnValue": true`. See `WHAT.md` for the full semantics.

Sizing for production: `server.max_message_size` must be larger than your
largest document (a bigger message drops that client connection and every
request in flight on it), and `database.max_connections` should follow the
number of clients doing cold reads at startup. Requests may carry
`"timeoutMs"`; the server drops a request that already waited longer than
that before it could be handled (`"error": "deadline exceeded"`).

## TypeScript client

Bun-only SDK living in `sdk/` (no runtime dependencies):

```ts
import { HorizonDB } from "./sdk/src";

const db = await HorizonDB.connect({
  url: "ws://127.0.0.1:8080",
  username: "admin",
  password: "secret",
});

await db.set("user.name", "Alice");
console.log(await db.get("user.name")); // "Alice"

db.close();
```

Usage and error handling: [sdk/SDK_GUIDE.md](sdk/SDK_GUIDE.md).
Installing it in another project from this Git repository: see section below.

## Installing the SDK without npm

The package lives in the `sdk/` subfolder of this repository, so a plain git
dependency needs one small root manifest. Add this `package.json` at the
repository root and push:

```json
{
  "name": "ihrzdb",
  "private": true,
  "exports": { ".": "./sdk/src/index.ts" }
}
```

Then, in your bot's `package.json`:

```json
{
  "dependencies": {
    "horizondb": "git+https://gitlab.com/ihrz/ihrzdb.git#main"
  }
}
```

```bash
bun install
```

Works because Bun executes TypeScript directly; pin to a tag instead of `main`
for reproducible builds (`#v1.0.0`). For a private repository use SSH
(`git+ssh://git@gitlab.com/ihrz/ihrzdb.git#main`) or a deploy token in the URL.
The alternative is GitLab's built-in npm registry (`npm publish` with a
project-scoped token) if you prefer real semver releases over git pins.

## Development

```bash
./scripts/test.sh            # Rust tests + SDK typecheck + SDK tests
cargo clippy --all-targets   # expected clean
./scripts/bench.sh --quick   # bounded performance suite
```

Further reading: [WHAT.md](WHAT.md) (design notes),
[benchmarks/BENCHMARKS.md](benchmarks/BENCHMARKS.md) (numbers and method),
[sdk/SDK_GUIDE.md](sdk/SDK_GUIDE.md) (client details).
