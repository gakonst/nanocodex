import { createNanocodexClient } from "../src/client.js";

const endpoint = process.env.RIVET_PUBLIC_ENDPOINT ?? "http://127.0.0.1:6420";
const actors = integerEnv("NANOCODEX_STRESS_ACTORS", 32, 1, 128);
const replaysPerActor = integerEnv("NANOCODEX_STRESS_REPLAYS", 128, 1, 2_048);
const concurrencyPerActor = integerEnv(
  "NANOCODEX_STRESS_CONCURRENCY_PER_ACTOR",
  8,
  1,
  64,
);
const keyspace = keyspaceEnv("NANOCODEX_STRESS_KEYSPACE", "local");
const client = createNanocodexClient(endpoint);
const handles = Array.from({ length: actors }, (_, index) =>
  client.nanocodex.getOrCreate([`stress-${keyspace}-${index}`]));
await Promise.all(handles.map((handle) => handle.reset()));
const sessions = handles.map((handle, index) => {
  return {
    connection: handle.connect(),
    handle,
    request: { id: "seed", input: `Reply with exactly ACTOR_${index}` },
  };
});

try {
  await Promise.all(sessions.map(({ connection }) => connection.ready));
  const seeded = await Promise.allSettled(sessions.map(async ({ connection, request }) => {
    const result = await connection.turn(request);
    if (result.final_message !== request.input.slice("Reply with exactly ".length)) {
      throw new Error(`unexpected seed result: ${result.final_message}`);
    }
  }));
  const seedFailure = seeded.find((result) => result.status === "rejected");
  if (seedFailure) throw seedFailure.reason;

  const expected = actors * replaysPerActor;
  const started = performance.now();
  let replayed = 0;
  for (let offset = 0; offset < replaysPerActor; offset += concurrencyPerActor) {
    const batchSize = Math.min(concurrencyPerActor, replaysPerActor - offset);
    const settled = await Promise.allSettled(sessions.flatMap(({ connection, request }) =>
      Array.from(
        { length: batchSize },
        () => connection.turn(request),
      )));
    const failure = settled.find((result) => result.status === "rejected");
    if (failure) throw failure.reason;
    if (settled.some((result) =>
      result.status === "fulfilled" && result.value.type !== "turn_completed")) {
      throw new Error("a terminal replay diverged from its committed result");
    }
    replayed += settled.length;
  }
  const elapsedMs = performance.now() - started;
  if (replayed !== expected) {
    throw new Error(`received ${replayed} terminal replays, expected ${expected}`);
  }
  console.log(JSON.stringify({
    actors,
    concurrency_per_actor: concurrencyPerActor,
    keyspace,
    terminal_replays: expected,
    elapsed_ms: Math.round(elapsedMs),
    replays_per_second: Math.round(expected / (elapsedMs / 1_000)),
    status: "ok",
  }));
} finally {
  await Promise.all(sessions.map(({ connection }) => connection.dispose()));
  await Promise.all(sessions.map(({ handle }) => handle.reset()));
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
