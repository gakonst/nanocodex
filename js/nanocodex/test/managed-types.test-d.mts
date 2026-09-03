import { Agent, ManagedError } from "nanocodex/managed";
import type {
  ManagedAgent,
  ManagedEvent,
  ManagedMemoryRecord,
  ManagedMemoryResult,
  ManagedOrganization,
  ManagedTurnResult,
} from "nanocodex/managed";
import type { Capabilities } from "../managed/Agent.mjs";

declare const apiKey: string;

const capabilities: Capabilities = {
  durable_turns: true,
  resumable_events: true,
  live_steer: true,
  live_cancel: true,
  workspace: "cloudflare-computer",
  execution_environments: true,
  execution_namespace: "cwd-root-v1",
  native_cross_mounts: false,
};
void capabilities;

async function checkManaged() {
  // @ts-expect-error the combined history search operation was removed.
  Agent.searchHistory;
  // @ts-expect-error thread terminology was replaced by sessions.
  Agent.findThreads;
  // @ts-expect-error thread terminology was replaced by sessions.
  Agent.readThread;
  const created: ManagedAgent = await Agent.create();
  const opened: ManagedAgent = Agent.open("0198d3f0-8844-7000-8000-000000000001");
  const serverAgent = await Agent.get("0198d3f0-8844-7000-8000-000000000001", {
    baseUrl: "https://managed.example",
    apiKey,
  });
  const agents: readonly ManagedAgent[] = await Agent.list({
    baseUrl: new URL("https://managed.example"),
    apiKey,
  });
  const turn = serverAgent.turn.prompt({
    input: [{ type: "text", text: "hello" }],
    idempotencyKey: "request-1",
  });
  const accepted: string = await turn.accepted();
  const result: ManagedTurnResult = await turn.result();
  await turn.result({ signal: new AbortController().signal });
  result.finalMessage;
  result.usage?.input_tokens;
  result.citations[0]?.sources[0]?.cursor;
  const found = await Agent.findSessions(
    { query: "remember", limit: 8 },
    { baseUrl: "https://managed.example", apiKey },
  );
  const read = await Agent.readSession(
    {
      session_id: found.results[0]!.session_id,
      turn_ids: [found.results[0]!.turn_id],
    },
    { baseUrl: "https://managed.example", apiKey },
  );
  read.turns[0]?.assistant;
  read.citations[0]?.sources[0]?.cursor;
  const memories: readonly ManagedMemoryRecord[] = await Agent.listMemories({
    baseUrl: "https://managed.example",
    apiKey,
  });
  await Agent.deleteMemory(memories[0]!.key, {
    baseUrl: "https://managed.example",
    apiKey,
  });
  const scanned = await Agent.memory(
    { operation: "scan", query: "deployment schedule", limit: 5 },
    { baseUrl: "https://managed.example", apiKey },
  );
  scanned.candidates[0]?.key.version;
  const stored = await Agent.memory(
    { operation: "put", content: "Deploy on Tuesday" },
    { baseUrl: "https://managed.example", apiKey },
  );
  const readMemory = await Agent.memory(
    { operation: "read", keys: [stored.memory.key] },
    { baseUrl: "https://managed.example", apiKey },
  );
  readMemory.memories[0]?.last_used_at_ms;
  await Agent.memory(
    { operation: "delete", key: stored.memory.key },
    { baseUrl: "https://managed.example", apiKey },
  );
  const memoryResult: ManagedMemoryResult = scanned;
  memoryResult.operation;
  const organization: ManagedOrganization = await Agent.getOrganization({
    baseUrl: "https://managed.example",
  });
  organization.rootTeam.name;
  await Agent.updateOrganization({ name: "Research" });
  await Agent.updateOrganization({ name: null });
  // @ts-expect-error memory operation names are closed.
  await Agent.memory({ operation: "list" });
  // @ts-expect-error memory keys require both positive-integer-shaped numeric fields.
  await Agent.memory({ operation: "read", keys: [{ id: 1 }] });
  // @ts-expect-error organization updates require a name.
  await Agent.updateOrganization({});
  for await (const event of serverAgent.events.watch({ cursor: result.cursor ?? "0" })) {
    const typed: ManagedEvent = event;
    typed.cursor;
    typed.data.type;
  }
  await turn.cancel();
  await created.delete();
  opened.id;
  await Agent.delete(accepted, { baseUrl: "https://managed.example", apiKey });
  new ManagedError("failed", "failed", { status: 500 });

  await Agent.create({
    // @ts-expect-error managed agents never accept provider credentials.
    providerApiKey: "sk-provider",
  });
  await Agent.create({
    // @ts-expect-error managed agents never accept runtime environments.
    env: {},
  });
}

void checkManaged;
