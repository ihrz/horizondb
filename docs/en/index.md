---
title: HorizonDB
description: JSON key-value server over WebSocket. Memory above, PostgreSQL below.
hide:
  - navigation
  - toc
---

<div class="hz-hero" markdown>

![HorizonDB](assets/logo.svg)

<span class="hz-eyebrow">JSON key-value over WebSocket</span>

# HorizonDB

<p class="hz-tagline">Memory above, PostgreSQL below.</p>

<p class="hz-lead">The key-value server behind iHorizon. Reads and writes at memory speed, batched into PostgreSQL behind the scenes, spoken over one WebSocket from a zero-dependency Bun client.</p>

[Get started](getting-started.md){ .md-button .md-button--primary }
[Protocol reference](protocol.md){ .md-button }
[Benchmark results](https://ihrz.gitlab.io/db/benchmarks/){ .md-button }

</div>

## What it is

HorizonDB is a **JSON-oriented key-value database server** written in Rust
(edition 2024, async Tokio, `unsafe` forbidden). Clients talk to it over a
WebSocket with small JSON packets. Every read and every write is served from an
in-memory store; a background task writes the changed keys to PostgreSQL in
batches every `flush_interval_secs` (300 s by default), and a graceful shutdown
flushes everything first.

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

## Why

A Discord bot hammers a handful of large JSON documents (a guild, a member, a
counter) from many shards at once. A SQL round trip on every `add` or `push`
makes that slow; a pure in-memory store forgets everything on restart.
HorizonDB sits on the line between the two: the working set lives in memory,
the durable copy lives in plain PostgreSQL tables you already know how to
query, back up and restore.

## Key features

<div class="grid cards" markdown>

-   :material-white-balance-sunny:{ .lg .middle } **Memory above**

    ---

    Every operation is answered from memory. A `push` into a multi-megabyte
    document edits it in place under a per-key lock: documents are never
    deep-copied on the hot path.

-   :material-weather-sunset-down:{ .lg .middle } **The horizon**

    ---

    Dirty keys are batch-flushed to PostgreSQL (`UNNEST` upserts, `ANY`
    deletes) every `flush_interval_secs`, and on SIGINT / SIGTERM.

-   :material-database-outline:{ .lg .middle } **PostgreSQL below**

    ---

    One plain table per namespace: `id VARCHAR(255) PRIMARY KEY, value TEXT`.
    Keys load lazily on first access, single-flighted.

-   :material-memory:{ .lg .middle } **Bounded memory**

    ---

    Idle documents are packed to compact JSON text (about 10x smaller), and
    clean idle documents leave memory entirely. RAM follows the working set,
    not everything read since startup.

-   :material-lock-check-outline:{ .lg .middle } **No lost updates**

    ---

    Read-modify-write operations are serialized per key. Sixteen workers on one
    counter end at the exact sum, checked on every benchmark run.

-   :material-file-tree-outline:{ .lg .middle } **Nested keys**

    ---

    `user.profile.name` addresses inside one document per base key, up to 32
    levels deep. Reads copy only the sub-tree you asked for.

-   :material-reply-outline:{ .lg .middle } **Answers, not acks**

    ---

    `add` / `sub` return the new number, `delete` whether the key existed,
    `push` / `pull` the resulting array on request.

-   :material-shield-check-outline:{ .lg .middle } **Hardened by default**

    ---

    argon2id or constant-time auth, validated table names, bounded messages,
    per-connection backpressure, request deadlines, idle-link pings.

</div>

## Where to next

<div class="grid cards" markdown>

-   :material-rocket-launch-outline:{ .lg .middle } [**Getting started**](getting-started.md)

    Run a server and a PostgreSQL container with one script, then send your
    first request.

-   :material-tune-variant:{ .lg .middle } [**Configuration**](configuration.md)

    Every `[server]`, `[database]` and `[auth]` option, its default and its
    validation rule.

-   :material-sitemap-outline:{ .lg .middle } [**Architecture**](architecture.md)

    Write-behind, dirty generations, flush batching, cold loads and the three
    memory tiers.

-   :material-swap-horizontal:{ .lg .middle } [**Protocol**](protocol.md)

    Packet shapes, every operation with request and response, errors and
    limits.

-   :material-language-typescript:{ .lg .middle } [**TypeScript SDK**](sdk.md)

    The Bun client: options, methods, error classes, reconnection.

-   :material-server-outline:{ .lg .middle } [**Operations**](operations.md)

    systemd and pm2, graceful shutdown, the stats lines, memory tuning,
    backups.

-   :material-speedometer:{ .lg .middle } [**Benchmarks**](benchmarks.md)

    Method, reference numbers, memory measurements and the
    [interactive results](https://ihrz.gitlab.io/db/benchmarks/).

-   :material-help-circle-outline:{ .lg .middle } [**FAQ**](faq.md)

    Restarts, cold starts, memory usage, closed connections and other
    surprises.

</div>

!!! note "Project status"

    HorizonDB is part of the **iHorizon** project. It is a work in progress: the
    wire protocol may still change, so upgrade the server and the SDK together.

[:material-weather-sunset: Landing page](https://ihrz.gitlab.io/db/){ .md-button }
[:material-palette-outline: Brand kit](https://ihrz.gitlab.io/db/brand/){ .md-button }
[:fontawesome-brands-gitlab: Source code](https://gitlab.com/ihrz/db){ .md-button }
