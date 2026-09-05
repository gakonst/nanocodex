import { createNanocodexClient } from "../src/client.js";

class LatencyHistogram {
  static readonly resolutionMs = 0.5;
  static readonly maximumMs = 10 * 60_000;

  readonly buckets = new Uint32Array(
    Math.ceil(LatencyHistogram.maximumMs / LatencyHistogram.resolutionMs) + 1,
  );
  count = 0;

  record(latencyMs: number): void {
    const bucket = Math.min(
      this.buckets.length - 1,
      Math.ceil(latencyMs / LatencyHistogram.resolutionMs),
    );
    this.buckets[bucket] = (this.buckets[bucket] ?? 0) + 1;
    this.count += 1;
  }

  percentile(quantile: number): number {
    const target = Math.max(1, Math.ceil(this.count * quantile));
    let observed = 0;
    for (let index = 0; index < this.buckets.length; index += 1) {
      observed += this.buckets[index] ?? 0;
      if (observed >= target) {
        return Number((index * LatencyHistogram.resolutionMs).toFixed(1));
      }
    }
    return LatencyHistogram.maximumMs;
  }
}

const endpoint = process.env.RIVET_PUBLIC_ENDPOINT ?? "http://127.0.0.1:6420";
const actors = integerEnv("NANOCODEX_SOAK_ACTORS", 64, 1, 128);
const replaysPerActor = integerEnv("NANOCODEX_SOAK_REPLAYS", 256, 1, 2_048);
const concurrencyPerActor = integerEnv(
  "NANOCODEX_SOAK_CONCURRENCY_PER_ACTOR",
  8,
  1,
  64,
);
const waves = integerEnv("NANOCODEX_SOAK_WAVES", 5, 1, 50);
const keyspace = keyspaceEnv("NANOCODEX_SOAK_KEYSPACE", "local");
const client = createNanocodexClient(endpoint);
const handles = Array.from({ length: actors }, (_, index) =>
  client.nanocodex.getOrCreate([`soak-${keyspace}-${index}`]));
const latencies = new LatencyHistogram();
const throughputs: number[] = [];
let peakRss = 0;

await Promise.all(handles.map((handle) => handle.reset()));
for (let wave = 0; wave < waves; wave += 1) {
  const sessions = handles.map((handle, index) => {
    return {
      connection: handle.connect(),
      handle,
      request: { id: "seed", input: `Reply with exactly SOAK_${wave}_${index}` },
    };
  });

  try {
    await Promise.all(sessions.map(({ connection }) => connection.ready));
    const seedStarted = performance.now();
    const seeded = await Promise.allSettled(sessions.map(async ({ connection, request }) => {
      const result = await connection.turn(request);
      if (result.final_message !== request.input.slice("Reply with exactly ".length)) {
        throw new Error(`unexpected seed result: ${result.final_message}`);
      }
    }));
    throwFirstFailure(seeded);
    const seedMs = performance.now() - seedStarted;

    await Promise.all(sessions.map(({ connection }) => connection.dispose()));
    for (const session of sessions) session.connection = session.handle.connect();
    await Promise.all(sessions.map(({ connection }) => connection.ready));

    const expected = actors * replaysPerActor;
    const replayStarted = performance.now();
    let replayed = 0;
    for (let offset = 0; offset < replaysPerActor; offset += concurrencyPerActor) {
      const batchSize = Math.min(concurrencyPerActor, replaysPerActor - offset);
      const settled = await Promise.allSettled(sessions.flatMap(({ connection, request }) =>
        Array.from({ length: batchSize }, async () => {
          const started = performance.now();
          const result = await connection.turn(request);
          latencies.record(performance.now() - started);
          return result;
        })));
      throwFirstFailure(settled);
      if (settled.some((result) =>
        result.status === "fulfilled" && result.value.type !== "turn_completed")) {
        throw new Error("a terminal replay diverged from its committed result");
      }
      replayed += settled.length;
    }
    const replayMs = performance.now() - replayStarted;
    if (replayed !== expected) {
      throw new Error(`received ${replayed} terminal replays, expected ${expected}`);
    }
    const throughput = expected / (replayMs / 1_000);
    throughputs.push(throughput);
    const rss = process.memoryUsage().rss;
    peakRss = Math.max(peakRss, rss);
    console.log(JSON.stringify({
      actors,
      replay_ms: Math.round(replayMs),
      replay_ops_per_second: Math.round(throughput),
      seed_ms: Math.round(seedMs),
      status: "wave_ok",
      wave: wave + 1,
    }));
  } finally {
    const disposed = await Promise.allSettled(
      sessions.map(({ connection }) => connection.dispose()),
    );
    const reset = await Promise.allSettled(sessions.map(({ handle }) => handle.reset()));
    throwFirstFailure(disposed);
    throwFirstFailure(reset);
  }
}

console.log(JSON.stringify({
  actors_per_wave: actors,
  latency_ms: {
    p50: latencies.percentile(0.50),
    p95: latencies.percentile(0.95),
    p99: latencies.percentile(0.99),
  },
  peak_client_rss_mib: Math.round(peakRss / 1024 / 1024),
  keyspace,
  replay_ops: actors * replaysPerActor * waves,
  replay_ops_per_second: {
    best: Math.round(Math.max(...throughputs)),
    worst: Math.round(Math.min(...throughputs)),
  },
  status: "ok",
  waves,
}));

function throwFirstFailure(results: PromiseSettledResult<unknown>[]): void {
  const failure = results.find((result) => result.status === "rejected");
  if (failure) throw failure.reason;
}

function integerEnv(name: string, fallback: number, minimum: number, maximum: number): number {
  const value = Number(process.env[name] ?? fallback);
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} must be an integer from ${minimum} through ${maximum}`);
  }
  return value;
}

function keyspaceEnv(name: string, fallback: string): string {
  const value = process.env[name] ?? fallback;
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(value)) {
    throw new Error(`${name} must contain 1-64 letters, numbers, underscores, or hyphens`);
  }
  return value;
}
