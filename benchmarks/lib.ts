export interface BenchResult {
  scenario: string;
  operations: number;
  errors: number;
  wallMs: number;
  opsPerSec: number;
  avgUs: number;
  minUs: number;
  p50Us: number;
  p95Us: number;
  p99Us: number;
  maxUs: number;
}

export interface ScenarioContext {
  op<T>(promise: Promise<T>): Promise<T>;
}

const MAX_SAMPLES = 500_000;

export async function measure(
  scenario: string,
  body: (ctx: ScenarioContext) => Promise<void>,
): Promise<BenchResult> {
  const samples: number[] = [];
  let errors = 0;

  const ctx: ScenarioContext = {
    async op<T>(promise: Promise<T>): Promise<T> {
      const start = Bun.nanoseconds();
      try {
        return await promise;
      } catch {
        errors += 1;
        return undefined as T;
      } finally {
        if (samples.length < MAX_SAMPLES) {
          samples.push(Number(Bun.nanoseconds() - start) / 1_000);
        }
      }
    },
  };

  const wallStart = Bun.nanoseconds();
  await body(ctx);
  const wallMs = Number(Bun.nanoseconds() - wallStart) / 1e6;

  const sorted = [...samples].sort((a, b) => a - b);
  const totalUs = sorted.reduce((sum, value) => sum + value, 0);

  return {
    scenario,
    operations: sorted.length,
    errors,
    wallMs,
    opsPerSec: wallMs > 0 ? (sorted.length / wallMs) * 1000 : 0,
    avgUs: sorted.length > 0 ? totalUs / sorted.length : 0,
    minUs: (sorted[0] as number) ?? 0,
    p50Us: percentile(sorted, 50),
    p95Us: percentile(sorted, 95),
    p99Us: percentile(sorted, 99),
    maxUs: (sorted[sorted.length - 1] as number) ?? 0,
  };
}

export function percentile(samples: number[], p: number): number {
  if (samples.length === 0) return 0;
  const index = Math.min(
    samples.length - 1,
    Math.max(0, Math.ceil((p / 100) * samples.length) - 1),
  );
  return samples[index] as number;
}

function compact(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return value.toFixed(value >= 100 ? 0 : 2);
}

export function printResults(results: BenchResult[]): void {
  const header =
    "scenario".padEnd(30) +
    "ops".padStart(9) +
    "err".padStart(5) +
    "ops/s".padStart(10) +
    "avgµs".padStart(9) +
    "p50µs".padStart(9) +
    "p95µs".padStart(9) +
    "p99µs".padStart(9) +
    "maxµs".padStart(9);
  console.log(header);
  console.log("-".repeat(header.length));
  for (const row of results) {
    console.log(
      row.scenario.padEnd(30) +
        compact(row.operations).padStart(9) +
        String(row.errors).padStart(5) +
        compact(row.opsPerSec).padStart(10) +
        row.avgUs.toFixed(0).padStart(9) +
        row.p50Us.toFixed(0).padStart(9) +
        row.p95Us.toFixed(0).padStart(9) +
        row.p99Us.toFixed(0).padStart(9) +
        row.maxUs.toFixed(0).padStart(9),
    );
  }
}

export interface BenchOptions {
  url: string;
  username: string;
  password: string;
  table: string;
  profile: "quick" | "standard" | "full";
  concurrency: number;
  pipeline: number;
  requestTimeoutMs: number;
  scenarioCapMs: number;
  /** When set, the results are also written there as JSON. */
  jsonPath?: string;
}

export function parseOptions(argv: string[]): BenchOptions {
  const options: BenchOptions = {
    url: process.env.HORIZON_URL ?? "ws://127.0.0.1:8080",
    username: process.env.HORIZON_USER ?? "admin",
    password: process.env.HORIZON_PASS ?? "secret",
    table: "bench",
    profile: "standard",
    concurrency: 16,
    pipeline: 32,
    requestTimeoutMs: 10_000,
    scenarioCapMs: 25_000,
  };
  for (let index = 0; index < argv.length; index++) {
    const flag = argv[index];
    const next = argv[index + 1];
    switch (flag) {
      case "--url":
        options.url = next as string;
        index++;
        break;
      case "--user":
        options.username = next as string;
        index++;
        break;
      case "--pass":
        options.password = next as string;
        index++;
        break;
      case "--table":
        options.table = next as string;
        index++;
        break;
      case "--profile":
        if (next === "quick" || next === "standard" || next === "full") {
          options.profile = next;
        }
        index++;
        break;
      case "--concurrency":
        options.concurrency = Number(next);
        index++;
        break;
      case "--pipeline":
        options.pipeline = Number(next);
        index++;
        break;
      case "--timeout":
        options.requestTimeoutMs = Number(next);
        index++;
        break;
      case "--cap":
        options.scenarioCapMs = Number(next);
        index++;
        break;
      case "--json":
        options.jsonPath = next as string;
        index++;
        break;
      default:
        break;
    }
  }
  return options;
}

export function scale(options: BenchOptions): number {
  if (options.profile === "quick") return 0.25;
  if (options.profile === "full") return 3;
  return 1;
}
