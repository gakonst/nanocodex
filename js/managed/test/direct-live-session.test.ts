import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import { routeManaged, type ManagedProxyEnv } from "../../account/worker/managedProxy";
import { apiKeyDigest } from "nanocodex/cloudflare/managed-auth";
import type { DurableAgentSession } from "../src/index";

it("direct ingress preserves real session ownership, ready socket and prepare acknowledgment", async () => {
  const namespace = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (session, state) => {
    const token = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
    const request = new Request("https://nanocodex.example/v1/agents/live?model=gpt-6-sol&thinking=high", { headers: {
      authorization: `Bearer ${token}`, upgrade: "websocket", "x-nanocodex-prepare": "active-conversation",
    } });
    const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
    let release!: () => void;
    const blocked = new Promise<void>(resolve => { release = resolve; });
    const discovery = vi.fn(async () => { await blocked; return new Response(null, { status: 503 }); });
    const runtime = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { value: { ...runtime,
      NANOCODEX: { fetch: async () => Response.json({ connectors: {}, mcp_connections: [] }) },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: discovery }) },
      NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => Response.json({ snapshot: null }) }) },
      NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
    } });
    const digest = await apiKeyDigest(request);
    let creates = 0, forwarded: Request | undefined, selectedId: string | undefined;
    const front: ManagedProxyEnv = {
      NANOCODEX_BACKEND: { fetch: async () => { throw Error("must not fall back"); } } as unknown as Fetcher,
      NANOCODEX_LIVE_API_KEYS: { getByName: key => {
        expect(key).toBe(digest);
        return { resolveAuthorizedKey: async () => ({ id: "k".repeat(12), prefix: `ncx_live_${"k".repeat(12)}`, label: "fixture", createdAt: 1,
          digest, userId: owner, organizationId: organization, teamId: team, authorizationEpoch: 1, role: "writer",
          capabilities: ["agents:read", "agents:write", "tools:use"] }) };
      } },
      NANOCODEX_LIVE_SESSIONS: { getByName: id => { selectedId = id; return { fetch: async req => {
        creates++; forwarded = req; return session.fetch(req);
      } }; } },
    };
    let socket: WebSocket | undefined;
    try {
      const response = (await routeManaged(request, front, new URL(request.url)))!;
      expect(response.status).toBe(101);
      expect(response.headers.get("x-nanocodex-prepare")).toBe("active-conversation");
      expect(response.headers.get("server-timing")).toContain("managed_auth");
      socket = response.webSocket!;
      const ready = new Promise<unknown>(resolve => socket!.addEventListener("message", event => resolve(JSON.parse(event.data as string)), { once: true }));
      socket.accept();
      expect(await ready).toMatchObject({ type: "ready", session_id: selectedId, active_turns: [], settings: {
        model: "gpt-6-sol", thinking: "high", reasoning_mode: "standard", fast_mode: false,
      } });
      expect(creates).toBe(1);
      expect(state.storage.sql.exec("SELECT session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin FROM session_state").one()).toMatchObject({
        session_id: selectedId, owner_id: owner, organization_id: organization, team_id: team, authorization_epoch: 1, public_origin: "https://nanocodex.example",
      });
      expect(await state.storage.get("nanocodex:credential-binding")).toMatchObject({ owner_id: owner, session_id: selectedId, subject: state.id.toString(), state: "active" });
      expect((await session.fetch(new Request(forwarded!))).status).toBe(409);
      expect(state.storage.sql.exec("SELECT session_id, owner_id FROM session_state").one()).toMatchObject({ session_id: selectedId, owner_id: owner });
    } finally {
      socket?.close(1000); release();
      await new Promise(resolve => setTimeout(resolve, 30));
      await state.storage.deleteAlarm();
    }
  });
});
