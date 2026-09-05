import { randomUUID } from "node:crypto";

import { createNanocodexClient } from "../src/client.js";

const endpoint = process.env.RIVET_PUBLIC_ENDPOINT ?? "http://127.0.0.1:6420";
const client = createNanocodexClient(endpoint);
const session = client.nanocodex.getOrCreate([
  process.env.NANOCODEX_SMOKE_ACTOR_KEY ?? "nanocodex-smoke",
]);
await session.reset();
const events = session.connect();
let eventCount = 0;
const sandboxToolCalls = new Map<string, string>();
const sandboxToolsCompleted = new Set<string>();
let previewPath: string | undefined;
let sandboxProcessId: number | undefined;
events.on("agentEvent", (event) => {
  eventCount += 1;
  if (process.env.NANOCODEX_SMOKE_TRACE === "1"
    && (event.type === "tool.call" || event.type === "tool.result")) {
    console.error(JSON.stringify({ type: event.type, payload: event.payload }));
  }
  if (event.type === "tool.call" && typeof event.payload.tool === "string") {
    sandboxToolCalls.set(String(event.payload.call_id), event.payload.tool);
  }
  if (event.type === "tool.result" && event.payload.status === "completed") {
    const tool = sandboxToolCalls.get(String(event.payload.call_id));
    if (tool) {
      sandboxToolsCompleted.add(tool);
      const result = parseToolResult(event.payload.result);
      if (tool === "sandbox_preview" && typeof result?.url === "string") {
        previewPath = result.url;
      }
      if (tool === "sandbox_start_process" && typeof result?.process_id === "number") {
        sandboxProcessId = result.process_id;
      }
    }
  }
});
await events.ready;

const firstRequest = {
  id: randomUUID(),
  input: "Reply with exactly EDGE_OK and nothing else.",
};
const started = performance.now();

try {
  const [first, duplicate] = await Promise.all([
    session.turn(firstRequest),
    session.turn(firstRequest),
  ]);
  if (first.final_message !== "EDGE_OK" || duplicate.final_message !== first.final_message) {
    throw new Error(`unexpected first turn: ${JSON.stringify(first)}`);
  }

  const replay = await session.turn(firstRequest);
  if (replay.final_message !== first.final_message) throw new Error("terminal replay changed its result");

  await session.unload();
  const unloaded = await session.status();
  if (unloaded.agent_loaded) throw new Error("unload left the WASM driver resident");

  const restored = await session.turn({
    id: randomUUID(),
    input: "What exact token did I ask you to return previously? Reply with only that token.",
  });
  if (restored.final_message !== "EDGE_OK") {
    throw new Error(`restored session lost history: ${restored.final_message}`);
  }
  const toolTurn = await awaitDurableTurn({
    id: randomUUID(),
    input: "Use sandbox_write_file to write exactly RIVET_SANDBOX_OK to index.html and to write server.mjs containing a Node HTTP server that reads /workspace/index.html and listens on 0.0.0.0:3000. Verify index.html with sandbox_exec and sandbox_read_file. Call sandbox_start_process with command `node`, args [`/workspace/server.mjs`], and ready_port 3000. After it reports the port ready, call sandbox_preview for port 3000. Reply with only the preview URL.",
  });
  const requiredTools = [
    "sandbox_write_file",
    "sandbox_exec",
    "sandbox_read_file",
    "sandbox_start_process",
    "sandbox_preview",
  ];
  if (requiredTools.some((tool) => !sandboxToolsCompleted.has(tool)) || !previewPath) {
    throw new Error(`sandbox tool proof failed: ${toolTurn.final_message}`);
  }
  const previewUrl = previewPath.startsWith("http://") || previewPath.startsWith("https://")
    ? previewPath
    : appendGatewayPath(await session.getGatewayUrl(), previewPath);
  const previewResponse = await fetch(previewUrl, { signal: AbortSignal.timeout(15_000) });
  const previewBody = await previewResponse.text();
  if (!previewResponse.ok || previewBody.trim() !== "RIVET_SANDBOX_OK") {
    throw new Error(`preview fetch failed (${previewResponse.status}): ${previewBody.slice(0, 256)}`);
  }
  const status = await session.status();
  console.log(JSON.stringify({
    actor_session_id: status.session_id,
    auth_mode: status.auth_mode,
    completed_turns: status.completed_turns,
    elapsed_ms: Math.round(performance.now() - started),
    events: eventCount,
    preview_url: previewUrl,
    tool_calls: requiredTools,
    restored: status.has_snapshot,
    status: "ok",
  }));
} finally {
  await cleanupSession();
}

async function awaitDurableTurn(request: { id: string; input: string }) {
  await session.start(request);
  let lastError: unknown;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      return await session.turn(request);
    } catch (error) {
      lastError = error;
      const status = await session.status();
      if (!status.active_turns.includes(request.id)) throw error;
    }
  }
  throw lastError;
}

async function cleanupSession(): Promise<void> {
  await events.dispose().catch(() => {});
  if (sandboxProcessId !== undefined) {
    await session.process.kill(sandboxProcessId).catch(() => {});
  }
  const status = await session.status().catch(() => undefined);
  await Promise.all((status?.active_turns ?? []).map((id) => session.cancel(id).catch(() => {})));
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const current = await session.status().catch(() => undefined);
    if (!current || current.active_turns.length === 0) break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  await session.reset().catch(() => {});
}

function parseToolResult(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "string") return undefined;
  try {
    const parsed: unknown = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : undefined;
  } catch {
    return undefined;
  }
}

function appendGatewayPath(gatewayUrl: string, path: string): string {
  const url = new URL(gatewayUrl);
  url.pathname = `${url.pathname.replace(/\/$/, "")}/request/${path.replace(/^\//, "")}`;
  return url.toString();
}
