import { HorizonDB } from "../sdk/src";
import type { Json } from "../sdk/src/types";
import {
  measure,
  parseOptions,
  printResults,
  scale,
  type BenchOptions,
  type BenchResult,
  type ScenarioContext,
} from "./lib";

const KB = 1024;
const SKIP = new Set((process.env.BENCH_SKIP ?? "").split(",").filter(Boolean));

interface Pool {
  clients: HorizonDB[];
}

function paddedString(targetBytes: number): string {
  return "x".repeat(Math.max(0, targetBytes));
}

function payload(targetBytes: number): Json {
  return { pad: paddedString(Math.max(0, targetBytes - 24)), seq: 0 };
}

function sizeKbList(options: BenchOptions): number[] {
  if (options.profile === "full") return [1, 64, 512];
  return [1, 64];
}

async function connectPool(
  options: BenchOptions,
  size: number,
): Promise<Pool> {
  const clients: HorizonDB[] = [];
  for (let index = 0; index < size; index++) {
    clients.push(
      await HorizonDB.connect({
        url: options.url,
        username: options.username,
        password: options.password,
        table: options.table,
        requestTimeoutMs: options.requestTimeoutMs,
      }),
    );
  }
  return { clients };
}

let scenarioDeadline = Number.POSITIVE_INFINITY;

async function pump(
  ctx: ScenarioContext,
  workerCount: number,
  budget: number,
  produce: (index: number, worker: number) => Promise<unknown>,
): Promise<void> {
  let cursor = 0;
  const workers = Array.from({ length: workerCount }, async (_, worker) => {
    while (cursor < budget && Date.now() < scenarioDeadline) {
      const index = cursor;
      cursor += 1;
      await ctx.op(produce(index, worker));
    }
  });
  await Promise.all(workers);
}

async function measureLogged(
  options: BenchOptions,
  name: string,
  body: (ctx: ScenarioContext) => Promise<void>,
): Promise<BenchResult> {
  console.log(`[scenario] ${name}`);
  const started = Date.now();
  scenarioDeadline = started + options.scenarioCapMs;
  const result = await measure(name, body);
  console.log(
    `[scenario] ${name} done in ${Date.now() - started} ms (${result.errors} errors)`,
  );
  return result;
}

export async function runBench(options: BenchOptions): Promise<BenchResult[]> {
  const factor = scale(options);
  const results: BenchResult[] = [];

  const primary = await HorizonDB.connect({
    url: options.url,
    username: options.username,
    password: options.password,
    table: options.table,
    requestTimeoutMs: options.requestTimeoutMs,
  });
  await primary.deleteAll();

  const pool = await connectPool(options, options.concurrency);

  try {
    await measure("warmup", async (ctx) => {
      await pump(ctx, options.concurrency, 200, (index) =>
        pool.clients[index % pool.clients.length]!.set(`warm.${index}`, index),
      );
    });

    if (!SKIP.has("get-hot/sequential")) results.push(
      await measureLogged(options, "get-hot/sequential", async (ctx) => {
        await primary.set("hot.key", { value: 1 });
        const budget = Math.round(2000 * factor);
        await pump(ctx, 1, budget, () => primary.get("hot.key"));
      }),
    );

    if (!SKIP.has("has/sequential")) results.push(
      await measureLogged(options, "has/sequential", async (ctx) => {
        const budget = Math.round(2000 * factor);
        await pump(ctx, 1, budget, () => primary.has("hot.key"));
      }),
    );

    if (!SKIP.has("set-small/sequential")) results.push(
      await measureLogged(options, "set-small/sequential", async (ctx) => {
        const budget = Math.round(4000 * factor);
        await pump(ctx, 1, budget, (index) =>
          primary.set(`seq.${index}`, { index, flag: index % 2 === 0 }),
        );
      }),
    );

    if (!SKIP.has("set-small/pipelined")) results.push(
      await measureLogged(options, "set-small/pipelined", async (ctx) => {
        const budget = Math.round(8000 * factor);
        await pump(ctx, pool.clients.length * options.pipeline, budget, (index, worker) => {
          const client = pool.clients[worker % pool.clients.length] as HorizonDB;
          return client.set(`pipe.${index}`, index);
        });
      }),
    );

    const contentionWorkers = options.concurrency;
    const contentionEach = Math.round(250 * factor);
    let contendedTotal = -1;
    if (!SKIP.has("add-counter/contented")) results.push(
      await measureLogged(options, "add-counter/contented", async (ctx) => {
        await primary.set("shared.counter", 0);
        await pump(ctx, contentionWorkers, contentionWorkers * contentionEach, (_, worker) => {
          const client = pool.clients[worker % pool.clients.length] as HorizonDB;
          return client.add("shared.counter", 1);
        });
        contendedTotal = (await primary.get<number>("shared.counter")) ?? -1;
      }),
    );
    const expectedTotal = contentionWorkers * contentionEach;
    if (contendedTotal !== expectedTotal) {
      throw new Error(
        `contention invariant broken: counter=${contendedTotal} expected=${expectedTotal}`,
      );
    }

    if (!SKIP.has("nested-merge/contented")) results.push(
      await measureLogged(options, "nested-merge/contented", async (ctx) => {
        const workers = options.concurrency;
        const each = Math.round(100 * factor);
        await pump(ctx, workers, workers * each, (_, worker) => {
          const client = pool.clients[worker % pool.clients.length] as HorizonDB;
          return client.add("shared.doc.count", 1);
        });
        const merged = (await primary.get<number>("shared.doc.count")) ?? -1;
        if (merged !== workers * each) {
          throw new Error(
            `merge invariant broken: count=${merged} expected=${workers * each}`,
          );
        }
      }),
    );

    if (!SKIP.has("mixed-workload/pipelined")) results.push(
      await measureLogged(options, "mixed-workload/pipelined", async (ctx) => {
        const budget = Math.round(10000 * factor);
        const workers = Math.min(budget, pool.clients.length * options.pipeline);
        await pump(ctx, workers, budget, (index, worker) => {
          const client = pool.clients[worker % pool.clients.length] as HorizonDB;
          switch (index % 5) {
            case 0:
              return client.get("hot.key");
            case 1:
              return client.set(`mix.${index}`, { n: index });
            case 2:
              return client.add("mix.counter", 1);
            case 3:
              return client.has(`mix.${Math.max(0, index - 1)}`);
            default:
              return client.delete(`mix.${Math.max(0, index - 2)}`);
          }
        });
      }),
    );

    for (const sizeKb of sizeKbList(options)) {
      results.push(
        await measureLogged(options, `payload-${sizeKb}kb/set+get`, async (ctx) => {
          const document = payload(sizeKb * KB);
          const budget = Math.max(20, Math.round((60000 / sizeKb) * factor));
          await pump(ctx, 1, budget, (index) =>
            primary.set(`pl${index}`, document),
          );
          await pump(ctx, 1, budget, (index) =>
            primary.get(`pl${index}`),
          );
        }),
      );
    }

    const scanRows = Math.round(1000 * factor);
    if (!SKIP.has("all-scan/preloaded")) results.push(
      await measureLogged(options, "all-scan/preloaded", async (ctx) => {
        await pump(ctx, pool.clients.length, scanRows, (index) =>
          pool.clients[index % pool.clients.length]!.set(
            `scan.${index}`,
            index,
          ),
        );
        const reps = Math.max(3, Math.round(10 * factor));
        await pump(ctx, 1, reps, () => primary.all());
      }),
    );

    results.push(
      await measureLogged(options, "session-churn/login+logout", async (ctx) => {
        const rounds = Math.round(500 * factor);
        await pump(ctx, 1, rounds, () => primary.login(options.username, options.password));
        await pump(ctx, 1, rounds, () => primary.logout());
      }),
    );

    results.push(
      await measureLogged(options, "spike-burst/idle->flood", async (ctx) => {
        const burst = Math.round(3000 * factor);
        const workers = Math.min(burst, pool.clients.length * options.pipeline);
        await Bun.sleep(250);
        await pump(ctx, workers, burst, (index, worker) => {
          const client = pool.clients[worker % pool.clients.length] as HorizonDB;
          return client.set(`spike.${index}`, index);
        });
      }),
    );
  } finally {
    await primary.deleteAll().catch(() => {});
    for (const client of [...pool.clients, primary]) {
      client.close();
    }
  }

  return results.filter((result) => result.scenario !== "warmup");
}

const options = parseOptions(process.argv.slice(2));

try {
  console.log(
    `[bench] target=${options.url} profile=${options.profile} concurrency=${options.concurrency}`,
  );
  const results = await runBench(options);
  console.log("");
  printResults(results);
  const totalErrors = results.reduce((sum, row) => sum + row.errors, 0);
  const totalOps = results.reduce((sum, row) => sum + row.operations, 0);
  console.log("");
  console.log(
    `[bench] ${totalOps} operations measured, ${totalErrors} errors`,
  );
  if (totalErrors > 0) {
    console.error("[bench] FAILED: non-zero error count");
    process.exit(1);
  }
  console.log("[bench] PASSED");
} catch (error) {
  console.error("[bench] aborted:", (error as Error).message);
  console.error(
    "[bench] hint: run './scripts/dev-up.sh' to start postgres + server",
  );
  process.exit(1);
}
