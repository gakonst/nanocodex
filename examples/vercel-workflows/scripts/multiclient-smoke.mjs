import WebSocket from "ws";

const baseUrl = new URL(process.env.NANOCODEX_DEMO_URL ?? "http://127.0.0.1:3000");
const adminToken = process.env.NANOCODEX_ADMIN_TOKEN?.trim();
const create = await fetch(new URL("/api/sessions", baseUrl), {
  method: "POST",
  headers: adminToken ? { authorization: `Bearer ${adminToken}` } : {},
});
const created = await create.json();
if (!create.ok) throw new Error(created?.error?.message ?? `session creation failed with HTTP ${create.status}`);
const sessionId = created.session_id;
const clients = await Promise.all([openClient(sessionId), openClient(sessionId)]);
try {
  await Promise.all(clients.map((client) => client.waitFor((message) => message.type === "ready")));
  const turnId = crypto.randomUUID();
  const marker = `VERCEL_SANDBOX_OK_${crypto.randomUUID()}`;
  const input = [
    `Use sandbox_write_file to write exactly ${marker} to index.html.`,
    "Use sandbox_exec with cwd /workspace to verify both that /workspace/index.html contains that exact marker and that pwd is /vercel/sandbox.",
    "Use sandbox_start_process with cwd /workspace to run `python3 -m http.server 3000 --directory .`, waiting for ready_port 3000.",
    "Use sandbox_preview for port 3000, then use sandbox_read_file to verify index.html.",
    `After every tool succeeds, reply with exactly ${marker}.`,
  ].join(" ");
  const prompt = await fetch(
    new URL(`/api/sessions/${encodeURIComponent(sessionId)}/prompt`, baseUrl),
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: turnId, input }),
    },
  );
  if (!prompt.ok) {
    const body = await prompt.text();
    throw new Error(`prompt failed with HTTP ${prompt.status}: ${body}`);
  }
  const observations = await Promise.all(clients.map(async (client) => {
    await client.waitFor((message) => message.type === "turn_accepted" && message.id === turnId);
    await client.waitFor((message) => message.type === "event" && message.turn_id === turnId);
    const completed = await client.waitFor(
      (message) => message.type === "turn_completed" && message.id === turnId,
      180_000,
    );
    const turnEvents = client.events.filter((event) => event.type === "event" && event.turn_id === turnId);
    const toolCalls = new Map(turnEvents
      .filter((event) => event.event?.type === "tool.call")
      .map((event) => [String(event.event.payload?.call_id), event.event.payload?.tool]));
    const completedTools = new Set();
    let previewUrl;
    for (const event of turnEvents.filter((item) => item.event?.type === "tool.result")) {
      if (event.event.payload?.status !== "completed") continue;
      const tool = toolCalls.get(String(event.event.payload?.call_id));
      if (!tool) continue;
      completedTools.add(tool);
      if (tool === "sandbox_preview") {
        previewUrl = parseToolResult(event.event.payload?.result)?.url;
      }
    }
    return {
      final_message: completed.final_message,
      events: turnEvents.length,
      tools: [...completedTools],
      preview_url: previewUrl,
    };
  }));
  if (observations.some((result) => result.final_message.trim() !== marker)) {
    throw new Error(`unexpected terminal messages: ${JSON.stringify(observations)}`);
  }
  const requiredTools = [
    "sandbox_write_file",
    "sandbox_exec",
    "sandbox_start_process",
    "sandbox_preview",
    "sandbox_read_file",
  ];
  if (observations.some((result) => requiredTools.some((tool) => !result.tools.includes(tool)))) {
    throw new Error(`a synchronized client missed successful sandbox tool results: ${JSON.stringify(observations)}`);
  }
  const previewUrl = observations[0].preview_url;
  if (!previewUrl || observations.some((result) => result.preview_url !== previewUrl)) {
    throw new Error(`synchronized clients disagreed on the preview URL: ${JSON.stringify(observations)}`);
  }
  const preview = await fetchWithRetry(previewUrl, 30_000);
  const previewBody = await preview.text();
  if (!preview.ok || previewBody.trim() !== marker) {
    throw new Error(`sandbox preview failed with HTTP ${preview.status}: ${previewBody}`);
  }
  process.stdout.write(`${JSON.stringify({
    session_id: sessionId,
    accepted_clients: clients.length,
    completed_clients: observations.length,
    event_counts: observations.map((result) => result.events),
    tool_calls: requiredTools,
    preview_url: previewUrl,
    preview_status: preview.status,
    status: "ok",
  })}\n`);
} finally {
  for (const client of clients) client.close();
}

function parseToolResult(result) {
  if (typeof result !== "string") return result && typeof result === "object" ? result : undefined;
  try {
    return JSON.parse(result);
  } catch {
    return undefined;
  }
}

async function fetchWithRetry(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastResponse;
  let lastError;
  while (Date.now() < deadline) {
    try {
      lastResponse = await fetch(url, { cache: "no-store" });
      if (lastResponse.ok) return lastResponse;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  if (lastResponse) return lastResponse;
  throw lastError ?? new Error("sandbox preview did not become reachable");
}

async function openClient(sessionId) {
  const socketUrl = new URL("/api/ws", baseUrl);
  socketUrl.protocol = baseUrl.protocol === "https:" ? "wss:" : "ws:";
  socketUrl.searchParams.set("sessionId", sessionId);
  socketUrl.searchParams.set("startIndex", "0");
  const socket = new WebSocket(socketUrl);
  const records = [];
  const waiters = new Set();
  socket.on("message", (encoded) => {
    const record = JSON.parse(encoded.toString());
    if (record.type !== "stream_event") return;
    records.push(record.event);
    for (const waiter of waiters) waiter(record.event);
  });
  await new Promise((resolveOpen, rejectOpen) => {
    const timeout = setTimeout(() => rejectOpen(new Error("WebSocket open timed out")), 20_000);
    socket.once("open", () => {
      clearTimeout(timeout);
      resolveOpen();
    });
    socket.once("error", rejectOpen);
  });
  return {
    events: records,
    close: () => socket.close(),
    waitFor(predicate, timeoutMs = 30_000) {
      const existing = records.find(predicate);
      if (existing) return Promise.resolve(existing);
      return new Promise((resolveEvent, rejectEvent) => {
        const timeout = setTimeout(() => {
          waiters.delete(observe);
          rejectEvent(new Error("timed out waiting for synchronized workflow event"));
        }, timeoutMs);
        const observe = (event) => {
          if (!predicate(event)) return;
          clearTimeout(timeout);
          waiters.delete(observe);
          resolveEvent(event);
        };
        waiters.add(observe);
      });
    },
  };
}
