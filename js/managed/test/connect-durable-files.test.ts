import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { createHash } from "node:crypto";
import { expect, it } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import { createBrainBucket } from "../src/brain-bucket";
import { createBrainWorkspace } from "../src/brain-workspace";
import { ConnectInputs } from "../src/connect-inputs";
import { SessionOperations } from "../src/session-operations";
import { ManagedTurnArchive } from "../src/managed-turn-archive";

// Failure modes: untrusted path/body, missing sandbox authority, immutable conflicts,
// partial R2 writes, quotas, cross-agent/grant/account artifact reads, stale outputs.
const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const bucket = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;
const grantId = `0x${"a".repeat(64)}`, foreignGrant = `0x${"b".repeat(64)}`;
const actor: Principal = { kind: "connect_grant", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner", subjectId: "user:durable-files", credentialId: grantId, authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write", "tools:use"],
  connectGrant: { grantId, connectors: ["chatgpt"], mcpIds: [], sandboxExecution: true } };
const encoded = (value: string) => ({ data_base64: btoa(value), sha256: createHash("sha256").update(value, "binary").digest("hex") });
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

it("admits bounded immutable inputs through HTTP and repeats authority at the DO", async () => {
  const { id, call } = await fixture(), generation = crypto.randomUUID();
  const path = `inputs/${generation}/model.step`, body = encoded("\0\xffSTEP");
  const uploaded = await call(path, body);
  expect(uploaded.status).toBe(201);
  const receipt = await uploaded.json<{ path: string; sha256: string; size: number }>();
  expect(receipt).toEqual({ path: `/brain/connect/${grantId}/inputs/${generation}/model.step`, sha256: body.sha256, size: 6 });
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    const workspace = createBrainWorkspace(createBrainBucket(state.storage, bucket, id), id);
    expect(await workspace.readFile(receipt.path)).toEqual(new Uint8Array([0,255,83,84,69,80]));
  });
  expect((await call(path, body)).status).toBe(200);
  expect((await call(path, encoded("different"))).status).toBe(409);
  expect((await call(`inputs/${generation}/wrong.step`, { ...body, sha256: "0".repeat(64) })).status).toBe(400);
  expect((await call(path, { ...body, path: "/brain/secret" })).status).toBe(400);
  expect((await call(`inputs/${generation}/large.step`, encoded("x".repeat(600_001)))).status).toBe(413);
  expect((await call(`inputs/${generation}/large.step`, { data_base64: "x".repeat(1_000_001), sha256: body.sha256 })).status).toBe(413);
  for (const name of ["%2fsecret", "%252e%252e", "a%5cb", "bad name", ".hidden", "a/b"]) {
    expect((await call(`inputs/${generation}/${name}`, body)).status).toBe(400);
  }
  for (const principal of [{ ...actor, connectGrant: { ...actor.connectGrant!, sandboxExecution: undefined } },
    { ...actor, capabilities: ["agents:read"] as const }, { ...actor, connectGrant: undefined, kind: "api_key" as const }]) {
    expect((await call(path, body, principal)).status).toBe(403);
  }
  expect((await call(path, body, { ...actor, userId: crypto.randomUUID() })).status).toBe(404);
  for (const name of ["files?path=/brain/secret", "configuration", "attachments/anything"]) expect((await call(name)).status).toBe(403);
  await runInDurableObject(sessions.getByName(id), async (session) => {
    expect((await session.fetch(req(`/${path}`, body))).status).toBe(403);
    const headers = new Headers(); forwardPrincipalAssertions(headers, actor);
    for (const path of ["/files?path=/brain/secret", "/configuration"]) {
      expect((await session.fetch(new Request(`https://session.internal${path}`, { headers }))).status).toBe(403);
    }
  });
});

it("resumes a partially written input and counts pending reservations against quotas", async () => {
  const { id } = await fixture(), generation = crypto.randomUUID();
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    const workspace = createBrainWorkspace(bucket, id), inputs = new ConnectInputs(state.storage);
    const body = encoded("durable"), path = `/inputs/${generation}/input.py`;
    let fail = true;
    const interrupted = { ...workspace, writeFile: async (...args: Parameters<typeof workspace.writeFile>) => {
      await workspace.writeFile(...args); if (fail) { fail = false; throw new Error("lost R2 receipt"); }
    } };
    expect((await inputs.put(req(path, body), grantId, interrupted)).status).toBe(503);
    expect((await inputs.put(req(path, encoded("replacement")), grantId, workspace)).status).toBe(409);
    expect((await new ConnectInputs(state.storage).put(req(path, body), grantId, workspace)).status).toBe(200);
    for (let i = 1; i < 8; i++) expect((await inputs.put(req(`/inputs/${generation}/${i}.txt`, body), grantId, workspace)).status).toBe(201);
    expect((await inputs.put(req(`/inputs/${generation}/overflow.txt`, body), grantId, workspace)).status).toBe(413);
    await workspace.writeFile(`/brain/connect/${grantId}/inputs/${generation}/input.py`, "changed by a tool");
    expect((await inputs.put(req(path, body), grantId, workspace)).status).toBe(200);
    expect(new TextDecoder().decode(await workspace.readFile(`/brain/connect/${grantId}/inputs/${generation}/input.py`))).toBe("durable");
    let release!: () => void;
    let entered!: () => void;
    const started = new Promise<void>(resolve => { entered = resolve; });
    const paused = new Promise<void>(resolve => { release = resolve; });
    let active = true;
    const pending = inputs.put(req(`/inputs/${crypto.randomUUID()}/late.txt`, body), grantId,
      { ...workspace, writeFile: async (...args: Parameters<typeof workspace.writeFile>) => {
        entered(); await paused; await workspace.writeFile(...args);
      } }, () => active);
    await started;
    active = false;
    let drained = false;
    const cleanup = inputs.drain().then(() => { drained = true; });
    await Promise.resolve(); expect(drained).toBe(false);
    release(); await cleanup;
    expect((await pending).status).toBe(409);
  });
});

it("serves only the requested grant-owned publication and immutable bytes", async () => {
  const { id, call } = await fixture();
  let ownId = "", foreignId = "", accountId = "";
  await runInDurableObject(sessions.getByName(id), async (_session, state) => {
    const ops = new SessionOperations(state.storage), workspace = createBrainWorkspace(bucket, id);
    await workspace.writeFile("/brain/outputs/account.txt", "account secret");
    for (const [turn, grant] of [["own", grantId], ["other", foreignGrant], ["account", null]] as const) {
      ops.retainTurnOwner(turn, grant);
      if (grant) await workspace.writeFile(`/brain/connect/${grant}/outputs/${turn}/model.step`, `${turn} bytes`);
      await ops.publish(turn, workspace);
      const page = await ops.artifacts(req(`/artifacts?turn_id=${turn}`)).json<{ data: { id: string }[] }>();
      if (turn === "own") ownId = page.data[0]!.id;
      if (turn === "other") foreignId = page.data[0]!.id;
      if (turn === "account") accountId = page.data[0]!.id;
    }
    await workspace.writeFile(`/brain/connect/${grantId}/outputs/own/model.step`, "changed live bytes");
    await ops.publish("own", workspace);
  });
  const page = await (await call("artifacts?turn_id=own")).json();
  expect(page).toMatchObject({ data: [{ id: ownId, turn_id: "own", path: `/brain/connect/${grantId}/outputs/own/model.step` }], publications: [{ turn_id: "own", state: "ready" }] });
  expect(JSON.stringify(page)).not.toContain("account");
  const content = await call(`artifacts/${ownId}/content`);
  expect(await content.text()).toBe("own bytes");
  expect(content.headers.get("content-length")).toBe("9");
  for (const path of ["artifacts?turn_id=other", "artifacts?turn_id=account", `artifacts/${foreignId}/content`, `artifacts/${accountId}/content`]) {
    expect((await call(path)).status).toBe(404);
  }
  for (const path of ["artifacts", "artifacts?turn_id=own&turn_id=other", "artifacts?turn_id=own&path=/brain/secret"]) {
    expect((await call(path)).status).toBe(400);
  }
  expect((await call(`artifacts/${ownId}/content`, undefined, { ...actor, connectGrant: { ...actor.connectGrant!, grantId: foreignGrant } })).status).toBe(404);
  const otherAgent = await fixture();
  expect((await otherAgent.call(`artifacts/${ownId}/content`)).status).toBe(404);
  console.info({ scenario: "connect.durable-files", agent: id, uploaded: true, own_artifact: ownId, foreign_denied: true, immutable: true });
});

it("finishes a Connect turn without a native file host and retains files through archival", async () => {
  const { DEFAULT_AGENT_SETTINGS } = await import("../src/agent-settings");
  const id = crypto.randomUUID(), turnId = crypto.randomUUID(), generation = crypto.randomUUID().toUpperCase();
  const inputPath = `/brain/connect/${grantId}/inputs/${generation}/source.txt`;
  const outputRoot = `/brain/connect/${grantId}/outputs/${turnId}`;
  await runInDurableObject(sessions.getByName(id), async (session, state) => {
    let step = 0;
    const transcript: unknown[] = [];
    const code = `text(await tools.exec_command({cmd: ${JSON.stringify(`mkdir -p ${outputRoot} && cat ${inputPath} > ${outputRoot}/model.step`)}}));`;
    class ModelSocket extends EventTarget {
      readyState = 1; bufferedAmount = 0;
      accept() {}
      close() { this.readyState = 3; }
      send(value: string) {
        transcript.push(JSON.parse(value));
        const output = step++ === 0
          ? [{ type: "custom_tool_call", name: "exec", call_id: "durable-files", input: code }]
          : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "DONE" }] }];
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          type: "response.completed", response: { id: `files-${step}`, status: "completed", output,
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
    } });
    const call = (path: string, method = "GET", body?: unknown, principal = actor) => {
      const headers = new Headers({ "content-type": "application/json" });
      if (path !== "/create") forwardPrincipalAssertions(headers, principal);
      return session.fetch(new Request(`https://session.internal${path}`, { method, headers,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }) }));
    };
    expect((await call("/create", "POST", { session_id: id, owner_id: actor.userId,
      organization_id: actor.organizationId, team_id: actor.teamId, authorization_epoch: 1,
      public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS, configuration: {} })).status).toBe(200);
    try {
      expect((await call(`/inputs/${generation}/source.txt`, "PUT", encoded("CLOUD-ONLY-STEP"))).status).toBe(201);
      const input = "Use the uploaded source and publish its bytes to the scoped output directory.";
      expect((await call("/turns", "POST", { id: turnId, input })).status).toBe(202);
      await expect.poll(() => state.storage.sql.exec("SELECT state,error FROM managed_turns WHERE id=?", turnId).one(), { timeout: 20_000 })
        .toEqual({ state: "completed", error: null });
      const page = await (await call(`/artifacts?turn_id=${turnId}`)).json<{ data: { id: string; path: string }[]; publications: unknown[] }>();
      expect(page.data).toHaveLength(1);
      expect(page.data[0]!.path).toBe(`${outputRoot}/model.step`);
      expect(await (await call(`/artifacts/${page.data[0]!.id}/content`)).text()).toBe("CLOUD-ONLY-STEP");
      expect((await call("/turns", "POST", { id: turnId, input }, { ...actor, connectGrant: { ...actor.connectGrant!, grantId: foreignGrant } })).status).toBe(403);
      expect((await call("/turns", "POST", { id: turnId, input })).status).toBe(200);
      // Publication ownership remains after the hot turn row is removed by archival.
      const historyBucket = (env as unknown as { NANOCODEX_HISTORY: R2Bucket }).NANOCODEX_HISTORY;
      const archive = new ManagedTurnArchive(state.storage, historyBucket, state.id.toString());
      expect((await archive.seal(true, 0)).sealed).toBe(true);
      expect(state.storage.sql.exec("SELECT id FROM managed_turns WHERE id=?", turnId).toArray()).toEqual([]);
      expect((await call("/turns", "POST", { id: turnId, input })).status).toBe(200);
      expect((await call("/turns", "POST", { id: turnId, input }, { ...actor, connectGrant: { ...actor.connectGrant!, grantId: foreignGrant } })).status).toBe(403);
      expect(await (await call(`/artifacts/${page.data[0]!.id}/content`)).text()).toBe("CLOUD-ONLY-STEP");
      console.info({ scenario: "connect.durable-files.runtime", turn: turnId, page, model_requests: transcript.length,
        last_model_input: (transcript.at(-1) as { input?: unknown }).input });
    } finally {
      // Retire the live runtime before the Workers pool waits for background work.
      state.storage.sql.exec("UPDATE session_state SET last_active=0");
      await session.alarm();
      await state.storage.deleteAlarm();
    }
  });
}, 60_000);
