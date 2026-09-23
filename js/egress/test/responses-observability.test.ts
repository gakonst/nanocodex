import { afterEach, describe, expect, it, vi } from "vitest";
import { handleEgress, type EgressEnv } from "../src/egress";
import { ChatGptEgress } from "../../account/worker/chatGptEgress";

const container = { fetch: vi.fn() };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const callerId = "11111111-1111-4111-8111-111111111111";
const secret = "synthetic-private-credential-marker";
afterEach(() => { vi.restoreAllMocks(); container.fetch.mockReset(); });

function fixture(method = "GET") {
  const relay = vi.fn(async (_request: Request) => new Response(null, { status: 204 }));
  const credential = vi.fn(async () => ({ status: 200, credential: {
    kind: "chatgpt", revision: 1, secret, accountId: "synthetic-private-account-marker",
  } }));
  const env = {
    AGENT_SUBJECTS: { getByName: () => ({ fetch: async () => Response.json({ user_id: "synthetic-user" }) }) },
    USER_CREDENTIALS: { getByName: () => ({ resolveModelCredential: credential }) },
    CHATGPT_EGRESS: { idFromName: (name: string) => name, get: () => ({ fetch: relay }) },
  } as unknown as EgressEnv;
  const request = new Request("https://nanocodex.internal/v1/responses", { method,
    headers: { authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "x-nanocodex-subject": "a".repeat(64),
      "openai-beta": "responses_websockets=2026-02-06",
      "x-nanocodex-egress-request-id": callerId, "x-nanocodex-relay-id": "synthetic-private-caller-marker",
      ...(method === "GET" ? { upgrade: "websocket" } : { "content-type": "application/json" }),
    }, ...(method === "POST" ? { body: '{"input":"synthetic-private-prompt-marker","stream":true}' } : {}),
  });
  return { env, request, relay, credential };
}

function captureLogs() {
  return ["info", "warn", "error"].map((method) => vi.spyOn(console, method as "info").mockImplementation(() => {}));
}

describe("Responses private correlation", () => {
  it("overwrites caller IDs, joins the audit, and retains the ID across one 401 recovery", async () => {
    const logs = captureLogs();
    const f = fixture();
    f.relay.mockResolvedValueOnce(new Response(null, { status: 401 }));
    expect((await handleEgress(f.request, f.env)).status).toBe(204);
    expect(f.relay).toHaveBeenCalledTimes(2);
    const ids = f.relay.mock.calls.map(([request]) => {
      expect(request.headers.get("authorization")).toBe(`Bearer ${secret}`);
      expect(request.headers.has("x-nanocodex-relay-id")).toBe(false);
      return request.headers.get("x-nanocodex-egress-request-id");
    });
    expect(ids[0]).toMatch(UUID);
    expect(ids[0]).not.toBe(callerId);
    expect(ids[1]).toBe(ids[0]);
    const records = logs.flatMap((log) => log.mock.calls.map(([record]) => record));
    expect(records).toContainEqual(expect.objectContaining({ egress_request_id: ids[0], recovered: true }));
    expect(JSON.stringify(records)).not.toMatch(/synthetic-private-|11111111-1111-4111/);
  });

  it.each(["http", "direct", "configured-relay"])("does not forward private IDs on %s requests", async (mode) => {
    const logs = captureLogs();
    const f = fixture(mode === "http" ? "POST" : "GET");
    if (mode === "direct") delete f.env.CHATGPT_EGRESS;
    if (mode === "configured-relay") f.env.CODEX_RELAY_URL = "https://relay.example.test";
    const upstream = vi.fn(async (request: Request) => {
      expect(request.headers.has("x-nanocodex-egress-request-id")).toBe(false);
      expect(request.headers.has("x-nanocodex-relay-id")).toBe(false);
      return new Response(null, { status: 204 });
    });
    if (mode === "http") f.relay.mockImplementation(upstream);
    expect((await handleEgress(f.request, f.env, undefined, upstream as typeof fetch)).status).toBe(204);
    expect(upstream).toHaveBeenCalledTimes(1);
    expect(JSON.stringify(logs.flatMap((log) => log.mock.calls))).not.toMatch(/synthetic-private-/);
  });
});

describe("Container Worker upgrade observation", () => {
  function worker() {
    vi.spyOn(Object.getPrototypeOf(ChatGptEgress.prototype), "fetch").mockImplementation(container.fetch);
    const worker = Object.create(ChatGptEgress.prototype) as ChatGptEgress;
    Object.defineProperty(worker, "ctx", { value: { container: { running: false } } });
    return worker;
  }
  function request(parentId: string) {
    return new Request("https://chatgpt-egress.internal/backend-api/codex/responses", { headers: {
      upgrade: "websocket", authorization: `Bearer ${secret}`,
      "x-nanocodex-egress-request-id": parentId, "x-nanocodex-relay-id": "synthetic-private-caller-marker",
    } });
  }
  it.each([callerId, "synthetic-private-invalid-parent-marker"])("preserves the exact runtime 101 and bounds parent ID %s", async (parentId) => {
    const logs = captureLogs();
    const [client, server] = Object.values(new WebSocketPair());
    server.accept();
    const upstream = new Response(null, { status: 101, webSocket: client });
    container.fetch.mockResolvedValue(upstream);
    const input = request(parentId);
    const response = await worker().fetch(input);
    expect(response).toBe(upstream);
    expect(response.webSocket).toBe(client);
    const forwarded = container.fetch.mock.calls[0]![0] as Request;
    const relayId = forwarded.headers.get("x-nanocodex-relay-id");
    expect(relayId).toMatch(UUID);
    expect(forwarded.headers.has("x-nanocodex-egress-request-id")).toBe(false);
    expect(input.headers.get("x-nanocodex-egress-request-id")).toBe(parentId);
    expect(logs[0]).toHaveBeenCalledWith(expect.objectContaining({ type: "responses.relay", relay_id: relayId,
      was_running: false, status: 101, outcome: "response" }));
    const record = logs[0]!.mock.calls[0]![0] as unknown as Record<string, unknown>;
    expect(record.egress_request_id).toBe(parentId === callerId ? callerId : undefined);
    expect(JSON.stringify(logs.flatMap((log) => log.mock.calls))).not.toMatch(/synthetic-private-/);
    client.accept(); client.close(); server.close();
  });
  it("preserves provider errors and success when logging throws", async () => {
    const logs = captureLogs();
    const error = new Error("synthetic-private-upstream-error-marker");
    container.fetch.mockRejectedValueOnce(error);
    await expect(worker().fetch(request(callerId))).rejects.toBe(error);
    expect(logs[0]).toHaveBeenCalledWith(expect.objectContaining({ outcome: "error" }));
    expect(JSON.stringify(logs.flatMap((log) => log.mock.calls))).not.toMatch(/synthetic-private-/);
    logs[0]!.mockImplementation(() => { throw new Error("logger unavailable"); });
    const response = new Response(null, { status: 204 });
    container.fetch.mockResolvedValueOnce(response);
    expect(await worker().fetch(request(callerId))).toBe(response);
  });
});
