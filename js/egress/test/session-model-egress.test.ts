import { createExecutionContext } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import { handleEgress, SessionModelEgress, type EgressEnv } from "../src/egress";

const owner = "11111111-1111-4111-8111-111111111111";
const subject = `managed-session-v1_${"a".repeat(64)}`;
const ownerHeader = "x-nanocodex-session-model-owner";
function request() {
  return new Request("https://nanocodex.internal/v1/responses", { headers: {
    [ownerHeader]: owner, "x-nanocodex-subject": subject,
    authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", upgrade: "websocket",
    "openai-beta": "responses_websockets=2026-02-06",
  } });
}
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("Session-only model egress", () => {
  it("uses the private binding's live Session assertion without a callback and still reads current credentials", async () => {
    const lookup = vi.fn(async () => ({ status: 200, resolve_ms: 3, resolve_id: "01234567-0123-4567-89ab-0123456789ab", credential: { kind: "openai", revision: 1, secret: "fixture-provider-secret" } }));
    const getByName = vi.fn(() => ({ resolveModelCredential: lookup }));
    const callback = vi.fn(async () => { throw new Error("unexpected ownership callback"); });
    const upstream = vi.fn(async (input: Request) => {
      expect(input.url).toBe("https://api.openai.com/v1/responses");
      expect(input.headers.has(ownerHeader)).toBe(false);
      expect(input.headers.has("x-nanocodex-subject")).toBe(false);
      return new Response(null, { status: 200 });
    });
    vi.stubGlobal("fetch", upstream);
    const log = vi.spyOn(console, "info").mockImplementation(() => {});
    const env = { USER_CREDENTIALS: { getByName }, MANAGED_AGENT_OWNERSHIP: { fetch: callback } } as unknown as EgressEnv;
    const entrypoint = new SessionModelEgress(createExecutionContext(), env);
    for (let i = 0; i < 2; i++) expect((await entrypoint.fetch(request())).status).toBe(200);
    expect(getByName).toHaveBeenCalledWith(owner);
    expect(lookup).toHaveBeenCalledTimes(2);
    expect(callback).not.toHaveBeenCalled();
    expect(upstream).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(log.mock.calls)).not.toContain("fixture-provider-secret");
    expect(log.mock.calls[0]?.[0]).toMatchObject({ credential_kind: "openai", subject_ms: expect.any(Number), credential_ms: expect.any(Number), credential_broker_ms: 3, credential_broker_resolve_id: "01234567-0123-4567-89ab-0123456789ab", upstream_ms: expect.any(Number) });
  });

  it("forwards POST SSE with current credentials, streams bytes, and propagates cancellation", async () => {
    const lookup = vi.fn(async () => ({ status: 200, credential: { kind: "openai", revision: 1, secret: "fixture-provider-secret" } }));
    let source!: ReadableStreamDefaultController<Uint8Array>;
    const cancelled = vi.fn();
    const body = new ReadableStream<Uint8Array>({ start(controller) { source = controller; }, cancel: cancelled });
    const abort = new AbortController();
    const upstream = vi.fn(async (input: Request) => {
      expect(input.url).toBe("https://api.openai.com/v1/responses");
      expect(input.method).toBe("POST");
      expect(await input.json()).toEqual({ model: "gpt-test", stream: true, input: [] });
      expect(input.headers.get("content-type")).toBe("application/json");
      expect(input.headers.get("accept")).toBe("text/event-stream");
      expect(input.headers.get("authorization")).toBe("Bearer fixture-provider-secret");
      for (const name of [ownerHeader, "x-nanocodex-subject", "upgrade", "openai-beta", "x-private"]) {
        expect(input.headers.has(name)).toBe(false);
      }
      abort.signal.addEventListener("abort", () => expect(input.signal.aborted).toBe(true));
      return new Response(body, { headers: { "content-type": "text/event-stream", "x-codex-turn-state": "turn", "set-cookie": "secret" } });
    });
    vi.stubGlobal("fetch", upstream);
    const entrypoint = new SessionModelEgress(createExecutionContext(), { USER_CREDENTIALS: { getByName: () => ({ resolveModelCredential: lookup }) } } as unknown as EgressEnv);
    const input = request(); input.headers.delete("upgrade"); input.headers.set("content-type", "application/json"); input.headers.set("x-private", "private");
    const response = await entrypoint.fetch(new Request(input, { method: "POST", signal: abort.signal,
      body: JSON.stringify({ model: "gpt-test", stream: true, input: [] }) }));
    expect(response.status).toBe(200);
    expect(response.headers.get("x-codex-turn-state")).toBe("turn");
    expect(response.headers.has("set-cookie")).toBe(false);
    const reader = response.body!.getReader();
    source.enqueue(new TextEncoder().encode("data: partial\n\n"));
    expect(new TextDecoder().decode((await reader.read()).value)).toBe("data: partial\n\n");
    await reader.cancel();
    expect(cancelled).toHaveBeenCalledOnce();
    abort.abort();
  });

  it("rejects POST authority, protocol, and path violations before credential lookup", async () => {
    const entrypoint = new SessionModelEgress(createExecutionContext(), {} as EgressEnv);
    const headers = new Headers(request().headers); headers.delete("upgrade"); headers.set("content-type", "application/json");
    for (const header of [ownerHeader, "x-nanocodex-subject", "authorization", "content-type"]) {
      const invalid = new Headers(headers); invalid.delete(header);
      expect((await entrypoint.fetch(new Request("https://nanocodex.internal/v1/responses", { method: "POST", headers: invalid, body: "{}" }))).status).toBe(403);
    }
    for (const [header, value] of [["upgrade", "websocket"], ["chatgpt-account-id", "spoofed"], ["authorization", "Bearer caller-secret"]]) {
      const invalid = new Headers(headers); invalid.set(header!, value!);
      expect((await entrypoint.fetch(new Request("https://nanocodex.internal/v1/responses", { method: "POST", headers: invalid, body: "{}" }))).status).toBe(403);
    }
    for (const path of ["/v1/responses?x=1", "/v1/responses/", "/v1/responses/compact", "/v1/search"]) {
      expect((await entrypoint.fetch(new Request(`https://nanocodex.internal${path}`, { method: "POST", headers, body: "{}" }))).status).toBe(403);
    }
  });

  it.each([undefined, `https://relay.example/v1/${"a".repeat(43)}`])("refreshes ChatGPT POST credentials with bounded body replay (relay=%s)", async (relay) => {
    const lookup = vi.fn(async (recover: boolean) => ({ status: 200, credential: {
      kind: "chatgpt", accountId: "account", revision: recover ? 2 : 1, secret: recover ? "new-secret" : "old-secret",
    } }));
    const bodies: string[] = [];
    const disposed = vi.fn();
    const upstream = vi.fn(async (input: Request) => {
      expect(input.url).toBe(relay ? `${relay}/http/codex-responses` : "https://chatgpt.com/backend-api/codex/responses");
      expect(input.headers.get("originator")).toBe("codex_cli_rs");
      expect(input.headers.get("chatgpt-account-id")).toBe("account");
      expect(input.headers.get("accept")).toBe("text/event-stream");
      bodies.push(await input.text());
      return input.headers.get("authorization") === "Bearer old-secret"
        ? new Response(new ReadableStream({ cancel: disposed }), { status: 401 })
        : new Response("data: complete\n\n", { headers: { "content-type": "text/event-stream" } });
    });
    vi.stubGlobal("fetch", upstream);
    const entrypoint = new SessionModelEgress(createExecutionContext(), {
      USER_CREDENTIALS: { getByName: () => ({ resolveModelCredential: lookup }) }, CODEX_RELAY_URL: relay,
    } as unknown as EgressEnv);
    const headers = new Headers(request().headers); headers.delete("upgrade"); headers.set("content-type", "application/json");
    const body = JSON.stringify({ stream: true, input: [] });
    const response = await entrypoint.fetch(new Request("https://nanocodex.internal/v1/responses", { method: "POST", headers, body }));
    expect(response.status).toBe(200);
    expect(await response.text()).toBe("data: complete\n\n");
    expect(bodies).toEqual([body, body]);
    expect(disposed).toHaveBeenCalledOnce();
    expect(lookup).toHaveBeenLastCalledWith(true, 1, undefined);
    headers.set("content-length", String(32 * 1024 * 1024 + 1));
    const denied = await entrypoint.fetch(new Request("https://nanocodex.internal/v1/responses", { method: "POST", headers, body }));
    expect(denied.status).toBe(413);
    expect(upstream).toHaveBeenCalledTimes(2);
  });

  it("preserves cancellation while the ChatGPT relay is awaiting response headers", async () => {
    const abort = new AbortController();
    let forwarded!: Request;
    let entered!: () => void;
    const dispatched = new Promise<void>((resolve) => { entered = resolve; });
    const relay = vi.fn(async (input: Request) => {
      forwarded = input;
      expect(input.url).toBe("https://chatgpt-egress.internal/backend-api/codex/responses");
      expect(input.method).toBe("POST");
      entered();
      return new Promise<Response>((_resolve, reject) => input.signal.addEventListener("abort", () => reject(input.signal.reason), { once: true }));
    });
    const entrypoint = new SessionModelEgress(createExecutionContext(), {
      USER_CREDENTIALS: { getByName: () => ({ resolveModelCredential: async () => ({ status: 200,
        credential: { kind: "chatgpt", revision: 1, secret: "fixture", accountId: "account" } }) }) },
      CHATGPT_EGRESS: { idFromName: () => "relay", get: () => ({ fetch: relay }) },
    } as unknown as EgressEnv);
    const headers = new Headers(request().headers); headers.delete("upgrade"); headers.set("content-type", "application/json");
    const pending = entrypoint.fetch(new Request("https://nanocodex.internal/v1/responses", {
      method: "POST", headers, body: '{"stream":true,"input":[]}', signal: abort.signal,
    }));
    await dispatched;
    abort.abort();
    expect(forwarded.signal.aborted).toBe(true);
    expect((await pending).status).toBe(502);
    expect(relay).toHaveBeenCalledOnce();
  });

  it("never accepts the owner header through the general broker", async () => {
    const response = await handleEgress(request(), {} as EgressEnv);
    expect(response.status).toBe(403);
  });

  it("rejects non-model destinations, malformed subjects, missing owners, and missing protocol headers", async () => {
    const entrypoint = new SessionModelEgress(createExecutionContext(), {} as EgressEnv);
    for (const url of ["https://broker.internal/users/other/credentials", "https://nanocodex.internal/v1/search", "https://nanocodex.internal/v1/responses?other=1", "https://example.com/v1/responses"]) {
      expect((await entrypoint.fetch(new Request(url, request()))).status).toBe(403);
    }
    for (const header of [ownerHeader, "x-nanocodex-subject", "authorization", "upgrade", "openai-beta"]) {
      const input = request(); input.headers.delete(header);
      expect((await entrypoint.fetch(input)).status).toBe(403);
    }
    for (const [header, value] of [[ownerHeader, "not a user"], ["x-nanocodex-subject", "b".repeat(64)]]) {
      const input = request(); input.headers.set(header!, value!);
      expect((await entrypoint.fetch(input)).status).toBe(403);
    }
  });
});
