import { randomUUID } from "node:crypto";

import { createNanocodexClient } from "../src/client.js";

const endpoint = process.env.RIVET_PUBLIC_ENDPOINT ?? "http://127.0.0.1:6420";
const client = createNanocodexClient(endpoint);
const session = client.nanocodex.getOrCreate([
  process.env.NANOCODEX_MULTICLIENT_ACTOR_KEY ?? "nanocodex-multiclient-smoke",
]);
await session.reset();
const first = session.connect();
const second = session.connect();
const accepted = [new Set<string>(), new Set<string>()];
const completed = [new Set<string>(), new Set<string>()];
const eventCounts = [0, 0];

for (const [index, connection] of [first, second].entries()) {
  connection.on("turnAccepted", (turn) => accepted[index]?.add(`${turn.id}:${turn.input}`));
  connection.on("turnCompleted", (turn) => completed[index]?.add(`${turn.id}:${turn.final_message}`));
  connection.on("agentEvent", () => {
    eventCounts[index] = (eventCounts[index] ?? 0) + 1;
  });
}
await Promise.all([first.ready, second.ready]);

const request = {
  id: randomUUID(),
  input: "Reply with exactly RIVET_SYNC_OK and nothing else.",
};
try {
  const started = await first.start(request);
  if (started.replayed) throw new Error("fresh synchronized turn was unexpectedly replayed");
  const result = await first.turn(request);
  if (result.final_message !== "RIVET_SYNC_OK") {
    throw new Error(`unexpected synchronized result: ${result.final_message}`);
  }
  const acceptedKey = `${request.id}:${request.input}`;
  const completedKey = `${request.id}:RIVET_SYNC_OK`;
  await waitUntil(() => accepted.every((turns) => turns.has(acceptedKey))
    && completed.every((turns) => turns.has(completedKey)));
  if (eventCounts.some((count) => count === 0)) {
    throw new Error(`one client missed the model event stream: ${eventCounts.join(",")}`);
  }
  console.log(JSON.stringify({
    actor_session_id: (await first.status()).session_id,
    accepted_clients: accepted.filter((turns) => turns.has(acceptedKey)).length,
    completed_clients: completed.filter((turns) => turns.has(completedKey)).length,
    event_counts: eventCounts,
    status: "ok",
  }));
} finally {
  await Promise.allSettled([first.dispose(), second.dispose()]);
  await session.reset();
}

async function waitUntil(predicate: () => boolean): Promise<void> {
  const deadline = Date.now() + 10_000;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("timed out waiting for synchronized client events");
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}
