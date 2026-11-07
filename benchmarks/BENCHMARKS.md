# Benchmarks

End-to-end performance suite for HorizonDB, executed through the official TypeScript SDK (`sdk/`) against a **real server** (no mocks). Every scenario measures genuine network + processing + PostgreSQL latency.

---

## 1. Quick start

```bash
# Prepares everything automatically: PostgreSQL container, config.toml, Rust build, detached server
./scripts/dev-up.sh

# Standard suite (~2 min, hard-bounded)
./scripts/bench.sh

# Variants
./scripts/bench.sh --quick          # reduced budgets (~4x shorter)
./scripts/bench.sh --full           # x3 budgets, payloads up to 512 KiB
```

Prerequisites: `bun` ≥ 1.4, `docker`/`podman`, Rust toolchain. The script fails with a clear message if the server is unreachable.

## 2. Options

| Option | Default | Description |
|---|---|---|
| `--profile <quick\|standard\|full>` | `standard` | Operation budget scale (×0.25 / ×1 / ×3) |
| `--concurrency N` | `16` | WebSocket connections in the client pool |
| `--pipeline N` | `32` | Simultaneous in-flight requests per connection |
| `--timeout ms` | `10000` | Per-request timeout (client side) |
| `--cap ms` | `25000` | **Wall-time budget per scenario** — past it, the scenario stops and its partials are reported |
| `--url`, `--user`, `--pass`, `--table` | env/local | Target |
| `--json path` | none | Also write the results (settings, environment, every scenario) as JSON. `BENCH_LABEL` names the machine in it. The website benchmarks page reads `benchmarks/results/latest.json`. |

Equivalent environment variables: `HORIZON_URL`, `HORIZON_USER`, `HORIZON_PASS`.
`BENCH_SKIP` environment variable: CSV list of scenarios to skip (e.g. `BENCH_SKIP="spike-burst/idle->flood"`).

### Guard rails
- **No scenario can stall the suite**: the time budget (`--cap`) is checked between operations.
- **Global verdict**: exit code `1` if any error is counted; coherence invariants (contended counters) fail the run immediately.
- Scenarios truncated by the cap are identifiable: their duration ≈ the cap.

## 3. Scenario catalog

| Scenario | Operation(s) | Access pattern | What it measures |
|---|---|---|---|
| `get-hot/sequential` | `get` | 1 connection, hot key (cache hit) | Floor latency: RTT + cache |
| `has/sequential` | `has` | 1 connection, hot key | Same, existence path |
| `set-small/sequential` | `set` | 1 connection, unique keys | Simple write throughput without concurrency |
| `set-small/pipelined` | `set` | 16 conns × depth 32, unique keys | Saturated write throughput (pipelining) |
| `add-counter/contented` | `add` | 16 workers on **a single key** | Optimistic-CAS behavior under maximum contention + **invariant**: final sum == number of increments |
| `nested-merge/contented` | nested `add` | 16 workers on one document | Transactional JSON merge (CAS) + **invariant** |
| `mixed-workload/pipelined` | get/set/add/has/delete | 512 in-flight requests, rotation | Behavior under realistic mixed load |
| `payload-Nkb/set+get` | `set`+`get` | 1 connection, large documents | Payload-size impact (1 / 64 / 512 KiB depending on profile) |
| `all-scan/preloaded` | `all` | Preloaded table, then scans | Full-scan cost (server-limited to 10 000 rows) |
| `session-churn/login+logout` | `login`/`logout` | Tight loops | Pure auth overhead (zero SQL) |
| `spike-burst/idle→flood` | `set` | 250 ms idle then flood | Absorption of a burst after idle |

All load keys use **unique bases** (`p123`, `m456`…) to model realistic KV traffic; only the `*contented` scenarios deliberately target one shared row to exercise the protected path.

## 4. Reading results

```
scenario                            ops  err     ops/s    avgµs    p50µs    p95µs    p99µs    maxµs
get-hot/sequential                 2.0k    0     18.1k       45       42       67       74      142
```

- **ops**: measured operations (sampling capped at 500 k)
- **err**: client-side errors (timeout, close, server rejection) — must be 0
- **ops/s**: average throughput over the scenario's wall duration
- **avg/p50/p95/p99/max µs**: per-operation latency distribution (includes pipeline wait)

Quick reading: low `p50` with high `p99` = latency tail (contention backoff or imminent timeout).

## 5. Reference results

Machine: Linux/NixOS laptop, consumer NVMe, PostgreSQL 16 containerized (host network, `synchronous_commit=off`), Bun 1.4, Rust debug build.
Profile `--quick`, 16 connections, pipeline 32:

```
scenario                            ops  err     ops/s    avgµs    p50µs    p95µs    p99µs    maxµs
---------------------------------------------------------------------------------------------------
get-hot/sequential                 2.0k    0     18.1k       45       42       67       74      142
has/sequential                     2.0k    0     20.1k       41       39       53       61      128
set-small/sequential               2.7k    0       109     9094     9041    16966    18542    29494 ⚠
set-small/pipelined                4.1k   62       126  3569523  3235505  8462285 10000085 10002181 ⚠
add-counter/contented              4.0k    0      1.9k     8526      545    31978   182455   676573
nested-merge/contented             1.6k    0      1.8k     8754      566    42599   182582   471873
mixed-workload/pipelined           9.4k    0       337  1440689  1225939  3524339  4200417  5316667 ⚠
payload-1kb/set+get               33.6k    0      1.3k      728      784     1121     1353     4205
payload-64kb/set+get               1.9k    0       311     3163     3059     4927     5604     6582
all-scan/preloaded                 1.0k    0        98     49690    17621   182356   662079   785260
session-churn/login+logout         1.0k    0     17.2k       47       43       66       89      450
spike-burst/idle->flood            3.0k    0       168  2528263  2073883  6215781  7366662  9372471 ⚠
```
⚠ = scenario bounded by the cap (known degradation, see §7).

### Highlights
- **Memory/auth paths: 17–20 k ops/s**, p50 ~40 µs.
- **Protected contention (CAS)**: ~1 900 tps writing concurrently to one key, **without losing a single write** (invariants verified on every run).
- **Payloads**: 1 300 ops/s at 1 KiB, 311 ops/s at 64 KiB (full set+get round trip).

## 6. Tuning and design

| Setting | Where | Effect |
|---|---|---|
| `synchronous_commit=off` | dev-only PG container (set by `dev-up.sh`) | Removes fsync wait (~1.9 ms/commit on this NVMe) so numbers measure application throughput rather than disk floor |
| CAS backoff (`db.rs`) | server | Exponential retry 1→25 ms on write conflict; guarantees progress without long lock queues |
| `max_message_size` | `[server]` config | Single message/frame ceiling (DoS protection); frame limit follows message limit |
| SQL pool (`[database]`) | config | `max_connections` should cover `--concurrency` to avoid acquisition queuing |

## 7. Known issue & tracking

**Degradation of plain `set` through the SDK inside long mixed runs** (⚠ scenarios above):
- The server processes every request in < 10 ms (zero "slow statement" logs logged, 100 % success rate);
- A raw WebSocket client (no SDK) reaches **48 k ops/s** against the same server and the same load shape;
- The interaction only appears when the Bun SDK chains: prior traffic on another connection → deep multi-socket burst.

Reproduction:
```bash
bun /tmp/opencode/repro.ts        # collapse (~60 ops/s)
bun /tmp/opencode/rawreal.ts      # same load, raw client → 48 k ops/s
```
Immediate workarounds: `--pipeline 4`, or run scenarios individually via `BENCH_SKIP`. Tracking: needs investigation on the Bun WebSocket read loop vs TCP window interplay.

## 8. CI integration

```yaml
- run: ./scripts/test.sh          # unit gates (Rust + SDK)
- run: ./scripts/bench.sh --quick # perf smoke: fails on errors or broken invariants
```
The suite is **bounded** (caps): guaranteed total duration, binary verdict usable in CI.
