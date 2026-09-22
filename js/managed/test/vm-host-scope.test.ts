import { env, evictDurableObject, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import {
  initializeEmptyVmHostScope,
  initializeVmHostScopeSchema,
  markVmHostScopeRegistration,
  shouldProbeAgentVmHostScope,
} from "../src/vm-host-scope";

function session() {
  return (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> })
    .NANOCODEX_SESSIONS.getByName(crypto.randomUUID());
}

const owner = "11111111-1111-4111-8111-111111111111";
const initialization = {
  session_id: "018f25e8-7b51-7a32-8c4d-0123456789ab", owner_id: owner,
  organization_id: owner, team_id: owner, authorization_epoch: 1,
  public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS,
  // Keep this lifecycle test independent of the personalization warmup timer.
  configuration: { tools: [] },
};

async function initialize(agent: DurableAgentSession, state: DurableObjectState): Promise<void> {
  expect((await agent.fetch(new Request("https://session.internal/credential-binding", {
    method: "PUT", body: JSON.stringify({ owner_id: owner, session_id: initialization.session_id,
      subject: state.id.toString(), durability_import: null }),
  }))).status).toBe(204);
  expect((await agent.fetch(new Request("https://session.internal/initialize", {
    method: "PUT", body: JSON.stringify(initialization),
  }))).status).toBe(204);
  await state.storage.deleteAlarm();
}

describe("agent VM factory scope", () => {
  it("keeps legacy/unknown scopes conservative and retained selection authoritative", async () => {
    await runInDurableObject(session(), (_agent, state) => {
      initializeVmHostScopeSchema(state.storage);
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(true);
      initializeEmptyVmHostScope(state.storage);
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(false);
      expect(shouldProbeAgentVmHostScope(state.storage, "retained-agent-pool")).toBe(true);
      expect(shouldProbeAgentVmHostScope(state.storage, "retained-account-pool")).toBe(true);
      markVmHostScopeRegistration(state.storage);
      initializeEmptyVmHostScope(state.storage);
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(true);
    });
  });

  it("initializes only new sessions and never resets a legacy or registered scope on replay", async () => {
    await runInDurableObject(session(), async (agent, state) => {
      await initialize(agent, state);
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(false);
      state.storage.sql.exec("DELETE FROM managed_vm_host_scope");
      await initialize(agent, state);
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(true);
      markVmHostScopeRegistration(state.storage);
      await initialize(agent, state);
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(true);
    });
  });

  it("fences registration before its reply and keeps failed upgrades conservative across eviction", async () => {
    const stub = session();
    await runInDurableObject(stub, async (agent, state) => {
      await initialize(agent, state);
      const denied = await agent.fetch(new Request("https://session.internal/vm-host-existence", {
        headers: { "x-nanocodex-owner-id": "22222222-2222-4222-8222-222222222222" },
      }));
      expect(denied.status).toBe(404);
      await denied.body?.cancel();
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(false);
      // A mount must consult the marker after its async locator work, not retain
      // the known-empty value observed before a concurrent registration.
      const locatorWork = Promise.resolve().then(async () => {
        expect((await agent.fetch(new Request("https://session.internal/vm-host-existence"))).status).toBe(204);
      });
      await locatorWork;
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(true);
      // No factory upgrade follows: failure must never erase the fence.
    });
    await evictDurableObject(stub);
    await runInDurableObject(stub, (_agent, state) => {
      expect(shouldProbeAgentVmHostScope(state.storage, undefined)).toBe(true);
    });
  });
});
