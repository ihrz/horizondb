---
title: Benchmarks
description: How HorizonDB is benchmarked, the reference numbers, the memory measurements, and how to reproduce them.
---

# Benchmarks

<div class="grid cards" markdown>

-   :material-chart-line:{ .lg .middle } **Interactive results**

    ---

    Every scenario, latency percentiles and the memory runs, charted from the
    latest published results.

    [:octicons-arrow-right-24: Open the benchmark page](https://ihrz.gitlab.io/db/benchmarks/){ .md-button .md-button--primary }

</div>

## Method { #method }

The suite (`benchmarks/bench.ts`) runs **end to end through the official
TypeScript SDK** against a real server and a real PostgreSQL: no mocks, so
every number includes the network, the WebSocket framing, the server and, when
it applies, PostgreSQL.

- A pool of client connections (16 by default), each pipelining requests
  (32 in flight per connection by default).
- Load keys use **unique base keys** to model ordinary key-value traffic; only
  the contended scenarios deliberately hit one shared document.
- **Bounded in time.** Each scenario has a wall-time budget (`--cap`, 25 s by
  default) checked between operations, so the suite always ends.
- **Binary verdict.** The run exits with status 1 if any error is counted, and
  the contended scenarios check an invariant (final counter == number of
  increments): a single lost update fails the run.

### Scenarios { #scenarios }

| Scenario | Operation(s) | Access pattern | What it measures |
| --- | --- | --- | --- |
| `get-hot/sequential` | `get` | 1 connection, hot key | Floor latency: round trip + memory |
| `has/sequential` | `has` | 1 connection, hot key | Same, existence path |
| `set-small/sequential` | `set` | 1 connection, unique keys | Write throughput without concurrency |
| `set-small/pipelined` | `set` | 16 connections × depth 32 | Saturated write throughput |
| `add-counter/contented` | `add` | 16 workers on **one key** | Per-key serialization under maximum contention, with the sum invariant |
| `nested-merge/contented` | nested `add` | 16 workers on one document | Nested in-place edits under contention, with the invariant |
| `mixed-workload/pipelined` | get/set/add/has/delete | 512 requests in flight | Realistic mixed load |
| `payload-Nkb/set+get` | `set` + `get` | 1 connection, large documents | Payload size impact (1 / 64 / 512 KiB depending on profile) |
| `all-scan/preloaded` | `all` | Preloaded table | Full scan cost (capped at 10 000 rows) |
| `session-churn/login+logout` | `login` / `logout` | Tight loop | Pure authentication overhead |
| `spike-burst/idle->flood` | `set` | 250 ms idle, then a flood | Absorbing a burst after idle |

### Reading a result line { #reading }

```text
scenario                            ops  err     ops/s    avgµs    p50µs    p95µs    p99µs    maxµs
get-hot/sequential                 2.0k    0     18.1k       45       42       67       74      142
```

- **ops**: measured operations (sampling capped at 500 k).
- **err**: client-side errors (timeout, close, server rejection). Must be 0.
- **ops/s**: average throughput over the scenario's wall time.
- **avg / p50 / p95 / p99 / max**: per-operation latency in µs, including the
  wait inside the pipeline.

A low p50 with a high p99 is a latency tail: contention or requests close to
their timeout. A duration close to the cap means the scenario was cut by its
budget.

## Reference results { #results }

From `benchmarks/BENCHMARKS.md`: Linux laptop, consumer NVMe, PostgreSQL 16 in
a container (host network, `synchronous_commit=off`), Bun 1.4, **debug build**
of the server, `--quick` profile, 16 connections, pipeline 32.

| Path | Throughput | p50 |
| --- | --- | --- |
| hot `get`, one connection | 18.1 k ops/s | 42 µs |
| `has`, one connection | 20.1 k ops/s | 39 µs |
| login + logout | 17.2 k ops/s | 43 µs |
| `add` on one key, 16 workers | 1.9 k ops/s | 545 µs |
| `set` + `get`, 1 KiB documents | 1.3 k ops/s | 784 µs |
| `set` + `get`, 64 KiB documents | 311 ops/s | 3 059 µs |
| raw WebSocket client, same server | 48 k ops/s | — |

The interactive page shows the latest full run, including the release build
results.

!!! info "Known issue: plain `set` through the SDK in long mixed runs"

    `set-small/pipelined`, `mixed-workload/pipelined`, `set-small/sequential`
    and `spike-burst` degrade in long runs through the Bun SDK, while the server
    answers every request in under 10 ms with a 100 % success rate, and a raw
    WebSocket client reaches 48 k ops/s with the same load shape. The
    interaction between the Bun WebSocket client and the TCP window is still
    under investigation. Workarounds: `--pipeline 4`, or run scenarios
    separately with `BENCH_SKIP`.

## Memory { #memory }

The memory policy (packing and eviction, see
[memory tiers](architecture.md#memory-tiers)) was measured with a release build
on **200 000 documents of about 1 KB (185 MB of JSON)**, guild-like objects
with settings, member objects, role ids and nested blocks. Timeline: load
everything, flush, 80 s without traffic, read 2 000 random keys back, rewrite
10 %, restart, read back. Every read was compared with the generated document:
**zero mismatches** in every configuration, before and after the restart.

Resident memory (RSS) after 80 s idle:

| Configuration | RSS after idle |
| --- | --- |
| main branch, no policy | **2 703 MiB**, and it stays there |
| eviction only | **60 MiB** |
| packing only (every document still in memory) | **417 MiB** |
| packing + eviction | **360 MiB** |

These runs used short thresholds (30 to 60 s) to fit the timeline; the
defaults are 120 s for packing and 900 s for eviction.

Cost of one document of ~940 bytes of JSON in memory:

| Form | Bytes per document |
| --- | --- |
| parsed `serde_json` tree (hot form) | ~11 200 |
| packed JSON text (idle form) | ~1 000 |

Parsing a packed document back: about **4 µs** for a 1 KB document, about
**7 ms** for an 870 KB document.

## Reproduce { #reproduce }

Prerequisites: Bun ≥ 1.4, Docker, a Rust toolchain.

```bash
./scripts/dev-up.sh            # PostgreSQL container, config.toml, build, detached server
./scripts/bench.sh             # standard suite (~2 min, hard-bounded)
./scripts/bench.sh --quick          # budgets ×0.25 (same as --profile quick)
./scripts/bench.sh --full           # budgets ×3, payloads up to 512 KiB
```

`bench.sh` calls `dev-up.sh` itself, so the server and its database are always
up. Note that `dev-up.sh` builds a **debug** binary; for release numbers, run a
release server and run the suite directly against it:
`bun benchmarks/bench.ts --url ws://host:port`.

| Option | Default | Meaning |
| --- | --- | --- |
| `--profile <quick\|standard\|full>` | `standard` | Budget scale (×0.25 / ×1 / ×3). |
| `--concurrency N` | `16` | WebSocket connections in the client pool. |
| `--pipeline N` | `32` | Requests in flight per connection. |
| `--timeout ms` | `10000` | Per-request timeout. |
| `--cap ms` | `25000` | Wall-time budget per scenario. |
| `--url`, `--user`, `--pass`, `--table` | env / local | Target. Environment: `HORIZON_URL`, `HORIZON_USER`, `HORIZON_PASS`. |
| `--json <path>` | none | Also write the results as JSON to `path`. |

`BENCH_SKIP` takes a comma-separated list of scenarios to skip (for example
`BENCH_SKIP="spike-burst/idle->flood"`). `BENCH_LABEL` names the machine in
the JSON output.

### Publishing results { #publishing }

The interactive page reads two files from the repository:

- `benchmarks/results/latest.json`: the output of `--json`, with the
  settings, the environment (Bun version, platform, `BENCH_LABEL`) and every
  scenario.
- `benchmarks/results/memory.json`: the memory runs above.

```bash
BENCH_LABEL="my laptop, release build" bun benchmarks/bench.ts --json benchmarks/results/latest.json
```

### In CI { #ci }

```yaml
- run: ./scripts/test.sh          # unit gates (Rust + SDK)
- run: ./scripts/bench.sh --profile quick # perf smoke: fails on errors or broken invariants
```
