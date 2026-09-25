import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { createHash } from "node:crypto";
import { expect, it } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import { createBrainBucket } from "../src/brain-bucket";
import { createBrainWorkspace } from "../src/brain-workspace";
import { OutputCheckpoints } from "../src/output-checkpoints";
import { SessionOperations } from "../src/session-operations";

// Checkpoint failure modes: torn writes, out-of-order completions, replay mutation,
// deletion during reads, quota pressure, cross-grant ownership, and missing permission.
// Existing file failure modes: untrusted path/body, missing sandbox authority, immutable conflicts,
// partial R2 writes, quotas, cross-agent/grant/account artifact reads, stale outputs.
const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const bucket = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;
const grantId = `0x${"a".repeat(64)}`, foreignGrant = `0x${"b".repeat(64)}`;
const actor: Principal = { kind: "connect_grant", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner", subjectId: "user:durable-files", credentialId: grantId, authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write", "tools:use"],
  connectGrant: { grantId, connectors: ["chatgpt"], mcpIds: [], sandboxExecution: true, outputCheckpoints: true } };
const req = (path: string, body?: unknown) => new Request(`https://session.internal${path}`, {
  method: body === undefined ? "GET" : "PUT", headers: { "content-type": "application/json" },
  ...(body === undefined ? {} : { body: JSON.stringify(body) }),
});
async function fixture() {
  const id = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    state.storage.sql.exec(`INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,
      authorization_epoch,public_origin,runtime_profile,last_active) VALUES (1,?,?,?,?,1,'https://nanocodex.example','managed',?)`,
      id, actor.userId, actor.organizationId, actor.teamId, Date.now());
  });
  return { id, call: (path: string, body?: unknown, principal = actor) => worker.fetch(
    new Request(`https://nanocodex.example/v1/agents/${id}/${path}`, { method: body === undefined ? "GET" : "PUT",
      headers: { origin: "https://nanocodex.example", "content-type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }) }),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext(), principal) };
}

const hash = (text: string) => createHash("sha256").update(text).digest("hex");
async function writeRevision(id: string, revision: number, content = "STEP", declared = content) {
  const root = `brains/${id}/connect/${grantId}/outputs/own/checkpoints/`;
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    const storage = createBrainBucket(state.storage, bucket, id);
    await storage.put(`${root}r${revision}/model.step`, content);
    await storage.put(`${root}latest.json`, JSON.stringify({ revision, files: [
      { path: `r${revision}/model.step`, sha256: hash(declared), size: declared.length },
    ] }));
  });
}
it("serves coherent monotonic snapshots across observer loss, rejecting wrong owners and missing authority", async () => {
  const { id, call } = await fixture();
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    const ops = new SessionOperations(state.storage); ops.retainTurnOwner("own", grantId); ops.retainTurnOwner("foreign", foreignGrant);
  });
  expect((await call("checkpoints?turn_id=own")).status).toBe(204);
  await writeRevision(id, 1);
  const first = await call("checkpoints?turn_id=own&after=0");
  expect(first.status).toBe(200);
  expect(await first.json()).toMatchObject({ turn_id: "own", revision: 1, files: [{ path: "r1/model.step", data_base64: btoa("STEP") }] });
  expect((await call("checkpoints?turn_id=own&after=1")).status).toBe(304);
  await writeRevision(id, 2, "torn", "complete");
  expect(await (await call("checkpoints?turn_id=own")).json()).toMatchObject({ revision: 1 });
  await writeRevision(id, 3, "NEW");
  expect(await (await call("checkpoints?turn_id=own")).json()).toMatchObject({ revision: 3 });
  await writeRevision(id, 1, "replay");
  expect(await (await call("checkpoints?turn_id=own")).json()).toMatchObject({ revision: 3 });
  await writeRevision(id, 3, "changed");
  expect(await (await call("checkpoints?turn_id=own")).json()).toMatchObject({ revision: 3, files: [{ data_base64: btoa("NEW") }] });
  for (const path of ["checkpoints?turn_id=foreign", "checkpoints?turn_id=missing"]) expect((await call(path)).status).toBe(404);
  expect((await call("checkpoints?turn_id=own", undefined, { ...actor, connectGrant: { ...actor.connectGrant!, grantId: foreignGrant } })).status).toBe(404);
  expect((await call("checkpoints?turn_id=own", undefined, { ...actor, connectGrant: { ...actor.connectGrant!, outputCheckpoints: undefined } })).status).toBe(403);
  await runInDurableObject(sessions.getByName(id), async (session, state) => {
    expect((await session.fetch(req("/checkpoints?turn_id=own"))).status).toBe(403);
    // Separate retained rows are still readable with a fresh service after hot history disappears.
    const restored = new OutputCheckpoints(state.storage);
    expect((await restored.get(req("/checkpoints?turn_id=own"), grantId, createBrainBucket(state.storage, bucket, id), id)).status).toBe(200);
  });
});
it("bounds manifest/file reads, rejects traversal and malformed selectors, and excludes previews from final publication", async () => {
  const { id, call } = await fixture();
  await runInDurableObject(sessions.getByName(id), async (_session, state) => { new SessionOperations(state.storage).retainTurnOwner("own", grantId); });
  for (const path of ["checkpoints", "checkpoints?turn_id=own&turn_id=foreign", "checkpoints?turn_id=own&after=-1", "checkpoints?turn_id=own&after=01", "checkpoints?turn_id=own&path=/brain/secret", "checkpoints?turn_id=.."])
    expect((await call(path)).status).toBe(400);
  expect((await call("checkpoints?turn_id=own", {})).status).toBe(405);
  const root = `brains/${id}/connect/${grantId}/outputs/own/checkpoints/`;
  const put = async (key: string, value: string) => runInDurableObject(sessions.getByName(id), async (_session, state) => {
    await createBrainBucket(state.storage, bucket, id).put(key, value);
  });
  for (const path of ["../secret", "/brain/secret", "r1/../secret", "r1/.hidden", "r2/model.step", "r1/a/b"]) {
    await put(`${root}latest.json`, JSON.stringify({ revision: 1, files: [{ path, sha256: hash(""), size: 0 }] }));
    expect((await call("checkpoints?turn_id=own")).status).toBe(204);
  }
  await writeRevision(id, 1, "x".repeat(1_000_001));
  expect((await call("checkpoints?turn_id=own")).status).toBe(204);
  await put(`${root}latest.json`, "x".repeat(16_385));
  expect((await call("checkpoints?turn_id=own")).status).toBe(204);
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    const workspace = createBrainWorkspace(bucket, id), ops = new SessionOperations(state.storage);
    await workspace.writeFile(`/brain/connect/${grantId}/outputs/own/model.step`, "FINAL");
    for (let i = 0; i < 101; i++) await bucket.put(`${root}ignored-${i}`, "preview");
    await ops.publish("own", workspace);
    const final = await ops.artifacts(req("/artifacts?turn_id=own"), grantId).json<{ data: unknown[]; publications: unknown[] }>();
    expect(final.data).toHaveLength(1); expect(final.publications).toMatchObject([{ state: "ready" }]);
  });
});
it("does not return or resurrect a snapshot after deletion while a read is pending", async () => {
  const { id } = await fixture(); await writeRevision(id, 1);
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    new SessionOperations(state.storage).retainTurnOwner("own", grantId);
    const checkpoints = new OutputCheckpoints(state.storage); let active = true;
    let release!: () => void, entered!: () => void;
    const started = new Promise<void>(r => { entered = r; });
    const blocked = new Promise<void>(r => { release = r; });
    const paused = { get: async (...args: Parameters<R2Bucket["get"]>) => {
      entered(); await blocked; return createBrainBucket(state.storage, bucket, id).get(...args);
    } } as R2Bucket;
    const reading = checkpoints.get(req("/checkpoints?turn_id=own"), grantId, paused, id, () => active);
    await started; active = false;
    state.storage.sql.exec("DELETE FROM managed_output_checkpoints");
    state.storage.sql.exec("DELETE FROM managed_turn_file_owners");
    release(); expect((await reading).status).toBe(404);
    expect(state.storage.sql.exec("SELECT * FROM managed_output_checkpoints").toArray()).toEqual([]);
  });
});

it("retains maximum bundles beyond the SQLite row limit and rejects an oversized replacement", async () => {
  const { id, call } = await fixture();
  const root = `brains/${id}/connect/${grantId}/outputs/own/checkpoints/`;
  const content = "a".repeat(1_000_000);
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    new SessionOperations(state.storage).retainTurnOwner("own", grantId);
    const storage = createBrainBucket(state.storage, bucket, id);
    const files = [];
    for (let i = 0; i < 4; i++) {
      const path = `r1/file${i}.bin`; await storage.put(root + path, content);
      files.push({ path, size: content.length, sha256: hash(content) });
    }
    await storage.put(root + "latest.json", JSON.stringify({ revision: 1, files }));
  });
  const first = await (await call("checkpoints?turn_id=own")).json<{ revision: number; files: { data_base64: string }[] }>();
  expect(first.revision).toBe(1); expect(first.files).toHaveLength(4);
  expect(first.files.every(file => atob(file.data_base64) === content)).toBe(true);
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    const storage = createBrainBucket(state.storage, bucket, id);
    const rows = state.storage.sql.exec<{ size: number }>("SELECT length(body) AS size FROM managed_output_checkpoint_chunks").toArray();
    expect(rows).toHaveLength(6); expect(rows.every(row => row.size <= 1_000_000)).toBe(true);
    const files = Array.from({ length: 5 }, (_, i) => ({ path: `r2/file${i}.bin`, size: content.length, sha256: hash(content) }));
    await storage.put(root + "latest.json", JSON.stringify({ revision: 2, files }));
    const fresh = new OutputCheckpoints(state.storage);
    expect((await fresh.get(req("/checkpoints?turn_id=own&after=1"), grantId, storage, id)).status).toBe(304);
  });
});
