import { test, expect, _electron as electron } from "@playwright/test";
import { createServer, type ServerResponse } from "node:http";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

test("long conversation stays responsive while streaming", async ({}, testInfo) => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-rendering-"));
  const emptyEnv = join(directory, ".env");
  await writeFile(emptyEnv, "");
  const agentId = "019a65fe-a456-7000-8000-000000000099";
  const events: Record<string, unknown>[] = [];
  const emit = (turn: string, data: Record<string, unknown>) => {
    const value = { cursor: String(events.length + 1), created_at: Date.now(), turn_id: turn, ...data };
    events.push(value);
    return value;
  };
  for (let i = 0; i < 100; i++) {
    const turn = `history-${i}`;
    emit(turn, { type: "turn_accepted", id: turn, input: `Review change ${i} and explain its behavior.` });
    emit(turn, { type: "turn_completed", id: turn, final_message: `### Change ${i}\n\n${"The application preserves your work and restores the conversation after reconnecting. ".repeat(6)}\n\n- Keep drafts in their own tab.\n- Show meaningful progress.\n\n\`\`\`ts\nconst ready = await connect();\n\`\`\`` });
  }
  emit("streaming", { type: "turn_accepted", id: "streaming", input: "Continue reviewing while I write my next request." });
  emit("streaming", { type: "event", event: { type: "assistant.delta", payload: { text: "Continuing the review. " } } });
  const watchers = new Set<ServerResponse>();
  const server = createServer((request, response) => {
    response.setHeader("content-type", "application/json");
    if (request.url?.includes("/events/history")) response.end(JSON.stringify({ data: events, latest_cursor: String(events.length), has_more: false }));
    else if (request.url?.includes("/events?")) {
      response.setHeader("content-type", "text/event-stream");
      response.write(": connected\n\n");
      watchers.add(response);
      request.on("close", () => watchers.delete(response));
    } else if (request.url === `/v1/agents/${agentId}`) response.end(JSON.stringify({ accepted_turns: 101, active_turns: ["streaming"], settings: { model: "gpt-5.6-sol", thinking: "high", reasoning_mode: "standard", fast_mode: false } }));
    else response.end(JSON.stringify({ data: [agentId], summaries: { [agentId]: { title: "Long conversation", created_at: 1, updated_at: Date.now(), turn_count: 101 } } }));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing test service address.");
  const application = await electron.launch({
    ...(process.env.NANOCODEX_TEST_EXECUTABLE ? { executablePath: process.env.NANOCODEX_TEST_EXECUTABLE, args: [] } : { args: [resolve("dist/main/index.js")] }),
    env: { ...process.env, NANOCODEX_ENV_FILE: emptyEnv, NANOCODEX_DESKTOP_DATA: join(directory, "app"), NANOCODEX_DESKTOP_TEST: "1", NANOCODEX_MANAGED_URL: `http://127.0.0.1:${address.port}`, NC_API_KEY: `ncx_live_${"p".repeat(12)}_${"q".repeat(43)}`, NANOCODEX_API_KEY: "" },
  });
  let timer: ReturnType<typeof setInterval> | undefined;
  try {
    const page = await application.firstWindow();
    await page.setViewportSize({ width: 1360, height: 850 });
    const errors: string[] = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.locator(".thread-link").filter({ hasText: "Long conversation" }).click();
    await expect(page.locator(".assistant-message")).toHaveCount(101);
    await expect.poll(() => watchers.size).toBeGreaterThan(0);
    timer = setInterval(() => {
      const event = emit("streaming", { type: "event", event: { type: "assistant.delta", payload: { text: "The next detail remains clear. " } } });
      for (const response of watchers) response.write(`id: ${event.cursor}\nevent: message\ndata: ${JSON.stringify(event)}\n\n`);
    }, 12);
    const composer = page.getByRole("textbox", { name: "Message Nanocodex" });
    const inputSamples: number[] = [];
    for (let i = 0; i < 12; i++) {
      const start = performance.now();
      await composer.fill(`My next request ${i} stays responsive during streaming.`);
      await expect(composer).toHaveValue(`My next request ${i} stays responsive during streaming.`);
      inputSamples.push(performance.now() - start);
    }
    const switchStarted = performance.now();
    await composer.press("Meta+t");
    await expect(composer).toHaveValue("");
    const newTabMs = performance.now() - switchStarted;
    await composer.press("Meta+2");
    await expect(composer).toHaveValue("My next request 11 stays responsive during streaming.");
    const metrics = { turns: 101, inputMedianMs: Math.round([...inputSamples].sort((a, b) => a - b)[6]), inputMaxMs: Math.round(Math.max(...inputSamples)), newTabMs: Math.round(newTabMs) };
    console.info("Long conversation responsiveness (HTTP/SSE fixture)", metrics);
    await testInfo.attach("long-conversation-performance.json", { body: JSON.stringify(metrics), contentType: "application/json" });
    expect(metrics.inputMaxMs).toBeLessThan(500);
    expect(metrics.newTabMs).toBeLessThan(500);
    expect(errors).toEqual([]);
  } finally {
    clearInterval(timer);
    await application.close();
    await new Promise<void>(resolve => { server.closeAllConnections(); server.close(() => resolve()); });
    await rm(directory, { recursive: true, force: true });
  }
});
