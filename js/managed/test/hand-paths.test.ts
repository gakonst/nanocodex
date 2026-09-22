import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { HandPaths, readableHandRoot } from "../src/hand-paths";
import type { DurableAgentSession } from "../src/index";

it("projects VM screen labels only for the owning account and exact allocation identity", async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  const stub = sessions.getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (_, state) => {
    state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
      authorization_epoch,public_origin,runtime_profile,accepted_turns,last_active)
      VALUES(1,?,'owner','org','team',1,'https://test.example','managed',0,?)`, crypto.randomUUID(), Date.now());
    state.storage.sql.exec(`INSERT INTO managed_mounts(id,provider,name,root,provider_resource_id,configuration_json,state,created_at,updated_at)
      VALUES(?,'host','demo','/vm-omarchy-demo',?,?,'mounted',1,1)`, crypto.randomUUID(), crypto.randomUUID(),
      JSON.stringify({ vm_factory_name: "omarchy", vm_host: { pool_locator: "a".repeat(43), allocation_id: crypto.randomUUID(),
        generation: 1, machine_id: "vm:allocated" } }));
  });
  expect(await stub.vmHostDisplayName("owner", "vm:allocated")).toBe("omarchy / demo");
  expect(await stub.vmHostDisplayName("other", "vm:allocated")).toBeUndefined();
  expect(await stub.vmHostDisplayName("owner", "vm:another")).toBeUndefined();
});

it("retains readable names by identity across rename, discovery reordering, and restart", async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_, state) => {
    const paths = new HandPaths(state.storage);
    const first = paths.assign([{ id: "uuid-1", name: "Omarchy Desktop" }, { id: "uuid-2", name: "Gak 9" }]);
    expect(first.get("uuid-1")).toBe("/omarchy-desktop");
    expect(first.get("uuid-2")).toBe("/gak-9");
    const next = new HandPaths(state.storage).assign([{ id: "uuid-3", name: "Gak 9" }, { id: "uuid-1", name: "Renamed" }]);
    expect(next.get("uuid-1")).toBe("/omarchy-desktop");
    expect(next.get("uuid-3")).toBe("/gak-9-2");
    expect(next.get("uuid-2")).toBe("/gak-9");
  });
});

it("does not steal another Hand's legacy path, a VM path, or a reserved namespace", async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_, state) => {
    const paths = new HandPaths(state.storage).assign([
      { id: "aaa", name: "laptop" }, { id: "laptop", name: "Real Laptop" },
      { id: "zzz", name: "vm-omarchy-demo" }, { id: "brain-id", name: "brain" },
    ], ["/vm-omarchy-demo"]);
    expect(paths.get("aaa")).toBe("/laptop-2");
    expect(paths.get("laptop")).toBe("/real-laptop");
    expect(paths.get("zzz")).toBe("/vm-omarchy-demo-2");
    expect(paths.get("brain-id")).toBe("/hand-brain");
    expect(readableHandRoot("Venu 3 · Health")).toBe("/venu-3-health");
    expect(readableHandRoot("../../CON")).toBe("/hand-con");
  });
});
