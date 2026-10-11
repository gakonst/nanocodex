import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { readD1Migrations } from "@cloudflare/vitest-pool-workers";
import { fetch } from "./support/miniflare-fetch.mjs";

// Real account ingress, managed HTTP/WS, Code Mode, D1 receipts and /brain
// storage. Only the model, account enrollment and OpenRouter are synthetic.
const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/openrouter-video-journey", `${Date.now()}-${process.pid}`);
const apiKey = "sk-or-synthetic-video-journey-DO-NOT-EXPOSE";
const source = `
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools, ManagedAgentOwnership } from './src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from './src/account-auth.ts';
export { DurableAgentSession, AccountHostedTools, ManagedAgentOwnership, UserAccount, Organization, ApiKeyRecord, NonceStorage };
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (new URL(request.url).pathname === '/__configure') {
      this.calls = (await request.json()).calls; this.index = 0; return new Response(null,{status:204});
    }
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client, server] = Object.values(new WebSocketPair()); server.accept();
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', () => {
      const input = this.calls?.[this.index++];
      const output = input ? [{type:'custom_tool_call',name:'exec',call_id:'video-'+this.index,input}]
        : [{type:'message',role:'assistant',content:[{type:'output_text',text:'VIDEO_JOURNEY_DONE'}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_video_'+this.index,status:'completed',end_turn:!input,
        output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default { async fetch(request, env, ctx) {
  if (new URL(request.url).pathname === '/__configure') return env.MODEL.getByName('fixture-model').fetch(request);
  if (new URL(request.url).pathname === '/__fixture') {
    const b = await request.json(); await ensureAccount(env,b.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    return Response.json(await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,
      credentialId:'fixture',capabilities:b.capabilities},'Synthetic video journey'));
  }
  return worker.fetch(request,env,ctx);
}};
`;

async function bundle(contents, resolveDir, provenance, aliases = {}) {
  const assets = [];
  const result = await build({ stdin: { contents, resolveDir }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs"), ...aliases },
    plugins: [{ name: "wasm", setup(builder) {
      builder.onResolve({ filter: /(?:\.wasm$|^nanocodex\/wasm$)/ }, async args => {
        const path = args.path === "nanocodex/wasm" ? join(root, "../nanocodex/pkg-web/nanocodex_bg.wasm") : join(args.resolveDir, args.path);
        const contents = await readFile(path), name = `fixture-${assets.length}.wasm`;
        assets.push({ type: "CompiledWasm", path: name, contents });
        provenance.push({ path, sha256: createHash("sha256").update(contents).digest("hex") });
        return { path: `./${name}`, external: true };
      });
    } }], logLevel: "silent",
  });
  return [{ type: "ESModule", path: "worker.mjs", contents: result.outputFiles[0].text }, ...assets];
}

// Public live-catalog entry shape for Seedance 2.0 (OpenRouter /api/v1/videos/models).
const seedance = {
  id: "bytedance/seedance-2.0", name: "ByteDance: Seedance 2.0", description: "Seedance 2.0 video generation",
  supported_resolutions: ["480p", "720p"], supported_aspect_ratios: ["16:9", "9:16", "1:1"],
  supported_sizes: ["1280x720", "720x1280"], supported_durations: [4, 5, 6, 8, 10, 12, 15],
  supported_frame_images: ["first_frame", "last_frame"], generate_audio: true, seed: true,
  pricing_skus: { video_tokens: "0.000007" }, allowed_passthrough_parameters: ["watermark", "return_last_frame"],
};
const clip = new Uint8Array(300_000).map((_, index) => index % 251);

test("OpenRouter video jobs are account-owned, catalog-validated, idempotent and downloadable through Code Mode", { timeout: 180_000 }, async () => {
  await mkdir(output, { recursive: true });
  const trace = [], wire = [], logs = [], provenance = [], upstream = [], submissions = [];
  const jobs = new Map();
  const unbounded = { sent: 0, cancelled: false };
  // Synthetic OpenRouter: the managed Worker's only external network boundary.
  async function openrouter(request) {
    const url = new URL(request.url), auth = request.headers.get("authorization");
    upstream.push({ method: request.method, origin: url.origin, path: url.pathname + url.search, credential: auth === null ? "none" : auth === `Bearer ${apiKey}` ? "deployment_key" : "other" });
    if (url.origin === "https://cdn.video.test") {
      assert.equal(auth, null, "provider storage redirect must not receive the deployment key");
      return new Response(clip, { headers: { "content-type": "video/mp4", "content-length": String(clip.byteLength) } });
    }
    // Default MCP discovery also leaves through this boundary; never reach real services.
    if (url.origin !== "https://openrouter.ai") return new Response("synthetic network unavailable", { status: 503 });
    assert.equal(auth, `Bearer ${apiKey}`);
    if (request.method === "GET" && url.pathname === "/api/v1/videos/models")
      return Response.json({ data: [seedance, { ...seedance, id: "google/veo-3.1", name: "Veo 3.1", seed: false }] });
    if (request.method === "POST" && url.pathname === "/api/v1/videos") {
      const body = await request.json(); submissions.push(body);
      if (body.prompt?.includes("LOSE_REPLY")) return new Response("upstream reset", { status: 502 });
      if (body.prompt?.includes("NO_CREDITS")) return Response.json({ error: { code: 402, message: "Insufficient credits" } }, { status: 402 });
      const id = `gen-vid-1789000000-${String(jobs.size + 1).padStart(20, "A")}`;
      jobs.set(id, { polls: 0, prompt: body.prompt ?? "" });
      return Response.json({ id, polling_url: `/api/v1/videos/${id}`, status: "pending" }, { status: 202 });
    }
    const content = /^\/api\/v1\/videos\/([^/]+)\/content$/.exec(url.pathname);
    if (content && jobs.has(content[1])) {
      const { prompt } = jobs.get(content[1]);
      // Bodies without content-length: a small clip, and an oversized stream that must be abandoned at the cap.
      if (prompt.includes("STREAM_SMALL")) return new Response(new Blob([clip]).stream(), { headers: { "content-type": "video/mp4" } });
      if (prompt.includes("UNBOUNDED")) {
        const chunk = new Uint8Array(1024 * 1024);
        return new Response(new ReadableStream({ pull(controller) {
          if (unbounded.sent >= 200) return controller.close();
          unbounded.sent++; controller.enqueue(chunk);
        }, cancel() { unbounded.cancelled = true; } }, { highWaterMark: 0 }), { headers: { "content-type": "video/mp4" } });
      }
      return new Response(null, { status: 302, headers: { location: "https://cdn.video.test/clip.mp4" } });
    }
    const poll = /^\/api\/v1\/videos\/([^/]+)$/.exec(url.pathname);
    if (poll && jobs.has(poll[1])) {
      const job = jobs.get(poll[1]); job.polls++;
      if (job.prompt.includes("FLAKY_STATUS") && job.polls === 1) return new Response("status backend unavailable", { status: 503 });
      return Response.json(job.polls === 1 ? { id: poll[1], polling_url: url.pathname, status: "in_progress" }
        : { id: poll[1], polling_url: url.pathname, status: "completed", generation_id: "gen-synthetic",
          unsigned_urls: [`https://openrouter.ai/api/v1/videos/${poll[1]}/content?index=0`], usage: { cost: 0.42, is_byok: false } });
    }
    return Response.json({ error: { code: 404, message: "Resource not found" } }, { status: 404 });
  }
  const modules = await bundle(source, root, provenance);
  const proxy = await bundle(`import {routeManaged} from '../account/worker/managedProxy.ts';
    export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url)) ?? new Response(null,{status:404})}}`, root, provenance);
  const egress = await bundle(`export { default, AgentSubjectDirectory, UserCredentialBroker } from './src/egress.ts';`, join(root, "../egress"), provenance,
    { "@whiskeysockets/baileys": join(root, "../egress/src/whatsapp-generated/baileys.js") });
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const mf = new Miniflare({ port: 0, handleRuntimeStdio(stdout, stderr) {
    createInterface({ input: stdout }).on("line", line => logs.push(line));
    createInterface({ input: stderr }).on("line", line => logs.push(line));
  }, workers: [
    { ...common, name: "account", modules: proxy, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { ...common, name: "managed", modules, bindings: { MANAGED_AGENT_DIRECT_CREDENTIALS: "true", OPENROUTER_API_KEY: apiKey }, durableObjects: {
      NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true },
      NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
      NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
      NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
      NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
      NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
      NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true }, MODEL: { className: "FixtureModel", useSQLite: true },
    }, serviceBindings: { NANOCODEX: "provider" }, d1Databases: { NANOCODEX_CRM: "video-journey-crm" },
    r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"], outboundService: openrouter },
    { ...common, name: "provider", modules: true, script: `export default {async fetch(request,env){
      if(new URL(request.url).hostname === 'broker.internal') return env.EGRESS.fetch(request);
      return env.MODEL.getByName('fixture-model').fetch(request);
    }};`, serviceBindings: { EGRESS: "egress" }, durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
    { ...common, name: "egress", modules: egress, bindings: { ENVIRONMENT: "test" },
      durableObjects: { USER_CREDENTIALS: { className: "UserCredentialBroker", useSQLite: true }, AGENT_SUBJECTS: { className: "AgentSubjectDirectory", useSQLite: true } },
      serviceBindings: { MANAGED_AGENT_OWNERSHIP: { name: "managed", entrypoint: "ManagedAgentOwnership" } },
      outboundService: () => { throw new Error("egress must not contact external services in this journey"); } },
  ] });
  const sockets = [];
  try {
    const base = await mf.ready, backend = await mf.getWorker("managed");
    const db = await mf.getD1Database("NANOCODEX_CRM", "managed");
    for (const migration of await readD1Migrations(join(root, "migrations"))) await db.batch(migration.queries.map(query => db.prepare(query)));
    const enroll = async (user, capabilities = ["agents:read", "agents:write", "tools:use"]) => {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user, capabilities }) });
      assert.equal(response.status, 200); return response.json();
    };
    const owner = "33333333-3333-4333-8333-333333333333", foreign = "44444444-4444-4444-8444-444444444444";
    const alice = await enroll(owner), bob = await enroll(foreign);
    async function call(path, { token = alice.token, body, method = body ? "POST" : "GET", expected = 200, raw = false } = {}) {
      const response = await fetch(new URL(path, base), { method, headers: { authorization: `Bearer ${token}`, ...(body ? { "content-type": "application/json" } : {}) },
        ...(body ? { body: JSON.stringify(body) } : {}) });
      const data = raw ? new Uint8Array(await response.arrayBuffer()) : await response.text().then(text => text ? JSON.parse(text) : null);
      trace.push({ path, method, expected, observed: response.status, result: raw ? `[${data.byteLength} bytes]` : data });
      assert.equal(response.status, expected, `${method} ${path}`);
      return data;
    }
    const createAgent = async token => (await call("/v1/agents", { token, expected: 201,
      body: { settings: { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false } } })).agent_id;
    async function turn(token, agent, toolCalls, label) {
      const configured = await backend.fetch("https://fixture.test/__configure", { method: "POST", body: JSON.stringify({
        calls: toolCalls.map(args => `text(JSON.stringify(await tools.openrouter_video(${JSON.stringify(args)})));`) }) });
      assert.equal(configured.status, 204);
      const frames = [], socket = new WebSocket(new URL(`/v1/agents/${agent}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: `Bearer ${token}` } });
      sockets.push(socket); let socketError; socket.on("error", error => { socketError = error; });
      socket.on("message", data => { const frame = JSON.parse(String(data)); frames.push(frame); wire.push({ label, frame }); });
      const waitFor = async (predicate, what) => {
        const deadline = Date.now() + 60_000;
        while (!predicate()) { if (socketError) throw socketError; assert.ok(Date.now() < deadline, `${what}: ${JSON.stringify(frames.slice(-6))}`); await delay(20); }
      };
      await waitFor(() => frames.some(frame => frame.type === "ready"), "ready");
      const id = crypto.randomUUID();
      socket.send(JSON.stringify({ type: "prompt", id, input: `Synthetic OpenRouter video journey: ${label}` }));
      await waitFor(() => frames.some(frame => frame.id === id && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type)), label);
      assert.equal(frames.find(frame => frame.id === id && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type))?.type, "turn_completed");
      socket.close();
      const results = frames.filter(frame => frame.event?.type === "tool.result" && frame.event.payload.tool === "openrouter_video").map(frame => frame.event.payload.structured_result);
      assert.equal(results.length, toolCalls.length, JSON.stringify(frames.slice(-6)));
      toolCalls.forEach((input, index) => trace.push({ case: label, input, result: results[index] }));
      return results;
    }
    const shot = crypto.randomUUID(), lost = crypto.randomUUID(), broke = crypto.randomUUID();
    const request = { operation: "submit", operation_id: shot, model: "bytedance/seedance-2.0",
      prompt: "Slow dolly through a glowing lattice of validator nodes, cinematic motion", duration: 5,
      resolution: "720p", aspect_ratio: "16:9", generate_audio: false, seed: 7,
      frame_images: [{ frame_type: "first_frame", url: "https://assets.example.test/frame.png" }] };
    const aliceAgent = await createAgent(alice.token);
    const first = await turn(alice.token, aliceAgent, [
      { operation: "models", q: "seedance" },
      { ...request, duration: 7 },
      { ...request, model: "google/veo-3.1" },
      request,
      request,
      { ...request, prompt: "A different shot" },
      { operation: "status", operation_id: shot },
      { operation: "status", operation_id: shot },
      { operation: "download", operation_id: shot },
      { ...request, operation_id: lost, prompt: "LOSE_REPLY shot" },
      { ...request, operation_id: lost, prompt: "LOSE_REPLY shot" },
      { ...request, operation_id: broke, prompt: "NO_CREDITS shot" },
    ], "owner_journey");
    const [models, badDuration, badSeed, submitted, replay, conflict, running, completed, downloaded, unknown, unknownReplay, rejected] = first;
    assert.deepEqual(models.models.map(model => model.id), ["bytedance/seedance-2.0"]);
    assert.deepEqual(models.models[0].supported_durations, seedance.supported_durations);
    assert.equal(badDuration.error, "invalid_request"); assert.match(badDuration.message, /does not support duration 7/);
    assert.equal(badSeed.error, "invalid_request"); assert.match(badSeed.message, /does not support seed/);
    assert.deepEqual(submitted, { operation_id: shot, model: "bytedance/seedance-2.0", state: "submitted", status: "pending" });
    assert.deepEqual(replay, { ...submitted, replayed: true });
    assert.equal(conflict.error, "invalid_request"); assert.match(conflict.message, /different arguments/);
    assert.equal(running.status, "in_progress"); assert.equal(running.status_source, "provider");
    assert.deepEqual(completed, { operation_id: shot, model: "bytedance/seedance-2.0", state: "submitted", status: "completed", outputs: 1, cost_usd: 0.42, status_source: "provider" });
    const path = `/brain/outputs/videos/${shot}.mp4`;
    const { status_source: _source, ...completedReceipt } = completed;
    assert.deepEqual(downloaded, { ...completedReceipt, path, bytes: clip.byteLength, content_type: "video/mp4" });
    assert.equal(unknown.state, "outcome_unknown"); assert.equal(unknownReplay.state, "outcome_unknown"); assert.equal(unknownReplay.replayed, true);
    assert.equal(rejected.state, "rejected"); assert.match(rejected.error, /^openrouter_402: Insufficient credits/);
    // Exactly one paid POST per distinct operation; invalid, replayed and conflicting calls never reach OpenRouter.
    assert.equal(submissions.length, 3, JSON.stringify(submissions));
    assert.deepEqual(submissions[0], { model: "bytedance/seedance-2.0", prompt: request.prompt, duration: 5, resolution: "720p", aspect_ratio: "16:9",
      generate_audio: false, seed: 7, frame_images: [{ type: "image_url", image_url: { url: "https://assets.example.test/frame.png" }, frame_type: "first_frame" }], session_id: shot });
    const stored = await call(`/v1/agents/${aliceAgent}/files?${new URLSearchParams({ path })}`, { raw: true });
    assert.deepEqual(stored, clip, "downloaded clip is readable from the owner's /brain over HTTP");

    // Another account cannot address the owner's receipts, jobs or files.
    const openrouterCalls = () => upstream.filter(call => call.origin === "https://openrouter.ai").length;
    const before = openrouterCalls();
    const bobAgent = await createAgent(bob.token);
    const foreignResults = await turn(bob.token, bobAgent, [
      { operation: "status", operation_id: shot },
      { operation: "download", operation_id: shot },
      { ...request, operation_id: lost, prompt: "LOSE_REPLY shot", previous_operation_id: shot },
    ], "foreign_account");
    for (const result of foreignResults) assert.deepEqual(result, { error: "invalid_request", message: "unknown operation_id for this account" });
    assert.equal(openrouterCalls(), before, "foreign reads never contact OpenRouter");
    await call(`/v1/agents/${aliceAgent}/files?${new URLSearchParams({ path })}`, { token: bob.token, expected: 404 });
    // Status freshness is explicit when the provider poll fails, and unknown-length bodies are bounded while streaming.
    const flaky = "33333333-3333-4333-8333-333333333333", huge = "44444444-4444-4444-8444-444444444444", small = "55555555-5555-4555-8555-555555555555";
    const robustness = await turn(alice.token, aliceAgent, [
      { ...request, operation_id: flaky, prompt: "FLAKY_STATUS shot" },
      { operation: "status", operation_id: flaky },
      { operation: "status", operation_id: flaky },
      { operation: "status", operation_id: flaky },
      { ...request, operation_id: huge, prompt: "UNBOUNDED shot" },
      { operation: "status", operation_id: huge },
      { operation: "download", operation_id: huge },
      { ...request, operation_id: small, prompt: "STREAM_SMALL shot" },
      { operation: "status", operation_id: small },
      { operation: "download", operation_id: small },
    ], "robustness");
    const [, staleStatus, freshStatus, terminalStatus, , , hugeDownload, , , smallDownload] = robustness;
    assert.equal(staleStatus.stale, true); assert.equal(staleStatus.status_source, "stale_receipt");
    assert.equal(staleStatus.refresh_error, "openrouter_status_503"); assert.equal(staleStatus.status, "pending"); assert.equal(typeof staleStatus.status_as_of, "number");
    assert.equal(freshStatus.status, "completed"); assert.equal(freshStatus.status_source, "provider"); assert.equal(freshStatus.stale, undefined);
    assert.equal(terminalStatus.status_source, "durable_terminal");
    assert.match(hugeDownload.error, /without content-length exceeds 96 MiB/); assert.equal(hugeDownload.path, undefined);
    // Miniflare's Node outbound bridge drains the synthetic body itself; the Worker-side bound is the error above and the absent file below.
    assert.equal(smallDownload.bytes, clip.byteLength); assert.equal(smallDownload.path, `/brain/outputs/videos/${small}.mp4`);
    assert.deepEqual(await call(`/v1/agents/${aliceAgent}/files?${new URLSearchParams({ path: smallDownload.path })}`, { raw: true }), clip);
    await call(`/v1/agents/${aliceAgent}/files?${new URLSearchParams({ path: `/brain/outputs/videos/${huge}.mp4` })}`, { expected: 404 });
    trace.push({ case: "robustness", unbounded, results: robustness });
    const rows = await db.prepare("SELECT owner_id, operation_id, state, job_status FROM openrouter_video_jobs ORDER BY created_at, operation_id").all();
    assert.deepEqual(rows.results.map(row => [row.owner_id, row.state]).sort(), [[owner, "outcome_unknown"], [owner, "rejected"],
      [owner, "submitted"], [owner, "submitted"], [owner, "submitted"], [owner, "submitted"]]);
    trace.push({ case: "d1_receipts", rows: rows.results });
    console.log(JSON.stringify({ evidence: output, paid_submissions: submissions.length, unbounded_mib_sent: unbounded.sent, unbounded_cancelled: unbounded.cancelled, replays_without_upstream: 2, foreign_reads_blocked: 3,
      downloaded_bytes: clip.byteLength, redirect_without_credential: upstream.some(call => call.origin === "https://cdn.video.test" && call.credential === "none") }));
  } finally {
    for (const socket of sockets) socket.terminate();
    await mf.dispose();
    const evidence = JSON.stringify({ command: "pnpm --dir js/managed test:openrouter-video", provenance, trace, upstream, submissions, wire }, null, 2);
    const runtime = logs.join("\n");
    await writeFile(join(output, "trace.json"), evidence + "\n"); await writeFile(join(output, "runtime.log"), runtime + "\n");
    assert.ok(!evidence.includes(apiKey), "transcript must not contain the deployment key");
    assert.ok(!runtime.includes(apiKey), "runtime logs must not contain the deployment key");
  }
});
