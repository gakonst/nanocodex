import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import { SessionAttachments, ATTACHMENT_PART_BYTES } from "../src/attachments";
import { createBrainWorkspace } from "../src/brain-workspace";
import type { Principal } from "../src/account-auth";

const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const bucket = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;
const metadata = (size: number) => ({ name: "Video with audio.mp4", media_type: "video/mp4", size });
const request = (method: string, body?: BodyInit, headers?: HeadersInit) => new Request("https://session.internal/attachment", { method, body, headers });

it("resumes original bytes across reconstruction, fences conflicting retries, and mounts the completed file", async () => {
  const agent = crypto.randomUUID(), id = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(agent), async (_session, ctx) => {
    let store = new SessionAttachments(ctx.storage, bucket, agent, () => true);
    const bytes = new Uint8Array(ATTACHMENT_PART_BYTES + 127);
    bytes.fill(17); bytes.set([0, 255, 3, 99], ATTACHMENT_PART_BYTES);
    const begin = () => store.fetch(request("POST", JSON.stringify(metadata(bytes.length))), id);
    expect(await (await begin()).json()).toMatchObject({ next_part: 1, complete: false });
    expect((await store.fetch(request("POST"), id, "complete")).status).toBe(409);
    expect((await store.fetch(request("PUT", bytes.slice(ATTACHMENT_PART_BYTES)), id, "parts/2")).status).toBe(409);
    expect((await store.fetch(request("PUT", bytes.slice(0, ATTACHMENT_PART_BYTES)), id, "parts/1")).status).toBe(200);
    store = new SessionAttachments(ctx.storage, bucket, agent, () => true);
    expect(await (await begin()).json()).toMatchObject({ next_part: 2, complete: false });
    expect((await store.fetch(request("POST", JSON.stringify(metadata(bytes.length + 1))), id)).status).toBe(409);
    expect((await store.fetch(request("PUT", new Uint8Array(ATTACHMENT_PART_BYTES)), id, "parts/1")).status).toBe(409);
    expect((await store.fetch(request("PUT", bytes.slice(0, ATTACHMENT_PART_BYTES)), id, "parts/1")).status).toBe(200);
    expect((await store.fetch(request("PUT", bytes.slice(ATTACHMENT_PART_BYTES)), id, "parts/2")).status).toBe(200);
    for (let attempt = 0; attempt < 2; attempt++) {
      expect(await (await store.fetch(request("POST"), id, "complete")).json()).toMatchObject({ complete: true, size: bytes.length, path: `/brain/attachments/${id}/original.mp4` });
    }
    expect(await (await begin()).json()).toMatchObject({ complete: true });
    const response = await store.fetch(request("GET"), id);
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("private, no-store");
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    expect(await crypto.subtle.digest("SHA-256", await response.arrayBuffer())).toEqual(digest);
    const range = await store.fetch(request("GET", undefined, { range: `bytes=${ATTACHMENT_PART_BYTES}-${ATTACHMENT_PART_BYTES + 3}` }), id);
    expect(range.status).toBe(206);
    expect(range.headers.get("content-range")).toBe(`bytes ${ATTACHMENT_PART_BYTES}-${ATTACHMENT_PART_BYTES + 3}/${bytes.length}`);
    expect(new Uint8Array(await range.arrayBuffer())).toEqual(new Uint8Array([0, 255, 3, 99]));
    expect(await crypto.subtle.digest("SHA-256", await createBrainWorkspace(bucket, agent).readFile(`/brain/attachments/${id}/original.mp4`))).toEqual(digest);
    await expect(createBrainWorkspace(bucket, crypto.randomUUID()).readFile(`/brain/attachments/${id}/original.mp4`)).rejects.toMatchObject({ code: "ENOENT" });
    await store.cleanup(); // Completed multipart uploads must not break deletion.
    await bucket.delete(`brains/${agent}/attachments/${id}/original.mp4`);
  });
}, 30_000);

it("bounds request bodies and aborts an upload when deletion races part delivery", async () => {
  const agent = crypto.randomUUID(), id = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(agent), async (_session, ctx) => {
    let active = true;
    const store = new SessionAttachments(ctx.storage, bucket, agent, () => active);
    expect((await store.fetch(request("POST", JSON.stringify({ ...metadata(4), name: "../bad" })), id)).status).toBe(400);
    expect((await store.fetch(request("POST", " ".repeat(2049)), id)).status).toBe(413);
    expect((await store.fetch(request("POST", JSON.stringify(metadata(4))), id)).status).toBe(200);
    expect((await store.fetch(request("PUT", new Uint8Array(5)), id, "parts/1")).status).toBe(413);
    const retained = (await ctx.storage.get<{ key: string; uploadId: string; created: number }>("attachment:" + id))!;
    await bucket.resumeMultipartUpload(retained.key, retained.uploadId).abort();
    await ctx.storage.put("attachment:" + id, { ...retained, created: 0 });
    expect(await (await store.fetch(request("POST", JSON.stringify(metadata(4))), id)).json()).toMatchObject({ next_part: 1, complete: false });
    const part = store.fetch(request("PUT", new ReadableStream<Uint8Array>()), id, "parts/1");
    await new Promise((resolve) => setTimeout(resolve, 0));
    active = false;
    const cleanup = store.cleanup();
    expect((await part).status).toBe(409);
    await cleanup;
    expect((await store.fetch(request("POST"), id, "complete")).status).toBe(409);
    expect((await bucket.list({ prefix: `brains/${agent}/` })).objects).toEqual([]);
    await store.cleanup(); // Aborting an already-aborted upload is idempotent.
  });
});

it("authenticates the HTTP attachment route, account scope, epoch, capabilities, and mutation origin", async () => {
  const id = crypto.randomUUID(), attachment = crypto.randomUUID();
  const principal: Principal = { kind: "api_key", userId: crypto.randomUUID(), organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: "api_key:attachment-test", credentialId: "attachment-test", authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"] };
  await runInDurableObject(sessions.getByName(id), async (session, ctx) => {
    ctx.storage.sql.exec(`INSERT INTO session_state (singleton, session_id, owner_id, organization_id, team_id,
      authorization_epoch, public_origin, runtime_profile, last_active) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`,
    id, principal.userId, principal.organizationId, principal.teamId, Date.now());
    expect((await session.fetch(new Request(`https://session.internal/attachments/${attachment}`))).status).toBe(404);
  });
  const call = (method: string, actor = principal, body?: BodyInit, action = "", origin?: string) => worker.fetch(
    new Request(`https://nanocodex.example/v1/agents/${id}/attachments/${attachment}${action}`, { method, body, headers: origin ? { origin } : {} }),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  const body = JSON.stringify(metadata(4));
  for (const actor of [{ ...principal, userId: crypto.randomUUID() }, { ...principal, teamId: crypto.randomUUID() },
    { ...principal, organizationId: crypto.randomUUID() }, { ...principal, authorizationEpoch: 2 }]) {
    expect((await call("POST", actor, body)).status).toBe(404);
  }
  expect((await call("POST", { ...principal, capabilities: ["agents:write"] }, body)).status).toBe(403);
  expect((await call("GET", { ...principal, capabilities: ["agents:write"] })).status).toBe(403);
  expect((await call("POST", { ...principal, kind: "account_session" }, body, "", "https://evil.example")).status).toBe(403);
  expect((await call("POST", { ...principal, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: [], mcpIds: [] } }, body)).status).toBe(403);
  expect((await call("POST", principal, body)).status).toBe(200);
  expect((await call("PUT", principal, new Uint8Array([1, 2, 3, 4]), "/parts/1")).status).toBe(200);
  expect((await call("POST", principal, undefined, "/complete")).status).toBe(200);
  expect(new Uint8Array(await (await call("GET")).arrayBuffer())).toEqual(new Uint8Array([1, 2, 3, 4]));
  expect((await call("GET", { ...principal, userId: crypto.randomUUID() })).status).toBe(404);
  await bucket.delete(`brains/${id}/attachments/${attachment}/original.mp4`);
});
