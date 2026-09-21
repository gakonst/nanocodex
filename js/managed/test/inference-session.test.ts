import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi, afterEach } from "vitest";
import {
  InferenceSession, InferenceSessionRuntime, executeStatelessInferenceResponse, INFERENCE_KEY_ID_HEADER, INFERENCE_MAX_OUTPUT_TOKENS_HEADER,
  INFERENCE_MAX_BODY_BYTES, INFERENCE_TIMEOUT_MS, INFERENCE_PROBE_TIMEOUT_MS, normalizeInferencePolicy, validateInferenceRequest,
  type InferenceSessionEnv, type InferenceSessionMetadata,
} from "../src/inference-session";
import { OSS_MODEL, ROUTING_CANDIDATES, taskFamily } from "../src/thread-model-routing";
import { PROBE_OWNER } from "../src/provider-probe-schedule";

const owner = "test_inference_key_a", other = "test_inference_key_b";
const sessionId = "dca2a2b4-23e7-4fe4-a888-b1767105e382";
const candidate = `${OSS_MODEL}:medium`;
const completion = (text = "fixture answer") => ({ choices: [{ message: { content: text }, finish_reason: "stop" }],
  usage: { prompt_tokens: 12, completion_tokens: 3, total_tokens: 15 } });
const classification = (choice = candidate) => ({ answers: {
  candidate: { choice, confidence: 0.95 }, family: { choice: "other", confidence: 0.95 },
}, usage: { arbitrary_provider_payload: "private-router-echo" } });
function fixture(env?: Partial<InferenceSessionEnv>) {
  const persisted = new Map<string, unknown>();
  const commits: InferenceSessionMetadata[] = [];
  const storage = {
    async get(key: string) { return structuredClone(persisted.get(key)); },
    async put(key: string, value: unknown) { const copy = structuredClone(value); persisted.set(key, copy); commits.push(copy as InferenceSessionMetadata); },
  };
  const ctx = { id: { toString: () => "synthetic-do-id" }, storage } as unknown as DurableObjectState;
  const ai = vi.fn(async (model: string, _input: unknown): Promise<unknown> => model === "typesafe/jev" ? classification() : completion());
  const bindings = { AI: { run: ai }, ...env };
  let session = new InferenceSessionRuntime(ctx, bindings);
  const call = (method: string, path = "/session", body?: unknown, key: string | null = owner, extra: Record<string, string> = {}) => {
    const headers: Record<string, string> = { ...extra };
    if (key !== null) headers[INFERENCE_KEY_ID_HEADER] = key;
    return session.fetch(new Request(`https://private.invalid${path}`, {
      method, headers, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }));
  };
  return { ai, bindings, persisted, commits, call,
    create: (routing: unknown = { candidates: [candidate] }) => call("PUT", "/session", { key_id: owner, session_id: sessionId, routing }),
    restart: () => { session = new InferenceSessionRuntime(ctx, bindings); },
  };
}
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); });

describe("standalone inference session isolation", () => {
  it("pins before generation and persists only route/policy/key/counters", async () => {
    const f = fixture();
    expect((await f.create()).status).toBe(201);
    f.ai.mockImplementation(async (model, input) => {
      if (model === "typesafe/jev") return classification();
      expect(f.commits.at(-1)?.route).toMatchObject({ model: OSS_MODEL, backend: "workers_ai", thinking: "medium" });
      expect(input).toMatchObject({ messages: [{ role: "user", content: "private prompt" }], max_completion_tokens: 4096,
        reasoning_effort: "medium" });
      return completion("private generated answer");
    });
    const response = await f.call("POST", "/responses", { input: "private prompt" });
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ object: "response", model: OSS_MODEL, session_id: sessionId,
      buffering: "buffered", status: "completed", route: { backend: "workers_ai" } });
    expect(JSON.stringify(f.commits)).not.toContain("private");
    expect(f.commits.at(-1)?.counters).toEqual({ requests: 1, completed: 1, failed: 0 });
  });
  it("denies other keys and parent-like keys and never rebinds after deletion", async () => {
    const f = fixture(); await f.create();
    for (const key of [other, "parent_account_key", null]) {
      for (const method of ["GET", "DELETE", "POST"]) {
        const response = await f.call(method, method === "POST" ? "/responses" : "/session",
          method === "POST" ? { input: "hello" } : undefined, key);
        expect([403, 404]).toContain(response.status);
      }
    }
    expect((await f.call("PUT", "/session", { key_id: other })).status).toBe(409);
    expect(f.ai).not.toHaveBeenCalled();
    expect((await f.call("DELETE")).status).toBe(204);
    expect((await f.call("GET")).status).toBe(404);
    expect((await f.call("PUT", "/session", { key_id: other })).status).toBe(409);
    expect((await f.call("PUT", "/session", { key_id: owner })).status).toBe(409);
  });
  it("returns 409 on concurrent first input and deletion with exactly one pin", async () => {
    let release!: (value: unknown) => void;
    let entered!: () => void;
    const started = new Promise<void>(resolve => { entered = resolve; });
    const gate = new Promise(resolve => { release = resolve; });
    const f = fixture(); await f.create();
    f.ai.mockImplementation(async model => {
      if (model === "typesafe/jev") { entered(); return gate; }
      expect(f.commits.at(-1)?.route?.thinking).toBe("medium");
      return completion();
    });
    const first = f.call("POST", "/responses", { input: "first" });
    await started;
    expect((await f.call("POST", "/responses", { input: "second" })).status).toBe(409);
    expect((await f.call("DELETE")).status).toBe(409);
    release(classification());
    expect((await first).status).toBe(200);
    expect(f.ai.mock.calls.filter(([model]) => model === "typesafe/jev")).toHaveLength(1);
  });
  it("retains exact pin across upstream failure and restart, sanitizing all errors", async () => {
    const f = fixture(); await f.create();
    f.ai.mockImplementation(async model => {
      if (model === "typesafe/jev") return classification();
      throw Error("private-upstream-prompt secret-key private-output");
    });
    const failed = await f.call("POST", "/responses", { input: "private-prompt" });
    expect(failed.status).toBe(502);
    expect(await failed.text()).toBe('{"error":{"code":"inference_failed"}}');
    const pinned = f.commits.at(-1)!.route;
    f.restart(); f.ai.mockImplementation(async () => completion());
    expect((await f.call("POST", "/responses", { input: "complete new history" })).status).toBe(200);
    expect(f.commits.at(-1)?.route).toEqual(pinned);
    expect(f.ai.mock.calls.filter(([model]) => model === "typesafe/jev")).toHaveLength(1);
    expect(JSON.stringify(f.commits)).not.toContain("private");
    expect(f.commits.at(-1)?.counters).toEqual({ requests: 2, completed: 1, failed: 1 });
    const count = f.ai.mock.calls.length;
    expect((await f.call("POST", "/responses", { input: "x", reasoning: { effort: "high" } })).status).toBe(409);
    expect((await f.call("POST", "/responses", { input: "x", model: "gpt-6-astra" })).status).toBe(409);
    expect((await f.call("POST", "/responses", { input: "x", routing: {} })).status).toBe(400);
    expect(f.ai).toHaveBeenCalledTimes(count);
  });
  it("excludes ChatGPT in classifier/fallback, and rejects legacy routing", async () => {
    const f = fixture();
    expect((await f.create({ strategy: "legacy" })).status).toBe(400);
    expect((await f.create({ low_confidence_fallback: "frontier" })).status).toBe(201);
    f.ai.mockImplementation(async (model, input) => {
      if (model !== "typesafe/jev") return completion();
      const state = JSON.parse((input as { state: string }).state);
      expect(state.candidates.every((c: { backend: string }) => c.backend === "workers_ai")).toBe(true);
      return classification("gpt-6-astra:high");
    });
    const response = await f.call("POST", "/responses", { input: "hello" });
    expect(response.status).toBe(200);
    expect((await response.json() as InferenceSessionMetadata).route?.backend).toBe("workers_ai");
    expect(normalizeInferencePolicy().candidates?.every(id => ROUTING_CANDIDATES.find(c => c.id === id)?.backend !== "chatgpt")).toBe(true);
    expect(() => normalizeInferencePolicy({ candidates: ["gpt-6-astra:high"] })).toThrow("no_inference_candidates");
  });
  it("uses only fixed gateway endpoints and never falls back when a secret disappears", async () => {
    const choice = "openrouter:openai/gpt-6-astra:high";
    const send = vi.fn(async (..._args: Parameters<typeof fetch>) => Response.json(completion()));
    vi.stubGlobal("fetch", send);
    const f = fixture({ OPENROUTER_API_KEY: "synthetic-deployment-key" });
    await f.create({ candidates: [choice] }); f.ai.mockResolvedValue(classification(choice));
    expect((await f.call("POST", "/responses", { input: "hello" })).status).toBe(200);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0]).toMatchObject(["https://openrouter.ai/api/v1/chat/completions", {
      method: "POST", redirect: "manual", headers: { authorization: "Bearer synthetic-deployment-key" },
    }]);
    expect(f.ai).toHaveBeenCalledTimes(1);
    delete f.bindings.OPENROUTER_API_KEY; f.restart();
    expect((await f.call("POST", "/responses", { input: "retry full history" })).status).toBe(502);
    expect(send).toHaveBeenCalledTimes(1); expect(f.ai).toHaveBeenCalledTimes(1);
    expect(f.commits.at(-1)?.route?.backend).toBe("openrouter");
    expect(JSON.stringify(f.commits)).not.toContain("synthetic-deployment-key");
  });
  it("never accesses account, connector, memory, hand or subscription bindings", async () => {
    const forbidden = new Proxy({}, { get() { throw new Error("account capability accessed"); } });
    const f = fixture({ ACCOUNT: forbidden, CONNECTORS: forbidden, HANDS: forbidden, MEMORY: forbidden,
      CHATGPT: forbidden } as Partial<InferenceSessionEnv>);
    const network = vi.fn(() => { throw Error("unexpected network access"); }); vi.stubGlobal("fetch", network);
    await f.create();
    expect((await f.call("POST", "/responses", { input: "hello" })).status).toBe(200);
    expect(network).not.toHaveBeenCalled();
    expect(f.ai.mock.calls.map(([model]) => model)).toEqual(["typesafe/jev", OSS_MODEL]);
  });
});

describe("strict Responses boundary", () => {
  it.each(["web_search", "web_search_preview", "file_search", "computer", "computer_use_preview", "code_interpreter", "mcp", "tool_search", "namespace"])("rejects server tool %s before routing", async type => {
    const f = fixture(); await f.create();
    expect((await f.call("POST", "/responses", { input: "x", tools: [{ type, name: "unsafe" }] })).status).toBe(400);
    expect(f.ai).not.toHaveBeenCalled();
  });
  it.each([
    { previous_response_id: "resp_other" }, { previous_response_id: null }, { credentials: {} },
    { account_id: "other" }, { api_key: "secret" }, { base_url: "https://attacker.invalid" }, { headers: {} },
    { context_management: [] }, { background: true }, { store: true }, { metadata: { arbitrary: "x" } },
    { text: { format: { type: "json_schema", schema: {} } } },
    { tools: [{ type: "function", name: "f", strict: true }] },
    { input: [{ type: "configuration_update", reasoning: { effort: "high" } }] },
    { input: [{ type: "additional_tools", tools: [{ type: "mcp" }] }] },
    { input: [{ role: "user", content: [{ type: "input_image", image_url: "https://example.invalid/a" }] }] },
    { tools: [{ type: "function", name: "x", handler: "execute" }] },
  ])("rejects unsupported or authority-bearing fields %#", async extra => {
    const f = fixture(); await f.create();
    expect((await f.call("POST", "/responses", { input: "x", ...extra })).status).toBe(400);
    expect(f.ai).not.toHaveBeenCalled();
  });
  it("bounds input/body bytes, including multibyte strings and absent content-length", async () => {
    const f = fixture(); await f.create();
    expect((await f.call("POST", "/responses", { input: "😀".repeat(8192) })).status).toBe(413);
    expect((await f.call("POST", "/responses", { input: "x", instructions: "a".repeat(32768) })).status).toBe(413);
    expect((await f.call("POST", "/responses", { input: "a".repeat(INFERENCE_MAX_BODY_BYTES) })).status).toBe(413);
    expect((await f.call("POST", "/responses", { input: "x" }, owner,
      { "content-length": String(INFERENCE_MAX_BODY_BYTES + 1) })).status).toBe(413);
    expect(f.ai).not.toHaveBeenCalled();
  });
  it("enforces trusted token limit and validates full tool history before pinning", async () => {
    const f = fixture(); await f.create();
    const header = { [INFERENCE_MAX_OUTPUT_TOKENS_HEADER]: "32" };
    expect((await f.call("POST", "/responses", { input: "x", max_output_tokens: 33 }, owner, header)).status).toBe(400);
    expect((await f.call("POST", "/responses", { input: "x", max_output_tokens: 4097 })).status).toBe(400);
    expect((await f.call("POST", "/responses", { input: "x" }, owner,
      { [INFERENCE_MAX_OUTPUT_TOKENS_HEADER]: "4097" })).status).toBe(403);
    expect(() => validateInferenceRequest({ input: [{ type: "function_call_output", call_id: "x", output: "oops" }] })).toThrow("invalid_tool_history");
    expect(() => validateInferenceRequest({ input: [{ type: "function_call", call_id: "x", name: "f", arguments: "{}" }] })).toThrow("invalid_tool_history");
    expect(f.ai).not.toHaveBeenCalled();
    expect((await f.call("POST", "/responses", { input: "x" }, owner, header)).status).toBe(200);
    expect(f.ai.mock.calls[1]?.[1]).toMatchObject({ max_completion_tokens: 32 });
  });
  it.each([false, true])("returns canonical Responses format with honest buffered stream=%s", async stream => {
    const f = fixture(); await f.create();
    const response = await f.call("POST", "/responses", { input: "hello", stream });
    expect(response.headers.get("x-nanocodex-inference-buffering")).toBe("buffered");
    expect(response.headers.get("x-nanocodex-session-id")).toBe(sessionId);
    expect(response.headers.get("x-nanocodex-provider")).toBe("workers_ai");
    expect(response.headers.get("x-nanocodex-model")).toBe(OSS_MODEL);
    expect(response.headers.get("x-nanocodex-thinking")).toBe("medium");
    expect(response.headers.get("cache-control")).toBe("no-store");
    if (stream) {
      expect(response.headers.get("content-type")).toContain("text/event-stream");
      const body = await response.text();
      expect(body).toContain("event: response.output_text.delta");
      const terminal = body.split("\n").filter(line => line.startsWith("data: ")).map(line => JSON.parse(line.slice(6)))
        .find(event => event.type === "response.completed");
      expect(terminal.response).toMatchObject({ model: OSS_MODEL, session_id: sessionId, buffering: "buffered" });
    } else {
      expect(response.headers.get("content-type")).toContain("application/json");
      expect(await response.json()).toMatchObject({ status: "completed", output: [{ type: "message", content: [{ text: "fixture answer" }] }],
        usage: { input_tokens: 12, output_tokens: 3, total_tokens: 15 } });
    }
  });
  it.each(["function", "custom"])("returns caller-defined %s calls passively and accepts complete replay", async type => {
    const f = fixture(); await f.create();
    f.ai.mockImplementation(async model => model === "typesafe/jev" ? classification() : {
      choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [
        { id: "call_fixture", type: "function", function: { name: "tool_0", arguments: type === "custom" ? '{"input":"caller-code"}' : '{"value":1}' } },
      ] } }],
    });
    const response = await f.call("POST", "/responses", { input: "hello", tools: [{ type, name: "caller_tool" }] });
    const body = await response.json() as { output: Record<string, unknown>[] };
    expect(response.status).toBe(200);
    expect(body.output[0]).toMatchObject({ type: type === "custom" ? "custom_tool_call" : "function_call", name: "caller_tool", call_id: "call_fixture" });
    expect(f.ai).toHaveBeenCalledTimes(2);
    f.ai.mockResolvedValue(completion());
    const next = await f.call("POST", "/responses", { input: [
      { role: "user", content: "hello" }, ...body.output,
      { type: type === "custom" ? "custom_tool_call_output" : "function_call_output", call_id: "call_fixture", output: "caller executed it" },
    ] });
    expect(next.status).toBe(200); expect(f.ai).toHaveBeenCalledTimes(3);
  });
  it("preserves incomplete responses", async () => {
    const f = fixture(); await f.create();
    f.ai.mockImplementation(async model => model === "typesafe/jev" ? classification() : {
      choices: [{ finish_reason: "length", message: { content: "partial" } }],
    });
    const response = await f.call("POST", "/responses", { input: "hello" });
    expect(await response.json()).toMatchObject({ status: "incomplete", incomplete_details: { reason: "max_output_tokens" } });
  });
  it("times out at 120s while retaining the persisted route", async () => {
    vi.useFakeTimers();
    const f = fixture(); await f.create();
    let started!: () => void;
    const entered = new Promise<void>(resolve => { started = resolve; });
    f.ai.mockImplementation(async model => {
      if (model === "typesafe/jev") return classification();
      started(); return new Promise(() => {});
    });
    const pending = f.call("POST", "/responses", { input: "hello" });
    await entered; await vi.advanceTimersByTimeAsync(INFERENCE_TIMEOUT_MS);
    const response = await pending;
    expect(response.status).toBe(504);
    expect(await response.text()).toBe('{"error":{"code":"inference_timeout"}}');
    expect(f.commits.at(-1)?.route).toMatchObject({ backend: "workers_ai", thinking: "medium" });
    expect(f.commits.at(-1)?.counters.failed).toBe(1);
  });
});

it("commits the route to real Durable Object SQLite storage and restores it with the same key", async () => {
  const bindings = env as unknown as { NANOCODEX_INFERENCE_SESSIONS: DurableObjectNamespace<InferenceSession> };
  const stub = bindings.NANOCODEX_INFERENCE_SESSIONS.getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (_object, ctx) => {
    const ai = vi.fn(async (model: string) => model === "typesafe/jev" ? classification() : completion());
    const runtime = new InferenceSession(ctx, { AI: { run: ai } });
    const req = (method: string, path: string, body?: unknown) => new Request("https://private.invalid" + path, {
      method, headers: { [INFERENCE_KEY_ID_HEADER]: owner }, ...(body ? { body: JSON.stringify(body) } : {}),
    });
    expect((await runtime.fetch(req("PUT", "/session", { key_id: owner, session_id: sessionId, routing: { candidates: [candidate] } }))).status).toBe(201);
    expect((await runtime.fetch(req("POST", "/responses", { input: "private real storage prompt" }))).status).toBe(200);
    const stored = [...await ctx.storage.list()];
    expect(JSON.stringify(stored)).not.toContain("private");
    const restarted = new InferenceSession(ctx, { AI: { run: ai } });
    const meta = await restarted.fetch(req("GET", "/session"));
    expect(await meta.json()).toMatchObject({ id: sessionId, route: { backend: "workers_ai", thinking: "medium" } });
    expect((await restarted.fetch(req("POST", "/responses", { input: "full history" }))).status).toBe(200);
    expect(ai.mock.calls.filter(([model]) => model === "typesafe/jev")).toHaveLength(1);
  });
});

it("aborts a stalled request body at the same 120s deadline without pinning", async () => {
  vi.useFakeTimers();
  const persisted = new Map<string, unknown>();
  const storage = { async get(key: string) { return persisted.get(key); }, async put(key: string, value: unknown) { persisted.set(key, value); } };
  const ctx = { storage, id: { toString: () => "fixture" } } as unknown as DurableObjectState;
  const ai = vi.fn();
  const runtime = new InferenceSessionRuntime(ctx, { AI: { run: ai } });
  let bodyRead!: () => void;
  const started = new Promise<void>(resolve => { bodyRead = resolve; });
  const cancel = vi.fn();
  const body = new ReadableStream<Uint8Array>({ pull() { bodyRead(); }, cancel });
  const pending = runtime.fetch(new Request("https://private.invalid/session", { method: "PUT", body }));
  await started;
  await vi.advanceTimersByTimeAsync(INFERENCE_TIMEOUT_MS);
  expect((await pending).status).toBe(504);
  expect(cancel).toHaveBeenCalledTimes(1);
  expect(ai).not.toHaveBeenCalled();
  expect(persisted.size).toBe(0);
});

it("restricts first-input reasoning to eligible candidates and then pins the chosen effort", async () => {
  const f = fixture(); await f.create({});
  f.ai.mockImplementation(async (model, input) => {
    if (model !== "typesafe/jev") return completion();
    const state = JSON.parse((input as { state: string }).state);
    expect(state.candidates.every((c: { thinking: string }) => c.thinking === "low")).toBe(true);
    return classification(`${OSS_MODEL}:low`);
  });
  expect((await f.call("POST", "/responses", { input: "hello", reasoning: { effort: "low" } })).status).toBe(200);
  expect(f.commits.at(-1)?.route?.thinking).toBe("low");
  expect((await f.call("POST", "/responses", { input: "full history", reasoning: { effort: "medium" } })).status).toBe(409);
});

function probe(effort = "medium", ttft = 90, patch: Record<string, unknown> = {}) {
  const now = Date.now();
  return { backend: "workers_ai", model: OSS_MODEL, effort, source: "probe", scope: "deployment_global", workerColo: null,
    signalKind: "context_only_not_completion_probability", usable: true,
    sampleCount: 3, successCount: 3, censoredCount: 0, lastObservedAt: now, windowMs: 300_000,
    fullResponseP50Ms: 3000, fullResponseEwmaMs: 3000, generationTtftP50Ms: ttft, generationTtftEwmaMs: ttft,
    generationTtftSampleCount: 3, lastTtftObservedAt: now, ...patch };
}

describe("trusted deployment probe context", () => {
  it("supplies matched global TTFT only before first pin; reversed latency cannot reselect after restart", async () => {
    let fastEffort = "medium";
    const snapshot = vi.fn(async () => [probe("medium", fastEffort === "medium" ? 90 : 900),
      probe("high", fastEffort === "high" ? 90 : 900)]);
    const getByName = vi.fn(() => ({ snapshot }));
    const bindings = { NANOCODEX_PROVIDER_PROBES: "true", NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName } };
    const first = fixture(bindings);
    await first.create({ candidates: [candidate, `${OSS_MODEL}:high`], preferences: { duration: 90, cost: 1 } });
    const chooser = async (model: string, input: unknown) => {
      if (model !== "typesafe/jev") return completion();
      const state = JSON.parse((input as { state: string }).state);
      expect(state.provider_telemetry).toMatchObject({ provenance: "trusted_runtime_aggregate", workerColo: null, clientIngressColo: null });
      expect(state.preferences).toMatchObject({ duration: 90, cost: 1 });
      expect(state.candidates).toHaveLength(2);
      for (const c of state.candidates) expect(c.responsiveness).toMatchObject({
        signalKind: "generation_ttft_not_task_duration", live: null,
        probe: { generationTtftSampleCount: 3, workerColo: null, regionalMatch: false },
      });
      const best = state.candidates.sort((a: any, b: any) =>
        a.responsiveness.probe.generationTtftP50Ms - b.responsiveness.probe.generationTtftP50Ms)[0];
      return classification(best.id);
    };
    first.ai.mockImplementation(chooser);
    expect((await first.call("POST", "/responses", { input: "first task" })).status).toBe(200);
    expect(first.commits.at(-1)?.route?.thinking).toBe("medium");
    expect(getByName).toHaveBeenCalledExactlyOnceWith(PROBE_OWNER);
    const pinned = first.commits.at(-1)!.route;
    fastEffort = "high";
    first.restart();
    expect((await first.call("POST", "/responses", { input: "complete followup history" })).status).toBe(200);
    expect(first.commits.at(-1)?.route).toEqual(pinned);
    expect(snapshot).toHaveBeenCalledTimes(1);
    expect(first.ai.mock.calls.filter(([model]) => model === "typesafe/jev")).toHaveLength(1);
    expect(JSON.stringify(first.commits)).not.toContain("generationTtft");
    const next = fixture(bindings);
    await next.create({ candidates: [candidate, `${OSS_MODEL}:high`], preferences: { duration: 90, cost: 1 } });
    next.ai.mockImplementation(chooser);
    expect((await next.call("POST", "/responses", { input: "new session task" })).status).toBe(200);
    expect(next.commits.at(-1)?.route?.thinking).toBe("high");
    expect(snapshot).toHaveBeenCalledTimes(2);
  });

  it("limits the shared snapshot read to250ms and safely ignores a later rejection", async () => {
    vi.useFakeTimers();
    let reject!: (reason: unknown) => void;
    let entered!: () => void;
    const started = new Promise<void>(resolve => { entered = resolve; });
    const snapshot = vi.fn(() => { entered(); return new Promise<unknown>((_resolve, fail) => { reject = fail; }); });
    const f = fixture({ NANOCODEX_PROVIDER_PROBES: "true", NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName: () => ({ snapshot }) } });
    await f.create();
    f.ai.mockImplementation(async (model, input) => {
      if (model !== "typesafe/jev") return completion();
      const state = JSON.parse((input as { state: string }).state);
      expect(state.candidates[0].responsiveness.probe).toBeNull();
      return classification();
    });
    const pending = f.call("POST", "/responses", { input: "hello" });
    await started;
    await vi.advanceTimersByTimeAsync(INFERENCE_PROBE_TIMEOUT_MS - 1);
    expect(f.ai).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect((await pending).status).toBe(200);
    reject(Error("private-late-coordinator-error"));
    await Promise.resolve();
    expect(snapshot).toHaveBeenCalledTimes(1);
    expect(JSON.stringify(f.commits)).not.toContain("private");
  });

  it.each(["failure", "malformed", "disabled"])("treats %s probe context as unknown without blocking inference", async mode => {
    const snapshot = vi.fn(async () => { if (mode === "failure") throw Error("private-probe-error"); return { invalid: true }; });
    const f = fixture({ NANOCODEX_PROVIDER_PROBES: mode === "disabled" ? "false" : "true",
      NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName: () => ({ snapshot }) } });
    await f.create();
    f.ai.mockImplementation(async (model, input) => {
      if (model !== "typesafe/jev") return completion();
      const state = JSON.parse((input as { state: string }).state);
      expect(state.candidates[0].responsiveness.probe).toBeNull();
      return classification();
    });
    expect((await f.call("POST", "/responses", { input: "hello" })).status).toBe(200);
    expect(snapshot).toHaveBeenCalledTimes(mode === "disabled" ? 0 : 1);
    expect(JSON.stringify(f.commits)).not.toContain("private");
  });

  it("excludes stale, insufficient, regional, live, noncandidate and subscription measurements", async () => {
    const snapshot = vi.fn(async () => [
      probe("medium", 1, { lastObservedAt: Date.now() - 400_000, lastTtftObservedAt: Date.now() - 400_000 }),
      probe("medium", 2, { generationTtftSampleCount: 2 }),
      probe("medium", 3, { workerColo: "LHR", scope: "worker_colo" }),
      probe("medium", 4, { source: "live" }),
      probe("medium", 5, { model: "uncatalogued-model" }),
      probe("medium", 6, { backend: "chatgpt", model: "gpt-6-astra" }),
    ]);
    const f = fixture({ NANOCODEX_PROVIDER_PROBES: "true", NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName: () => ({ snapshot }) } });
    await f.create();
    f.ai.mockImplementation(async (model, input) => {
      if (model !== "typesafe/jev") return completion();
      const state = JSON.parse((input as { state: string }).state);
      expect(state.candidates).toHaveLength(1);
      expect(state.candidates[0].responsiveness).toMatchObject({ live: null, probe: null });
      return classification();
    });
    expect((await f.call("POST", "/responses", { input: "hello" })).status).toBe(200);
    expect(f.commits.at(-1)?.route?.backend).toBe("workers_ai");
  });

  it("rejects client telemetry fields before any coordinator or model call", async () => {
    const snapshot = vi.fn(async () => [probe()]);
    const f = fixture({ NANOCODEX_PROVIDER_PROBES: "true", NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName: () => ({ snapshot }) } });
    expect((await f.create({ provider_performance: [probe()] })).status).toBe(400);
    await f.create();
    expect((await f.call("POST", "/responses", { input: "x", provider_performance: [probe()] })).status).toBe(400);
    expect((await f.call("POST", "/responses", { input: "x", workerColo: "LHR" })).status).toBe(400);
    expect(snapshot).not.toHaveBeenCalled();
    expect(f.ai).not.toHaveBeenCalled();
  });
});

describe("stateless standard Responses", () => {
  const call = (bindings: InferenceSessionEnv, body: unknown, limit = 4096, signal = new AbortController().signal) =>
    executeStatelessInferenceResponse(bindings, body, limit, signal);

  it("routes independent requests without storage, session identity or account access", async () => {
    let selected = candidate;
    const ai = vi.fn(async (model: string, input: unknown) => {
      if (model === "typesafe/jev") return classification(selected);
      expect(input).toMatchObject({ messages: [{ role: "user", content: selected }] });
      return completion();
    });
    const bindings = new Proxy({ AI: { run: ai } }, {
      get(target, key) {
        if (["OPENROUTER_API_KEY", "AI_GATEWAY_API_KEY", "NANOCODEX_PROVIDER_PROBES",
          "NANOCODEX_PROVIDER_PROBE_COORDINATOR"].includes(String(key))) return undefined;
        if (key === "AI") return target.AI;
        throw Error(`unexpected capability ${String(key)}`);
      },
    });
    const send = vi.fn(() => { throw Error("unexpected account or network call"); });
    vi.stubGlobal("fetch", send);
    const ids: string[] = [];
    for (const effort of ["medium", "high"]) {
      selected = `${OSS_MODEL}:${effort}`;
      const response = await call(bindings, { model: "auto", input: selected });
      expect(response.status).toBe(200);
      const body = await response.json() as Record<string, unknown>;
      expect(body).toMatchObject({ object: "response", status: "completed", model: OSS_MODEL,
        output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "fixture answer" }] }],
        usage: { input_tokens: 12, output_tokens: 3, total_tokens: 15 },
        route: { backend: "workers_ai", thinking: effort }, buffering: "buffered" });
      expect(body).not.toHaveProperty("session_id");
      expect(response.headers.get("x-nanocodex-session-id")).toBeNull();
      expect(response.headers.get("x-nanocodex-inference-session-id")).toBeNull();
      ids.push(body.id as string);
    }
    expect(new Set(ids).size).toBe(2);
    expect(ai.mock.calls.map(([model]) => model)).toEqual(["typesafe/jev", OSS_MODEL, "typesafe/jev", OSS_MODEL]);
    expect(send).not.toHaveBeenCalled();
  });

  it.each(["auto", OSS_MODEL, `${OSS_MODEL}:low`])("selects %s within the requested constraints", async model => {
    const f = fixture();
    f.ai.mockImplementation(async (backend, input) => {
      if (backend !== "typesafe/jev") return completion();
      const state = JSON.parse((input as { state: string }).state);
      expect(state.candidates.every((c: any) => c.model === OSS_MODEL && c.backend === "workers_ai")).toBe(true);
      if (model.endsWith(":low")) expect(state.candidates.map((c: any) => c.id)).toEqual([model]);
      return classification(`${OSS_MODEL}:low`);
    });
    const response = await call(f.bindings, { model, input: "hello" }, 32);
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ model: OSS_MODEL, route: { thinking: "low" } });
    expect(f.ai.mock.calls[1]?.[1]).toMatchObject({ max_completion_tokens: 32 });
    expect(f.persisted.size).toBe(0);
  });

  it("lets canonical models span providers and exact candidates select provider/effort", async () => {
    const exact = "vercel:openai/gpt-6-astra:high";
    const send = vi.fn(async (..._args: Parameters<typeof fetch>) => Response.json(completion()));
    vi.stubGlobal("fetch", send);
    const f = fixture({ OPENROUTER_API_KEY: "synthetic-key", AI_GATEWAY_API_KEY: "synthetic-key" });
    for (const model of ["gpt-6-astra", exact]) {
      f.ai.mockImplementation(async (_backend, input) => {
        const state = JSON.parse((input as { state: string }).state);
        expect(state.candidates.every((c: any) => c.model === "gpt-6-astra")).toBe(true);
        if (model === exact) expect(state.candidates.map((c: any) => c.id)).toEqual([exact]);
        else expect(new Set(state.candidates.map((c: any) => c.backend))).toEqual(new Set(["openrouter", "vercel"]));
        return classification(exact);
      });
      const response = await call(f.bindings, { model, input: "hello" });
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({ model: "gpt-6-astra", route: { backend: "vercel", thinking: "high" } });
    }
    expect(send).toHaveBeenCalledTimes(2);
    for (const [endpoint, init] of send.mock.calls) {
      expect(endpoint).toBe("https://ai-gateway.vercel.sh/v1/chat/completions");
      expect(JSON.parse(init!.body as string)).toMatchObject({ model: "openai/gpt-6-astra", reasoning_effort: "high" });
    }
  });

  it.each([
    { model: "unknown-model" }, { model: "gpt-6-astra:high" }, { session_id: sessionId },
    { previous_response_id: "resp_unknown" }, { previous_response_id: null },
    { model: `${OSS_MODEL}:low`, reasoning: { effort: "high" } },
    { max_output_tokens: 33 }, { account_id: "synthetic-account" },
  ])("rejects invalid stateless requests before routing %#", async extra => {
    const f = fixture();
    expect((await call(f.bindings, { input: "hello", ...extra }, 32)).status).toBe(400);
    expect(f.ai).not.toHaveBeenCalled();
    expect(f.persisted.size).toBe(0);
  });

  it("keeps buffered SSE free of session fields and headers", async () => {
    const f = fixture();
    const response = await call(f.bindings, { input: "hello", stream: true });
    expect(response.headers.get("x-nanocodex-inference-buffering")).toBe("buffered");
    expect(response.headers.get("x-nanocodex-session-id")).toBeNull();
    const text = await response.text();
    expect(text).toContain("event: response.completed");
    expect(text).toContain('"object":"response"');
    expect(text).not.toContain("session_id");
  });

  it("sanitizes stateless provider failures", async () => {
    const f = fixture();
    f.ai.mockImplementation(async model => {
      if (model === "typesafe/jev") return classification();
      throw Error("private prompt and deployment key");
    });
    const response = await call(f.bindings, { input: "hello" });
    expect(response.status).toBe(502);
    expect(await response.json()).toEqual({ error: { code: "inference_failed" } });
  });

  it("shares the 120s deadline across routing and generation", async () => {
    vi.useFakeTimers();
    let routing!: () => void, generation!: () => void;
    const routeStarted = new Promise<void>(resolve => { routing = resolve; });
    const generationStarted = new Promise<void>(resolve => { generation = resolve; });
    const f = fixture();
    f.ai.mockImplementation(async model => {
      if (model === "typesafe/jev") {
        routing();
        return new Promise(resolve => setTimeout(() => resolve(classification()), 9000));
      }
      generation(); return new Promise(() => {});
    });
    const pending = call(f.bindings, { input: "hello" });
    await routeStarted;
    await vi.advanceTimersByTimeAsync(9000);
    await generationStarted;
    await vi.advanceTimersByTimeAsync(INFERENCE_TIMEOUT_MS - 9000);
    const response = await pending;
    expect(response.status).toBe(504);
    expect(await response.json()).toEqual({ error: { code: "inference_timeout" } });
    expect(f.persisted.size).toBe(0);
  });

  it("cancels routing without issuing generation after a late route resolves", async () => {
    let started!: () => void, release!: (value: unknown) => void;
    const entered = new Promise<void>(resolve => { started = resolve; });
    const f = fixture();
    f.ai.mockImplementation(async () => { started(); return new Promise(resolve => { release = resolve; }); });
    const controller = new AbortController();
    const pending = call(f.bindings, { input: "hello" }, 4096, controller.signal);
    await entered; controller.abort(new Error("private cancellation reason"));
    expect((await pending).status).toBe(502);
    release(classification());
    await Promise.resolve(); await Promise.resolve();
    expect(f.ai).toHaveBeenCalledTimes(1);
    expect(f.persisted.size).toBe(0);
  });
});

it("accepts matching session models and rejects models conflicting with the pin", async () => {
  const f = fixture(); await f.create({});
  expect((await f.call("POST", "/responses", { input: "hello", model: candidate })).status).toBe(200);
  for (const model of ["auto", OSS_MODEL, candidate])
    expect((await f.call("POST", "/responses", { input: "full history", model })).status).toBe(200);
  const count = f.ai.mock.calls.length;
  for (const model of ["gpt-6-astra", `${OSS_MODEL}:high`, "openrouter:z-ai/glm-5.3:medium"])
    expect((await f.call("POST", "/responses", { input: "full history", model })).status).toBe(409);
  expect((await f.call("POST", "/responses", { input: "full history", model: "unknown-model" })).status).toBe(400);
  expect(f.ai).toHaveBeenCalledTimes(count);
  expect(f.ai.mock.calls.filter(([model]) => model === "typesafe/jev")).toHaveLength(1);
});


describe("sanitized public routing diagnostics", () => {
  function answerWithProbabilities(input: unknown) {
    const request = input as { questions: Record<string, { criteria: Record<string, string> }> };
    return { ...classification(), answers: {
      candidate: { choice: candidate, confidence: .8,
        probabilities: Object.fromEntries(Object.keys(request.questions.candidate.criteria).map(id => [id, id === candidate ? 1 : 0])),
        reasoning: "private-router-echo" },
      family: { choice: "other", confidence: .94,
        probabilities: Object.fromEntries(taskFamily.options.map(f => [f, f === "other" ? 1 : 0])) },
    }, audit: { prompt: "private-router-echo" } };
  }
  it.each([false, true])("returns real choice distributions in stateless JSON/SSE (stream=%s)", async stream => {
    const f = fixture();
    f.ai.mockImplementation(async (model, input) => model === "typesafe/jev" ? answerWithProbabilities(input) : completion());
    const response = await executeStatelessInferenceResponse(f.bindings, { input: "private prompt", stream },
      4096, new AbortController().signal);
    expect(response.status).toBe(200);
    const text = await response.text();
    const body = stream ? text.split("\n").filter(line => line.startsWith("data: "))
      .map(line => JSON.parse(line.slice(6))).find(event => event.type === "response.completed").response : JSON.parse(text);
    expect(body.route.diagnostics).toMatchObject({
      candidate_confidence: .8, family_confidence: .94, proposed_candidate: candidate, chosen_candidate: candidate,
      eligible_candidates: [`${OSS_MODEL}:low`, candidate, `${OSS_MODEL}:high`],
      candidate_probabilities: { [`${OSS_MODEL}:low`]: 0, [candidate]: 1, [`${OSS_MODEL}:high`]: 0 },
      confidence_status: "accepted", fallback_basis: "none", min_confidence: .75,
    });
    expect(text).not.toContain("private");
    expect(body.route).not.toHaveProperty("audit");
    expect(body.route).not.toHaveProperty("router_usage");
    expect(f.persisted.size).toBe(0);
  });

  it("retains safe distributions through session restart without another Jev call", async () => {
    const f = fixture(); await f.create({ candidates: [candidate], preferences: { text: "private preference" } });
    f.ai.mockImplementation(async (model, input) => model === "typesafe/jev" ? answerWithProbabilities(input) : completion());
    const first = await f.call("POST", "/responses", { input: "private prompt" });
    const body = await first.json() as any;
    expect(body.route.diagnostics.candidate_probabilities).toEqual({ [candidate]: 1 });
    expect(JSON.stringify(body.route)).not.toContain("private");
    f.restart();
    const metadata = await (await f.call("GET")).json() as any;
    expect(metadata.route).toEqual(body.route);
    const second = await f.call("POST", "/responses", { input: "new full history" });
    expect((await second.json() as any).route).toEqual(body.route);
    expect(f.ai.mock.calls.filter(([model]) => model === "typesafe/jev")).toHaveLength(1);
  });

  it("marks unavailable distributions as null without using confidence as probabilities", async () => {
    const f = fixture();
    const response = await executeStatelessInferenceResponse(f.bindings, { input: "hello" }, 4096, new AbortController().signal);
    expect(await response.json()).toMatchObject({ route: { diagnostics: {
      candidate_confidence: .95, family_confidence: .95, candidate_probabilities: null, family_probabilities: null,
    } } });
  });
});
