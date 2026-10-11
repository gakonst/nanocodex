import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { appendFileSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { fetch } from "./support/miniflare-fetch.mjs";

// Reproduce: pnpm --filter nanocodex-managed-service run test:session-control
// Only identity enrollment and the external model are fixtures. Public HTTP,
// authentication, the session_control tool, Code Mode and both sessions'
// production turn lifecycles run in workerd.
const root = fileURLToPath(new URL("..", import.meta.url));
const evidence = join(root, "../../output/session-control-journey", `${Date.now()}-${process.pid}`);
const alice = "22222222-2222-4222-8222-222222222201";
const bob = "22222222-2222-4222-8222-222222222202";
const caps = ["agents:read", "agents:write", "tools:use"];
const source = `
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools } from './src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from './src/account-auth.ts';
export { DurableAgentSession, AccountHostedTools, UserAccount, Organization, ApiKeyRecord, NonceStorage };
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);
export class FixtureSandbox extends DurableObject {
  async clearRemoteDesktop() {}
  async destroy() {}
}
// One scripted model for every session: a user message carrying
// SESSION_TOOL <base64 code> runs that Code Mode program once; SLOW_TARGET
// keeps a target turn active long enough to steer it.
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client, server] = Object.values(new WebSocketPair()); server.accept();
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', async event => {
      const body = JSON.parse(event.data);
      const items = body.input ?? [];
      const last = items.at(-1);
      const user = JSON.stringify(items.filter(item => item.role === 'user').at(-1) ?? '');
      const followUp = last && last.role !== 'user' && String(last.type ?? '').endsWith('output');
      if (!followUp && user.includes('SLOW_TARGET')) await new Promise(resolve => setTimeout(resolve, 4000));
      const marker = user.match(/SESSION_TOOL ([A-Za-z0-9+/=]+)/);
      const input = marker && !followUp ? atob(marker[1]) : undefined;
      const reply = user.includes('STEER_TEXT') ? 'STEERED_REPLY' : 'JOURNEY_DONE';
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_'+crypto.randomUUID(),status:'completed',end_turn:!input,
        output:input ? [{type:'custom_tool_call',name:'exec',call_id:'call-'+crypto.randomUUID(),input}]
          : [{type:'message',role:'assistant',content:[{type:'output_text',text:reply}]}],
        usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default { async fetch(request, env, ctx) {
  if (new URL(request.url).pathname === '/__fixture') {
    const b = await request.json(); await ensureAccount(env,b.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    return Response.json(await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,
      credentialId:'fixture',capabilities:b.capabilities},'Synthetic session control journey'));
  }
  if (new URL(request.url).pathname === '/__archive') {
    const b = await request.json();
    return env.NANOCODEX_SESSIONS.getByName(b.agent).fetch('https://session.internal/events/archive', { method: 'POST' });
  }
  return worker.fetch(request,env,ctx);
}};
`;

test("session_control drives another owned session through the managed turn lifecycle", { timeout: 180_000 }, async () => {
  await mkdir(evidence, { recursive: true });
  const trace = [], logs = [], assets = [], provenance = [];
  const record = entry => { trace.push(entry); appendFileSync(join(evidence, "trace.jsonl"), JSON.stringify(entry) + "\n"); };
  const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const path = join(args.resolveDir, args.path), contents = await readFile(path);
      const name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents });
      provenance.push({ path, sha256: createHash("sha256").update(contents).digest("hex") });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent",
  });
  const proxy = await build({ stdin: { contents: `import {routeManaged} from '../account/worker/managedProxy.ts';
    export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url)) ?? new Response(null,{status:404})}}`, resolveDir: root },
    bundle: true, write: false, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    external: ["cloudflare:*", "node:*"], logLevel: "silent" });
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const mf = new Miniflare({ port: 0, handleRuntimeStdio(stdout, stderr) {
    for (const stream of [stdout, stderr]) createInterface({ input: stream }).on("line", line => logs.push(line));
  }, workers: [
    { ...common, name: "account", modules: true, script: proxy.outputFiles[0].text, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { ...common, name: "managed", modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
      durableObjects: {
        NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true },
        NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
        NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
        NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
        NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
        NANOCODEX_SANDBOXES: { className: "FixtureSandbox", useSQLite: true },
        NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
        NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true },
        MODEL: { className: "FixtureModel", useSQLite: true },
      }, serviceBindings: { NANOCODEX: "provider" },
      r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES", "NANOCODEX_USER_DATA_OBJECTS"] },
    { ...common, name: "provider", modules: true,
      script: "export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}};",
      durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
  ] });
  const sockets = [];
  try {
    const base = await mf.ready, backend = await mf.getWorker("managed");
    const key = async (user, capabilities) => {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user, capabilities }) });
      assert.equal(response.status, 200, await response.clone().text());
      return (await response.json()).token;
    };
    const tokens = { alice: await key(alice, caps), bob: await key(bob, caps), aliceNoRead: await key(alice, ["agents:write", "tools:use"]) };
    async function call(token, path, method = "GET", body, expected = 200) {
      const response = await fetch(new URL(path, base), { signal: AbortSignal.timeout(15_000), method, headers: {
        "content-type": "application/json", authorization: "Bearer " + tokens[token] }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const raw = await response.text(); let data; try { data = JSON.parse(raw); } catch { data = raw; }
      record({ kind: "http", token, method, path, expected, status: response.status, data });
      assert.equal(response.status, expected, `${method} ${path}: ${raw}`);
      return data;
    }
    const create = async token => (await call(token, "/v1/agents", "POST", { settings: { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false } }, 201)).agent_id;
    const driver = await create("alice"), target = await create("alice"), foreign = await create("bob");
    record({ kind: "sessions", driver, target, foreign });
    const program = calls => "SESSION_TOOL " + Buffer.from(`const results = [];
for (const [label, args] of ${JSON.stringify(calls)}) {
  try { results.push({ label, ok: true, value: await tools.session_control(args) }); }
  catch (error) { results.push({ label, ok: false, error: String(error?.message ?? error) }); }
}
text(JSON.stringify(results));`).toString("base64");
    const results = frames => {
      const exec = frames.find(frame => frame.event?.type === "tool.result" && frame.event.payload.tool === "exec")?.event.payload;
      const output = exec?.result?.find?.(item => item.type === "input_text" && item.text.startsWith("[{"))?.text;
      assert.ok(output, "exec output: " + JSON.stringify(exec ?? frames.slice(-8)));
      return Object.fromEntries(JSON.parse(output).map(entry => [entry.label, entry]));
    };
    async function socketTurn(id, input) {
      const frames = [];
      const socket = new WebSocket(new URL(`/v1/agents/${id}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: "Bearer " + tokens.alice } });
      sockets.push(socket);
      socket.on("message", data => { const frame = JSON.parse(String(data)); frames.push(frame); appendFileSync(join(evidence, "wire.jsonl"), JSON.stringify({ session: id, ...frame }) + "\n"); });
      let error; socket.on("error", value => { error = value; });
      const waitFor = async predicate => {
        const deadline = Date.now() + 60_000;
        while (!predicate()) { if (error) throw error; assert.ok(Date.now() < deadline, "WebSocket timeout: " + JSON.stringify(frames.slice(-6))); await delay(20); }
      };
      await waitFor(() => frames.some(frame => frame.type === "ready"));
      const turnId = crypto.randomUUID();
      socket.send(JSON.stringify({ type: "prompt", id: turnId, input }));
      await waitFor(() => frames.some(frame => frame.id === turnId && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type)));
      const terminal = frames.find(frame => frame.id === turnId && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type));
      assert.equal(terminal.type, "turn_completed", JSON.stringify(terminal));
      socket.close();
      return frames;
    }
    async function settled(session, turnId, token = "alice") {
      const deadline = Date.now() + 60_000;
      for (;;) {
        const turn = await call(token, `/v1/agents/${session}/turns/${encodeURIComponent(turnId)}`);
        if (["completed", "failed", "cancelled"].includes(turn.state)) return turn;
        assert.ok(Date.now() < deadline, "turn did not settle: " + JSON.stringify(turn));
        await delay(100);
      }
    }

    // 1. Inspect, submit, replay, conflict and boundary denials from the driver.
    const first = "design-restart-" + crypto.randomUUID();
    const before = await call("alice", `/v1/agents/${target}`);
    const phase1 = results(await socketTurn(driver, program([
      ["list", { operation: "list" }],
      ["status", { operation: "status", session_id: target }],
      ["submit", { operation: "submit", session_id: target, turn_id: first, input: "TARGET_PROMPT restart design agents" }],
      ["replay", { operation: "submit", session_id: target, turn_id: first, input: "TARGET_PROMPT restart design agents" }],
      ["conflict", { operation: "submit", session_id: target, turn_id: first, input: "TARGET_PROMPT different input" }],
      ["self", { operation: "submit", session_id: driver, turn_id: "self-" + crypto.randomUUID(), input: "loop" }],
      ["self_status", { operation: "status", session_id: driver }],
      ["foreign_status", { operation: "status", session_id: foreign }],
      ["foreign_submit", { operation: "submit", session_id: foreign, turn_id: "foreign-" + crypto.randomUUID(), input: "BOB_SHOULD_NOT_SEE" }],
      ["invalid", { operation: "submit", session_id: target, turn_id: "bad id", input: "x" }],
    ])));
    record({ kind: "phase", phase: "inspect_submit_replay_denials", results: phase1 });
    assert.ok(phase1.list.ok, JSON.stringify(phase1.list));
    const listed = phase1.list.value.data;
    assert.deepEqual(listed.map(row => row.session_id).sort(), [driver, target].sort(), "only owned sessions are listed");
    assert.equal(listed.find(row => row.session_id === driver).current, true);
    assert.equal(phase1.status.value.session_id, target);
    assert.equal(phase1.status.value.accepted_turns, before.accepted_turns);
    assert.equal(phase1.submit.ok, true, JSON.stringify(phase1.submit));
    assert.equal(phase1.submit.value.created, true);
    assert.equal(phase1.submit.value.turn_id, first);
    assert.equal(phase1.replay.ok, true, JSON.stringify(phase1.replay));
    assert.equal(phase1.replay.value.created, false, "identical replay must not admit a second turn");
    assert.equal(phase1.replay.value.turn_id, first);
    assert.equal(phase1.conflict.ok, false);
    assert.match(phase1.conflict.error, /HTTP 409/);
    assert.equal(phase1.self.ok, false);
    assert.match(phase1.self.error, /current session/);
    assert.equal(phase1.self_status.ok, true, "reading the current session is allowed");
    assert.equal(phase1.foreign_status.ok, false);
    assert.match(phase1.foreign_status.error, /not found/);
    assert.equal(phase1.foreign_submit.ok, false);
    assert.match(phase1.foreign_submit.error, /not found/);
    assert.equal(phase1.invalid.ok, false);
    const firstTurn = await settled(target, first);
    assert.equal(firstTurn.state, "completed", JSON.stringify(firstTurn));
    const afterFirst = await call("alice", `/v1/agents/${target}`);
    assert.equal(afterFirst.accepted_turns, before.accepted_turns + 1, "exactly one admitted turn");
    const bobHistory = JSON.stringify(await call("bob", `/v1/agents/${foreign}/events/history`));
    assert.doesNotMatch(bobHistory, /BOB_SHOULD_NOT_SEE/);
    assert.equal((await call("bob", `/v1/agents/${foreign}`)).accepted_turns, 0);

    // 2. Read the completed turn and its events, then steer an active turn.
    const slow = "design-slow-" + crypto.randomUUID(), message = "steer-" + crypto.randomUUID();
    const phase2 = results(await socketTurn(driver, program([
      ["turn", { operation: "turn", session_id: target, turn_id: first }],
      ["events", { operation: "events", session_id: target, limit: 100 }],
      ["slow", { operation: "submit", session_id: target, turn_id: slow, input: "SLOW_TARGET keep working" }],
      ["steer", { operation: "steer", session_id: target, turn_id: slow, message_id: message, input: "STEER_TEXT also restart reviewers" }],
      ["steer_replay", { operation: "steer", session_id: target, turn_id: slow, message_id: message, input: "STEER_TEXT also restart reviewers" }],
      ["steer_receipt", { operation: "turn", session_id: target, turn_id: slow, message_id: message }],
      ["steer_conflict", { operation: "steer", session_id: target, turn_id: slow, message_id: message, input: "STEER_TEXT changed" }],
    ])));
    record({ kind: "phase", phase: "turn_events_steer", results: phase2 });
    assert.equal(phase2.turn.value.state, "completed", JSON.stringify(phase2.turn));
    assert.match(JSON.stringify(phase2.turn.value.input), /TARGET_PROMPT restart design agents/);
    assert.ok(phase2.events.ok, JSON.stringify(phase2.events));
    assert.match(JSON.stringify(phase2.events.value.data), /JOURNEY_DONE/);
    assert.ok(phase2.events.value.last_cursor);
    assert.equal(phase2.slow.value.created, true, JSON.stringify(phase2.slow));
    assert.equal(phase2.steer.ok, true, JSON.stringify(phase2.steer));
    assert.equal(phase2.steer_replay.ok, true, JSON.stringify(phase2.steer_replay));
    assert.equal(phase2.steer_receipt.value.state, "accepted", JSON.stringify(phase2.steer_receipt));
    assert.equal(phase2.steer_conflict.ok, false, JSON.stringify(phase2.steer_conflict));
    const slowTurn = await settled(target, slow);
    assert.equal(slowTurn.state, "completed", JSON.stringify(slowTurn));
    const targetHistory = JSON.stringify(await call("alice", `/v1/agents/${target}/events/history?limit=256`));
    assert.equal(targetHistory.match(/STEER_TEXT also restart reviewers/g)?.length >= 1, true, "steer reached the target");
    assert.match(targetHistory, /STEERED_REPLY/);
    const newer = results(await socketTurn(driver, program([
      ["newer", { operation: "events", session_id: target, after: String(phase2.events.value.last_cursor) }],
    ])));
    assert.match(JSON.stringify(newer.newer.value.data), /STEERED_REPLY/);
    assert.doesNotMatch(JSON.stringify(newer.newer.value.data), /TARGET_PROMPT restart design agents/);

    // 3. A turn without agents:read cannot use the tool at all.
    const denied = "denied-" + crypto.randomUUID();
    await call("aliceNoRead", `/v1/agents/${driver}/turns`, "POST", { id: denied, input: program([
      ["list", { operation: "list" }],
      ["submit", { operation: "submit", session_id: target, turn_id: "denied-submit-" + crypto.randomUUID(), input: "TARGET_DENIED" }],
    ]) }, 202);
    assert.equal((await settled(driver, denied)).state, "completed");
    const driverHistory = JSON.stringify(await call("alice", `/v1/agents/${driver}/events/history?limit=256`));
    assert.match(driverHistory, /requires current direct account root authorization with agents:read, tools:use/);
    assert.doesNotMatch(JSON.stringify(await call("alice", `/v1/agents/${target}/events/history?limit=256`)), /TARGET_DENIED/);
    const final = await call("alice", `/v1/agents/${target}`);
    assert.equal(final.accepted_turns, before.accepted_turns + 2, "only the two authorized turns reached the target");

    // 4. Byte-bounded history. Huge tool results, a stored event just above the
    // 8 KiB stand-in threshold next to a chunked (>1 MB) event, multibyte text,
    // an event above the 4 MiB readable limit and an archive boundary must
    // never produce a large page, skip or repeat a cursor, or lose content.
    const bulk = await create("alice");
    const bytesOf = value => Buffer.byteLength(typeof value === "string" ? value : JSON.stringify(value));
    const storedBytes = event => { const { cursor, created_at, turn_id, ...message } = event; return bytesOf(message); };
    async function quiet(path, method = "GET", body, expected = 200) {
      const response = await fetch(new URL(path, base), { signal: AbortSignal.timeout(30_000), method, headers: {
        "content-type": "application/json", authorization: "Bearer " + tokens.alice }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const raw = await response.text();
      record({ kind: "http_summary", method, path, expected, status: response.status, response_bytes: Buffer.byteLength(raw) });
      assert.equal(response.status, expected, method + " " + path + ": " + raw.slice(0, 500));
      return { raw, data: JSON.parse(raw) };
    }
    let lastBulkTurn;
    async function bulkTurn(input) {
      const id = "bulk-" + crypto.randomUUID();
      await quiet("/v1/agents/" + bulk + "/turns", "POST", { id, input }, 202);
      const deadline = Date.now() + 90_000;
      for (;;) {
        const { data } = await quiet("/v1/agents/" + bulk + "/turns/" + id);
        if (["completed", "failed", "cancelled"].includes(data.state)) { lastBulkTurn = id; return data.state; }
        assert.ok(Date.now() < deadline, "bulk turn did not settle");
        await delay(100);
      }
    }
    async function truthHistory() {
      const events = [];
      for (let after = "0"; ;) {
        const { data } = await quiet("/v1/agents/" + bulk + "/events/history?after=" + after + "&limit=256");
        events.push(...data.data);
        if (!data.has_more) return events;
        after = String(data.data.at(-1).cursor);
      }
    }
    const tool = "SESSION_TOOL " + Buffer.from('text("Ωé🚀".repeat(60000)); text("BULK_TOOL_DONE");').toString("base64");
    const states = { small: await bulkTurn("BULK_SMALL hello"), tool: await bulkTurn(tool) };
    const toolTurn = lastBulkTurn;
    states.calibrate = await bulkTurn("BULK_CAL1 " + "a".repeat(8_000));
    const calibration = Math.max(...(await truthHistory()).filter(event => JSON.stringify(event).includes("BULK_CAL1")).map(storedBytes));
    states.edge = await bulkTurn("BULK_CAL2 " + "a".repeat(8_000 + 8_320 - calibration));
    states.chunked = await bulkTurn("BULK_CHUNKED " + "é漢🚀a".repeat(150_000));
    states.large = await bulkTurn("BULK_LARGE " + "é漢🚀a".repeat(40_000));
    states.huge = await bulkTurn("BULK_HUGE " + "x🚀".repeat(1_000_000));
    states.tail = await bulkTurn("BULK_TAIL after the huge event");
    // Verify once with every event in local SQLite storage (where stand-ins
    // skip hydration), then again after sealing so pages cross the archive.
    async function verifyBounded(stage, sealed) {
      const truth = await truthHistory();
      const truthCursors = truth.map(event => String(event.cursor));
      const sizes = truth.map(event => ({ cursor: String(event.cursor), stored: storedBytes(event), json: bytesOf(event) }));
      record({ kind: "ground_truth", stage, events: truth.length, sizes });
      if (sealed) assert.ok(BigInt(sealed.end_cursor) > 0n && BigInt(sealed.end_cursor) < BigInt(truthCursors.at(-1)), "history spans archive and local storage");
      assert.ok(sizes.some(size => size.stored > 8_192 && size.stored <= 8_448), "an event sits just above the stand-in threshold");
      assert.ok(sizes.some(size => size.stored > 1_000_000 && size.stored <= 4 * 1024 * 1024), "a chunked readable event exists");
      assert.ok(sizes.some(size => size.stored > 4 * 1024 * 1024), "an event exceeds the readable limit");
      const toolResult = truth.filter(event => event.turn_id === toolTurn).sort((left, right) => storedBytes(right) - storedBytes(left))[0];
      record({ kind: "huge_tool_result", cursor: toolResult?.cursor, stored: toolResult && storedBytes(toolResult), prefix: JSON.stringify(toolResult).slice(0, 300) });
      assert.ok(toolResult && storedBytes(toolResult) > 60 * 1024 && toolResult.event?.type === "tool.result", "a huge tool result exists");

      // Public HTTP: opt-in bounds keep pages small and every cursor present.
      const httpPages = [], httpCursors = [];
      for (let before; ;) {
        const { raw, data } = await quiet("/v1/agents/" + bulk + "/events/history?limit=100&max_bytes=61440&max_event_bytes=8192" + (before ? "&before=" + before : ""));
        httpPages.push({ response_bytes: bytesOf(raw), count: data.data.length, has_more: data.has_more,
          stand_ins: data.data.filter(event => event.truncated === true && event.type === undefined).map(event => String(event.cursor)) });
        httpCursors.unshift(...data.data.map(event => String(event.cursor)));
        for (const event of data.data) {
          const size = sizes.find(entry => entry.cursor === String(event.cursor));
          if (size.stored > 8_192) assert.deepEqual([event.truncated, event.message_bytes, event.type], [true, size.stored, undefined]);
          else assert.equal(JSON.stringify(event), JSON.stringify(truth.find(entry => String(entry.cursor) === size.cursor)));
        }
        assert.ok(bytesOf(raw) <= 61_440 + 100 * 256 + 512, "bounded HTTP page " + bytesOf(raw));
        if (!data.has_more) break;
        before = String(data.data[0].cursor);
      }
      assert.deepEqual(httpCursors, truthCursors, "bounded HTTP pages cover every cursor once");
      await quiet("/v1/agents/" + bulk + "/events/history?max_bytes=1023", "GET", undefined, 400);
      await quiet("/v1/agents/" + bulk + "/events/history?max_event_bytes=abc", "GET", undefined, 400);
      record({ kind: "http_bounded_pages", stage, pages: httpPages });

      // session_control: complete older and newer walks plus chunked reads.
      const reads = sizes.filter(size => size.json > 8_192 || size.stored > 8_192).map(size => size.cursor);
      const walkProgram = "SESSION_TOOL " + Buffer.from(`const SID = ${JSON.stringify(bulk)}, READ = ${JSON.stringify(reads)};
  const utf8 = s => { let n = 0; for (const ch of s) { const c = ch.codePointAt(0); n += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4; } return n; };
  const fnv = s => { let h = 0x811c9dc5; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; } return h; };
  const summary = page => ({ count: page.count, bytes: page.bytes, data_bytes: utf8(JSON.stringify(page.data)), result_bytes: utf8(JSON.stringify(page)),
    max_event_bytes: Math.max(0, ...page.data.map(event => utf8(JSON.stringify(event)))), cursors: page.data.map(event => event.cursor),
    has_more: page.has_more, next_before: page.next_before, next_after: page.next_after, omitted: page.omitted_events ?? 0,
    truncated: page.data.filter(event => event.truncated).map(event => ({ cursor: event.cursor, message_bytes: event.message_bytes ?? null,
      original_bytes: event.original_bytes ?? null, readable: !/only this preview/.test(event.full_content) })) });
  const older = [], newer = [], read = [];
  for (let before, i = 0; i < 100; i++) {
    const page = await tools.session_control({ operation: "events", session_id: SID, limit: 100, ...(before ? { before } : {}) });
    older.push(summary(page)); if (page.next_before === null) break; before = page.next_before;
  }
  for (let after = "0", i = 0; i < 100; i++) {
    const page = await tools.session_control({ operation: "events", session_id: SID, limit: 100, after });
    newer.push(summary(page)); if (!page.has_more) break; after = page.next_after;
  }
  for (const cursor of READ) {
    let offset = 0, text = "", calls = 0, sha, maxResult = 0;
    for (;;) {
      const chunk = await tools.session_control({ operation: "event", session_id: SID, cursor, ...(offset ? { offset } : {}) });
      calls++; maxResult = Math.max(maxResult, utf8(JSON.stringify(chunk)));
      if (!chunk.readable) { read.push({ cursor, readable: false, message_bytes: chunk.message_bytes, result_bytes: maxResult }); break; }
      if (sha !== undefined && sha !== chunk.sha256) throw new Error("serialization changed between chunks");
      sha = chunk.sha256; text += chunk.chunk;
      if (chunk.complete) { read.push({ cursor, readable: true, calls, total_bytes: chunk.total_bytes, sha256: sha, length: text.length, fnv: fnv(text), max_result_bytes: maxResult }); break; }
      offset = chunk.next_offset;
    }
  }
  let badOffset; try { await tools.session_control({ operation: "event", session_id: SID, cursor: READ[0], offset: 999999999 }); } catch (error) { badOffset = String(error.message); }
  text(JSON.stringify([{ label: "walk", ok: true, value: { older, newer, read, badOffset } }]));`).toString("base64");
      const walk = results(await socketTurn(driver, walkProgram)).walk.value;
      record({ kind: "bounded_walk", stage, walk });
      const fnv = s => { let h = 0x811c9dc5; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; } return h; };
      for (const [direction, pages] of [["older", walk.older], ["newer", walk.newer]]) {
        for (const page of pages) {
          assert.ok(page.count <= 100 && page.count === page.cursors.length, direction + " count");
          assert.equal(page.bytes, page.data_bytes, direction + " reported bytes are exact");
          assert.ok(page.data_bytes <= 60 * 1024 && page.max_event_bytes <= 8 * 1024 && page.result_bytes <= 64 * 1024,
            direction + " page bounds " + JSON.stringify({ ...page, cursors: page.cursors.length }));
        }
        const order = (direction === "older" ? pages.slice().reverse() : pages).flatMap(page => page.cursors.map(String));
        assert.deepEqual(order, truthCursors, direction + " walk returns every cursor exactly once in order");
      }
      assert.equal(walk.older.at(-1).next_before, null);
      const truncated = new Map(walk.older.flatMap(page => page.truncated).map(entry => [String(entry.cursor), entry]));
      for (const size of sizes) {
        const entry = truncated.get(size.cursor);
        if (size.stored > 8_192) assert.equal(entry?.message_bytes, size.stored, "stand-in for " + size.cursor);
        else if (size.json > 8_192) assert.equal(entry?.original_bytes, size.json, "tool stub for " + size.cursor);
        else assert.equal(entry, undefined, "small event " + size.cursor + " is complete");
      }
      for (const entry of walk.read) {
        const event = truth.find(candidate => String(candidate.cursor) === entry.cursor), size = sizes.find(candidate => candidate.cursor === entry.cursor);
        if (size.stored > 4 * 1024 * 1024) {
          assert.deepEqual([entry.readable, entry.message_bytes], [false, size.stored], "unreadable huge event is explicit");
          assert.equal(truncated.get(entry.cursor).readable, false);
          continue;
        }
        const serialized = JSON.stringify(event);
        assert.deepEqual([entry.readable, entry.total_bytes, entry.sha256, entry.length, entry.fnv],
          [true, bytesOf(serialized), createHash("sha256").update(serialized).digest("hex"), serialized.length, fnv(serialized)],
          "complete content of " + entry.cursor);
        assert.ok(entry.max_result_bytes <= 64 * 1024, "chunk result bound " + entry.max_result_bytes);
      }
      assert.match(walk.badOffset, /not a next_offset/);
      const measurements = { truth_events: truth.length, stage, archived_through: sealed?.end_cursor ?? "0", latest: truthCursors.at(-1),
        older_pages: walk.older.map(page => [page.count, page.bytes, page.result_bytes]), newer_pages: walk.newer.map(page => [page.count, page.bytes, page.result_bytes]),
        http_pages: httpPages.map(page => [page.count, page.response_bytes]), reads: walk.read.map(entry => [entry.cursor, entry.readable, entry.total_bytes ?? entry.message_bytes, entry.calls ?? 1, entry.max_result_bytes ?? entry.result_bytes]) };
      record({ kind: "bounded_measurements", ...measurements });
      console.log(JSON.stringify({ bounded: measurements }));
    }
    await verifyBounded("local");
    const sealed = await (await backend.fetch("https://fixture.test/__archive", { method: "POST", body: JSON.stringify({ agent: bulk }) })).json();
    record({ kind: "archive_seal", bulk, states, calibration, sealed });
    assert.equal(sealed.sealed, true, JSON.stringify(sealed));
    assert.equal(sealed.start_cursor, "1", "the first verification read only local storage");
    await verifyBounded("archived", sealed);

    record({ kind: "summary", driver, target, foreign, admitted_turns: [first, slow], steer_message: message, denied_turn: denied });
    console.log(JSON.stringify({ evidence, driver, target, admitted: [first, slow], steered: message }));
  } finally {
    for (const socket of sockets) socket.terminate();
    await writeFile(join(evidence, "trace.json"), JSON.stringify({ command: "pnpm --filter nanocodex-managed-service run test:session-control", provenance, trace }, null, 2) + "\n");
    await writeFile(join(evidence, "runtime.log"), logs.join("\n") + "\n");
    await mf.dispose();
  }
});
