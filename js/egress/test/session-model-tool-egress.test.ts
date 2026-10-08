import { createExecutionContext } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import { handleEgress, SessionModelEgress, SessionToolEgress, type EgressEnv } from "../src/egress";

// Managed web search and image tools of a session_v1 Session. Through the
// general broker every call resolves the subject via ManagedAgentOwnership,
// which calls back into the originating Session (Workers depth ratchet).
const owner = "11111111-1111-4111-8111-111111111111";
const subject = `managed-session-v1_${"a".repeat(64)}`;
const ownerHeader = "x-nanocodex-session-model-owner";
const TOOL_PATHS = ["/v1/search", "/v1/images/generations", "/v1/images/edits"] as const;
const UPSTREAM: Record<string, string> = {
  "/v1/search": "https://api.openai.com/v1/alpha/search",
  "/v1/images/generations": "https://api.openai.com/v1/images/generations",
  "/v1/images/edits": "https://api.openai.com/v1/images/edits",
};

function toolRequest(path: string, headers: Record<string, string> = {}, init: RequestInit = {}): Request {
  return new Request(`https://nanocodex.internal${path}`, { method: "POST", headers: {
    authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json",
    "x-nanocodex-subject": subject, [ownerHeader]: owner, ...headers,
  }, body: JSON.stringify({ prompt: "fixture" }), ...init });
}
function fixtureEnv(extra: Record<string, unknown> = {}) {
  const callback = vi.fn(async () => Response.json({ user_id: owner }));
  const lookup = vi.fn(async (_recover: boolean, _revision?: number, _account?: string) => ({
    status: 200, credential: { kind: "openai", revision: 1, secret: "fixture-provider-secret" } }));
  const getByName = vi.fn((_name: string, _options?: unknown) => ({ resolveModelCredential: lookup }));
  const env = {
    MANAGED_AGENT_OWNERSHIP: { fetch: callback },
    AGENT_SUBJECTS: { getByName: () => { throw new Error("must not allocate a legacy subject DO"); } },
    USER_CREDENTIALS: { getByName }, ...extra,
  } as unknown as EgressEnv;
  return { env, callback, lookup, getByName };
}
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("Session model egress: managed web search and image tools", () => {
  it("baseline: the general broker calls back into the Session once per search/image request", async () => {
    const { env, callback } = fixtureEnv();
    const upstream = vi.fn(async () => Response.json({ output: "ok" }));
    vi.spyOn(console, "info").mockImplementation(() => {});
    for (let i = 0; i < 6; i++) {
      const path = TOOL_PATHS[i % 3]!;
      const request = toolRequest(path); request.headers.delete(ownerHeader);
      expect((await handleEgress(request, env, undefined, upstream as typeof fetch)).status).toBe(200);
    }
    expect(callback).toHaveBeenCalledTimes(6);
  });

  it("a burst of search, generation, and edit calls uses the asserted owner with zero Session callbacks", async () => {
    const { env, callback, lookup, getByName } = fixtureEnv();
    const upstream = vi.fn(async (input: Request) => {
      expect(Object.values(UPSTREAM)).toContain(input.url);
      expect(input.method).toBe("POST");
      expect(input.headers.get("authorization")).toBe("Bearer fixture-provider-secret");
      for (const name of [ownerHeader, "x-nanocodex-subject", "x-nanocodex-model-region"]) expect(input.headers.has(name)).toBe(false);
      expect(await input.json()).toEqual({ prompt: "fixture" });
      return Response.json({ output: "ok" });
    });
    vi.stubGlobal("fetch", upstream);
    const log = vi.spyOn(console, "info").mockImplementation(() => {});
    const entrypoint = new SessionModelEgress(createExecutionContext(), env);
    for (let i = 0; i < 12; i++) {
      const path = TOOL_PATHS[i % 3]!;
      const response = await entrypoint.fetch(toolRequest(path));
      expect(response.status, path).toBe(200);
      expect(await response.json()).toEqual({ output: "ok" });
    }
    expect(callback).not.toHaveBeenCalled();
    expect(upstream).toHaveBeenCalledTimes(12);
    expect(upstream.mock.calls.map(([request]) => (request as Request).url)).toEqual(
      Array.from({ length: 12 }, (_, i) => UPSTREAM[TOOL_PATHS[i % 3]!]));
    expect(lookup).toHaveBeenCalledTimes(12);
    expect(new Set(getByName.mock.calls.map(([name]) => name))).toEqual(new Set([owner]));
    expect(JSON.stringify(log.mock.calls)).not.toContain("fixture-provider-secret");
  });

  it("keeps the retained ChatGPT account selection and ignores tool-call placement headers", async () => {
    const { env, callback, lookup, getByName } = fixtureEnv();
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ output: "ok" })));
    vi.spyOn(console, "info").mockImplementation(() => {});
    const entrypoint = new SessionModelEgress(createExecutionContext(), env);
    const response = await entrypoint.fetch(toolRequest("/v1/search", {
      "x-nanocodex-chatgpt-account-id": "acct-fixture", "x-nanocodex-model-region": "weur",
    }));
    expect(response.status).toBe(200);
    expect(lookup).toHaveBeenCalledWith(false, undefined, "acct-fixture");
    // A region assertion places only the model transport, never tool calls.
    expect(getByName).toHaveBeenCalledWith(owner);
    expect(callback).not.toHaveBeenCalled();
  });

  it("still requires the provider placeholder and JSON content type for tool calls", async () => {
    const { env, callback, lookup } = fixtureEnv();
    const upstream = vi.fn(async () => new Response("must not fetch"));
    vi.stubGlobal("fetch", upstream);
    vi.spyOn(console, "info").mockImplementation(() => {});
    const entrypoint = new SessionModelEgress(createExecutionContext(), env);
    for (const headers of [{ authorization: "Bearer caller-secret" }, { "content-type": "text/plain" },
      { "chatgpt-account-id": "spoofed" }, { originator: "spoofed" }]) {
      expect((await entrypoint.fetch(toolRequest("/v1/images/generations", headers))).status).toBe(403);
    }
    expect(callback).not.toHaveBeenCalled();
    expect(lookup).not.toHaveBeenCalled();
    expect(upstream).not.toHaveBeenCalled();
  });

  it("accepts only exact POST tool routes; Realtime, other methods, queries, and variants fail closed", async () => {
    const { env, callback, lookup } = fixtureEnv();
    const upstream = vi.fn(async () => new Response("must not fetch"));
    vi.stubGlobal("fetch", upstream);
    const entrypoint = new SessionModelEgress(createExecutionContext(), env);
    const denied: [string, string][] = [
      ["GET", "/v1/search"], ["PUT", "/v1/images/generations"],
      ["POST", "/v1/search?x=1"], ["POST", "/v1/search/"], ["POST", "/v1/images/edits/extra"],
      ["POST", "/v1/images/variations"], ["POST", "/v1/realtime/calls"], ["GET", "/v1/realtime/sideband"],
      ["POST", "/v1/model-status"], ["POST", "/v1/broker-readiness"],
    ];
    for (const [method, path] of denied) {
      const response = await entrypoint.fetch(toolRequest(path, {}, { method, ...(method === "GET" ? { body: null } : {}) }));
      expect(response.status, `${method} ${path}`).toBe(403);
      expect(await response.json()).toMatchObject({ error: "invalid_session_model_authority" });
    }
    for (const url of ["https://example.com/v1/search", "https://nanocodex.internal:8443/v1/search", "https://public-egress.internal/v1/request"]) {
      expect((await entrypoint.fetch(new Request(url, toolRequest("/v1/search")))).status, url).toBe(403);
    }
    expect(callback).not.toHaveBeenCalled();
    expect(lookup).not.toHaveBeenCalled();
    expect(upstream).not.toHaveBeenCalled();
  });

  it("rejects missing, malformed, and non-Session authority before any lookup", async () => {
    const { env, callback, lookup } = fixtureEnv();
    const entrypoint = new SessionModelEgress(createExecutionContext(), env);
    for (const headers of [{ [ownerHeader]: "" }, { [ownerHeader]: "../owner" }, { "x-nanocodex-subject": "b".repeat(64) },
      { "x-nanocodex-subject": "s".repeat(43) }]) {
      expect((await entrypoint.fetch(toolRequest("/v1/search", headers))).status).toBe(403);
    }
    const missing = toolRequest("/v1/search"); missing.headers.delete(ownerHeader);
    expect((await entrypoint.fetch(missing)).status).toBe(403);
    expect(callback).not.toHaveBeenCalled();
    expect(lookup).not.toHaveBeenCalled();
  });

  it("the general broker and the tool binding never accept a model owner assertion for tool routes", async () => {
    const { env, callback, lookup } = fixtureEnv();
    const upstream = vi.fn(async () => new Response("must not fetch"));
    vi.stubGlobal("fetch", upstream);
    for (const path of TOOL_PATHS) {
      const general = await handleEgress(toolRequest(path), env, undefined, upstream as typeof fetch);
      expect(general.status).toBe(403);
      expect(await general.json()).toMatchObject({ error: "invalid_session_model_authority" });
      // The Session tool binding cannot reach model credentials either.
      const toolBinding = new SessionToolEgress(createExecutionContext(), env);
      const viaTool = toolRequest(path); viaTool.headers.delete(ownerHeader);
      viaTool.headers.set("x-nanocodex-session-tool-owner", owner);
      expect((await toolBinding.fetch(viaTool)).status).toBe(403);
    }
    expect(callback).not.toHaveBeenCalled();
    expect(lookup).not.toHaveBeenCalled();
    expect(upstream).not.toHaveBeenCalled();
  });

  it("an authority never applies to another subject or to Realtime operations", async () => {
    const { env, callback, lookup } = fixtureEnv();
    const upstream = vi.fn(async () => new Response("must not fetch"));
    const other = `managed-session-v1_${"b".repeat(64)}`;
    const foreign = toolRequest("/v1/search", { "x-nanocodex-subject": other }); foreign.headers.delete(ownerHeader);
    expect((await handleEgress(foreign, env, undefined, upstream as typeof fetch, undefined, { subject, owner })).status).toBe(403);
    const realtime = toolRequest("/v1/realtime/calls"); realtime.headers.delete(ownerHeader);
    expect((await handleEgress(realtime, env, undefined, upstream as typeof fetch, undefined, { subject, owner })).status).toBe(403);
    expect(callback).not.toHaveBeenCalled();
    expect(lookup).not.toHaveBeenCalled();
    expect(upstream).not.toHaveBeenCalled();
  });
});
