import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import type { DurableAgentSession } from "../src/index";

// Production HTTP admission -> retained authorization -> WASM model loop ->
// Code Mode -> actual mount/environment handlers. The native provider stops at
// its first RPC: no container or account credentials are needed by this test.
it("routes an explicitly scoped Connect turn only to its Cloudflare mounts", async () => {
  const grantId = `0x${"a".repeat(64)}`;
  let principal: Principal = {
    kind: "connect_grant", userId: "11111111-1111-4111-8111-111111111111",
    organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    role: "owner", subjectId: "user:sandbox-fixture", credentialId: grantId, authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"],
    connectGrant: { grantId, connectors: ["chatgpt"], mcpIds: [], sandboxExecution: true },
  };
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    let step = 0, turn = 0;
    const transcript: unknown[] = [], native: unknown[] = [];
    const code = `
      for (const [label, provider, name] of [
        ["new", "cf_sandbox", "cad"], ["vm", "fixture-mac", "vm"],
        ["personal", "cf_sandbox", "personal"], ["foreign", "cf_sandbox", "foreign"],
        ["own", "cf_sandbox", "own"]
      ]) {
        try { text({label, result: await tools.mount({provider, name})}); }
        catch (error) { text({label, error: String(error)}); }
      }
      text({environment: await tools.environment()});
    `;
    class ModelSocket extends EventTarget {
      readyState = 1;
      bufferedAmount = 0;
      accept() {}
      close() { this.readyState = 3; }
      send(value: string) {
        transcript.push(JSON.parse(value));
        const output = step++ === 0
          ? [{ type: "custom_tool_call", name: "exec", call_id: `sandbox-${turn}`, input: code }]
          : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "DONE" }] }];
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          type: "response.completed", response: { id: `response-${turn}-${step}`, status: "completed", output,
            usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 } },
        }) })));
      }
    }
    const original = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { configurable: true, value: { ...original,
      NANOCODEX: { fetch: async (input: RequestInfo | URL) => {
        const url = new URL(input instanceof Request ? input.url : String(input));
        if (url.pathname.includes("/responses")) return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
        if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
        return Response.json({ connectors: {}, mcp_connections: [], vault: [] });
      } },
      NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => Response.json({ tools: [], machines: [] }) }) },
      NANOCODEX_SANDBOXES: { idFromName: (name: string) => name, get: () => ({ bindAccountEgress: async (subject: string, owner: string) => {
        native.push({ subject, owner }); throw Error("fixture-native-boundary");
      } }) },
      NANOCODEX_VM_HOST_POOLS: { getByName: () => { throw Error("VM factory must never be probed"); } },
    } });
    const request = (path: string, body: unknown) => {
      const headers = new Headers();
      if (path !== "/create") forwardPrincipalAssertions(headers, principal);
      return session.fetch(new Request(`https://session.internal${path}`, { method: "POST", headers, body: JSON.stringify(body) }));
    };
    expect((await request("/create", {
      session_id: crypto.randomUUID(), owner_id: principal.userId, organization_id: principal.organizationId,
      team_id: principal.teamId, authorization_epoch: 1, public_origin: "https://nanocodex.example",
      settings: DEFAULT_AGENT_SETTINGS, configuration: {},
    })).status).toBe(200);
    for (const [slot, name, owner] of [[0, "personal", undefined], [1, "foreign", `0x${"b".repeat(64)}`], [2, "own", grantId]] as const) {
      state.storage.sql.exec(`INSERT INTO managed_mounts VALUES (?, 'cloudflare', ?, ?, ?, ?, 'mounted', 1, 1)`,
        crypto.randomUUID(), name, `/cloudflare-${name}`, crypto.randomUUID(),
        JSON.stringify({ namespace_slot: slot, ...(owner === undefined ? {} : { connect_grant_id: owner }) }));
    }
    try {
      for (turn = 0; turn < 2; turn++) {
        step = 0;
        if (turn === 1) principal = { ...principal, connectGrant: { grantId, connectors: ["chatgpt"], mcpIds: [] } };
        expect((await request("/turns", { id: `sandbox-turn-${turn}`, input: "Exercise the sandbox policy fixture." })).status).toBe(202);
        await expect.poll(() => state.storage.sql.exec("SELECT state, error FROM managed_turns WHERE id = ?", `sandbox-turn-${turn}`).one(), { timeout: 20_000 })
          .toEqual({ state: "completed", error: null });
        const last = JSON.stringify(transcript.at(-1));
        console.info({ type: "connect.sandbox.turn", turn, native, input: (transcript.at(-1) as { input?: unknown }).input });
        if (turn === 0) {
          expect(native).toEqual([{ subject: expect.any(String), owner: grantId }]);
          expect(last).toContain("fixture-native-boundary");
          expect(last).toContain("mount belongs to another authorization");
          expect(last).toContain('the current authorization cannot provision');
          expect(last).toContain("cloudflare-own");
          expect(last).not.toContain("cloudflare-personal");
          expect(last).not.toContain("cloudflare-foreign");
          const retained = state.storage.sql.exec<{ configuration_json: string; provider_resource_id: string }>("SELECT configuration_json,provider_resource_id FROM managed_mounts WHERE name='cad'").one();
          expect(JSON.parse(retained.configuration_json)).toMatchObject({ connect_grant_id: grantId });
          expect(state.storage.sql.exec("SELECT * FROM managed_mounts WHERE provider='host'").toArray()).toEqual([]);
        } else {
          expect(native).toHaveLength(1);
          expect(last).not.toContain("cloudflare-own");
          expect(last).not.toContain("fixture-native-boundary");
        }
      }
    } finally { await state.storage.deleteAlarm(); }
  });
}, 60_000);
