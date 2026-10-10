import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createWriteStream } from "node:fs";
import { appendFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";
import { fileURLToPath, pathToFileURL } from "node:url";
import { fetch } from "./support/miniflare-fetch.mjs";

// Large Hand results through the shipped path: Node Hand exec_command ->
// reverse WebSocket -> workerd AccountHostedTools broker with its persisted
// SQLite ledger and R2 result archive -> public account HTTP (/invoke,
// /invoke-receipt, /hosted-tool-stats) driven with curl. The only fault
// boundary is the R2 binding: a fixture wrapper consumes a plan queue stored in
// the same emulated bucket (fail or hold one put). Lost ACKs, holds and
// resends happen on the Hand's real socket; nothing in the broker is stubbed.
// NANOCODEX_BENCHMARK_SOURCE_ROOT selects the implementation under test (e.g.
// a master worktree for before-evidence); LABEL names the evidence directory.
const execFileAsync = promisify(execFile);
// Hand attachment lifecycle records go to each scenario's hand-attachment.jsonl instead of the console.
const attachmentLog = [];
const originalInfo = console.info;
console.info = (record, ...rest) => { if (record?.type === "hand.attachment") attachmentLog.push({ at: Date.now(), ...record }); else originalInfo(record, ...rest); };
const checkout = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const repo = resolve(process.env.NANOCODEX_BENCHMARK_SOURCE_ROOT ?? checkout);
const label = process.env.NANOCODEX_BENCHMARK_LABEL ?? "candidate";
assert.match(label, /^[A-Za-z0-9_.-]+$/);
const outputRoot = join(checkout, "output/hand-large-result-r2-journey", label, Date.now() + "-" + process.pid);
const requireFromRepo = createRequire(join(repo, "js/managed/package.json"));
const { build } = requireFromRepo("esbuild");
const { Miniflare } = requireFromRepo("miniflare");
const { WebSocket } = requireFromRepo("ws");
const { createTools } = await import(pathToFileURL(join(repo, "js/nanocodex/tools/Tools.mjs")));
const { createNodeProcessTools } = await import(pathToFileURL(join(repo, "js/nanocodex-tools/tools/nodeProcess.mjs")));
const owner = "00000000-0000-4000-8000-000000000083";
const session = "fixture-session";
const machine = "fixture-hand";
const MiB = 1024 * 1024;
const ARCHIVE_PREFIX = '{"hosted_tool_archive":';
const source = { root: repo, commit: execFileSync("git", ["-C", repo, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  dirty: execFileSync("git", ["-C", repo, "status", "--short", "--", "js/managed/src", "js/nanocodex-tools/src"], { encoding: "utf8" }).trim() };

const fixtureSource = `
import {DurableObject} from 'cloudflare:workers';
import {AccountHostedTools} from './src/account-hosted-tools.ts';
import {HostedToolsBroker,R2HostedToolsResultArchive} from './src/hosted-tools-broker.ts';
const PLAN='__fault/plan/', GETPLAN='__fault/getplan/', HELD='__fault/held/', RELEASE='__fault/release/';
const log=(event,fields)=>console.log(JSON.stringify({fixture:'r2_fault',at:Date.now(),event,...fields}));
// Real emulated R2 underneath; only hosted-tools/ puts consult the plan queue.
function faultBucket(bucket){
  let puts=0;
  return {
    async put(key,value,options){
      if(!key.startsWith('hosted-tools/'))return bucket.put(key,value,options);
      const n=++puts;
      const plan=(await bucket.list({prefix:PLAN})).objects.map(object=>object.key).sort()[0];
      let action='pass';
      if(plan){action=(await (await bucket.get(plan)).json()).action;await bucket.delete(plan);}
      const id=plan?plan.slice(PLAN.length):'pass-'+n;
      log('put',{n,id,key,bytes:value.byteLength,action});
      if(action==='fail'){log('put_failed',{n,id,key});throw new Error('fixture R2 put failure '+id);}
      if(action==='hold'){
        await bucket.put(HELD+id,JSON.stringify({key,bytes:value.byteLength,at:Date.now()}));
        const until=Date.now()+120000;
        while(!(await bucket.head(RELEASE+id))){if(Date.now()>until)throw new Error('fixture hold expired '+id);await new Promise(resolve=>setTimeout(resolve,20));}
        log('put_released',{n,id,key});
      }
      const stored=await bucket.put(key,value,options);
      log('put_done',{n,id,key,stored:stored!==null});
      if(action==='store_then_hold'){
        // The object is durable; the broker has not seen put() return (crash window before its CAS).
        await bucket.put(HELD+id,JSON.stringify({key,bytes:value.byteLength,stored:stored!==null,at:Date.now()}));
        const until=Date.now()+120000;
        while(!(await bucket.head(RELEASE+id))){if(Date.now()>until)throw new Error('fixture hold expired '+id);await new Promise(resolve=>setTimeout(resolve,20));}
        log('put_released',{n,id,key});
      }
      return stored;
    },
    head:key=>bucket.head(key),
    // Archive reads may be held the same way (plan prefix __fault/getplan/).
    async get(key,options){
      if(!key.startsWith('hosted-tools/'))return bucket.get(key,options);
      const plan=(await bucket.list({prefix:GETPLAN})).objects.map(object=>object.key).sort()[0];
      if(plan){
        await bucket.delete(plan);const id=plan.slice(GETPLAN.length);
        log('get_held',{id,key});await bucket.put(HELD+id,JSON.stringify({key,at:Date.now()}));
        const until=Date.now()+120000;
        while(!(await bucket.head(RELEASE+id))){if(Date.now()>until)throw new Error('fixture hold expired '+id);await new Promise(resolve=>setTimeout(resolve,20));}
        log('get_released',{id,key});
      }
      return bucket.get(key,options);
    }, delete:keys=>bucket.delete(keys), list:options=>bucket.list(options),
  };
}
export class FixtureHands extends AccountHostedTools {
  constructor(ctx,env){super(ctx,{...env,NANOCODEX_HISTORY:faultBucket(env.NANOCODEX_HISTORY)});}
  async fetch(request){
    const path=new URL(request.url).pathname;
    // Read-only evidence from the persisted ledger (sizes, states, small row text).
    if(path==='/__fixture/inspect'){
      const sql=this.ctx.storage.sql;
      return Response.json({
        routes:sql.exec('SELECT * FROM hosted_tool_routes').toArray().map(({catalog_json,machines_json,...row})=>row),
        calls:sql.exec("SELECT call_id,source_call_id,name,state,lease_id,generation,host_id,host_runtime_id,deadline_at,dispatched_at,created_at,updated_at,"
          +"length(CAST(input_json AS BLOB)) AS input_bytes,length(CAST(result_json AS BLOB)) AS result_bytes,length(CAST(receipt_json AS BLOB)) AS receipt_bytes,"
          +"CASE WHEN length(CAST(result_json AS BLOB))<=65536 THEN result_json END AS result_json,"
          +"CASE WHEN length(CAST(receipt_json AS BLOB))<=65536 THEN receipt_json END AS receipt_json,"
          +"json_valid(result_json) AS result_valid,json_extract(result_json,'$.status') AS result_status,json_extract(result_json,'$.output.success') AS result_success "
          +"FROM hosted_tool_calls ORDER BY created_at,source_call_id").toArray()});
    }
    // The owner's real "forget this Hand" operation (normally reached over RPC).
    if(path==='/__fixture/forget')return Response.json(await this.forgetMachine('${owner}','${machine}',true));
    return super.fetch(request);
  }
}
// A finite-lease (VM-style) Hand broker with the same R2 archive. Its external lease
// authority is synthetic: renewal succeeds only while R2 key __lease/mode is "valid".
export class FixtureLeasedBroker extends DurableObject {
  constructor(ctx,env){
    super(ctx,env);
    const bucket=faultBucket(env.NANOCODEX_HISTORY);
    this.broker=new HostedToolsBroker(ctx,{resultArchive:new R2HostedToolsResultArchive(bucket,ctx.id.toString()),
      renewLeasedAttachment:async()=>{const mode=await env.NANOCODEX_HISTORY.get('__lease/mode');
        const value=mode?await mode.text():'valid';log('lease_renewal',{mode:value});return value==='valid'?Date.now()+500:undefined;}});
  }
  async fetch(request){
    const path=new URL(request.url).pathname;
    if(path==='/leased/attach')return this.broker.upgrade('synthetic-vm-session',undefined,undefined,undefined,{
      expectedAttachmentId:'fixture-leased-hand',fixedRouteId:'vm-host:synthetic-allocation:1',
      renewalToken:'synthetic-vm-scope-token',maximumLeaseExpiresAt:Date.now()+500});
    if(path==='/leased/invoke'){
      const {call_id,input}=await request.json();
      const tool=this.broker.machineTool('fixture-leased-hand','exec_command');
      if(!tool)return Response.json({status:'unavailable'},{status:503});
      return Response.json(await tool.handler(input,{sessionId:'synthetic-caller',callId:call_id,model:'fixture-model'}));
    }
    if(path==='/leased/inspect'){
      const sql=this.ctx.storage.sql;
      return Response.json({routes:sql.exec('SELECT * FROM hosted_tool_routes').toArray().map(({catalog_json,machines_json,...row})=>row),
        calls:sql.exec("SELECT call_id,source_call_id,state,lease_id,generation,deadline_at,result_json,receipt_json FROM hosted_tool_calls").toArray()});
    }
    return new Response(null,{status:404});
  }
  webSocketMessage(socket,message){return this.broker.webSocketMessage(socket,message);}
  webSocketClose(socket,code,reason){this.broker.webSocketClose(socket,code,reason);}
  webSocketError(socket){this.broker.webSocketError(socket);}
}
export default {fetch(request,env){const url=new URL(request.url);
  if(url.pathname.startsWith('/leased/'))return env.LEASED.getByName('fixture-leased').fetch(request);
  return env.HANDS.getByName('${owner}').fetch(new Request('https://hand.internal'+url.pathname+url.search,request));}};
`;

function payload(kind, bytes) {
  const unit = kind === "ascii" ? "abcdefghijklmnopqrstuvwxyz012345\n"
    : 'é中😀 "quote" \\back\\slash\t\u0001\u2028{"nested":{"json":[1,"two",null]}}\r\n';
  const encoded = Buffer.from(unit, "utf8");
  const out = Buffer.alloc(bytes);
  for (let offset = 0; offset < bytes; offset += encoded.length) encoded.copy(out, offset, 0, Math.min(encoded.length, bytes - offset));
  let end = bytes;
  while (end > 0 && (out[end - 1] & 0xc0) === 0x80) end--;
  if (end > 0 && out[end - 1] >= 0xc0) end--;
  out.fill(0x2e, end);
  return out;
}
const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");
const bounded = async (promise, what, ms) => {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(what + " exceeded " + ms + "ms")), ms); })]); }
  finally { clearTimeout(timer); }
};
async function waitFor(what, predicate, ms = 30_000) {
  const until = Date.now() + ms;
  for (;;) {
    const value = await predicate();
    if (value) return value;
    if (Date.now() > until) throw new Error("timed out waiting for " + what);
    await delay(25);
  }
}

let bundled;
async function bundle() {
  bundled ??= build({ stdin: { contents: fixtureSource, resolveDir: join(repo, "js/managed") },
    bundle: true, write: false, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    external: ["cloudflare:*", "node:*"], alias: {
      "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"),
      "nanocodex-tools/internal/hosted-machine": join(repo, "js/nanocodex-tools/tools/hostedMachine.mjs"),
      "nanocodex-tools": join(repo, "js/nanocodex-tools/src/index.ts"),
      "node-rsa": join(repo, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs"),
    }, logLevel: "silent" }).then(result => result.outputFiles[0].text);
  return bundled;
}

/** One isolated deployment: workerd + SQLite + R2 persisted under its own evidence directory, and one Node Hand. */
async function startStack(name, { execTimeoutMs = 20_000 } = {}) {
  const dir = join(outputRoot, name);
  const handDir = join(dir, "hand");
  await mkdir(join(dir, "http"), { recursive: true }); await mkdir(handDir, { recursive: true });
  const stack = { name, dir, handDir, execTimeoutMs, wire: [], http: [], httpSeq: 0, attempts: 0, sources: new Map(),
    rules: new Map(), resultSends: new Map(), sockets: [], tokens: new Map(), expected: new Map(), planSeq: 0,
    attachments: [], gates: new Map(), observations: {}, attachmentStart: attachmentLog.length };
  const script = await bundle();
  await writeFile(join(dir, "worker.mjs"), script);
  const runtimeLog = createWriteStream(join(dir, "workerd.log"));
  stack.runtimeLog = runtimeLog;
  stack.mfOptions = { name: "r2journey", port: 0, modules: true, script,
    compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
    durableObjectsPersist: join(dir, "do-state"), r2Persist: join(dir, "r2-state"), r2Buckets: ["NANOCODEX_HISTORY"],
    durableObjects: { HANDS: { className: "FixtureHands", useSQLite: true }, LEASED: { className: "FixtureLeasedBroker", useSQLite: true } },
    handleRuntimeStdio(stdout, stderr) { stdout.pipe(runtimeLog, { end: false }); stderr.pipe(runtimeLog, { end: false }); } };
  stack.mf = new Miniflare(stack.mfOptions);
  stack.base = String(await stack.mf.ready);
  stack.bucket = await stack.mf.getR2Bucket("NANOCODEX_HISTORY");
  stack.native = await createNodeProcessTools({ workspace: handDir });
  stack.tools = await createTools({ attachmentId: machine, machines: [{ id: machine, name: "Synthetic Hand", workspace: handDir, capabilities: ["shell"] }],
    tools: stack.native.tools.map(tool => ({ ...tool, timeoutMs: execTimeoutMs })) });
  stack.main = await attach(stack, "runtime-1");
  await refreshToken(stack);
  return stack;
}

/** A Hand runtime (one attachment = one runtime id) whose real ws socket is tapped for evidence and wire faults. */
async function attach(stack, tag, { path = "/tool-host", tools = stack.tools } = {}) {
  const endpoint = new URL(path, stack.base).href.replace(/^http/, "ws");
  const attachment = tools.attach({ endpoint, transport: { async connect() {
    const gate = stack.gates.get(tag);
    if (gate) { await gate.promise; if (gate.stop) throw new Error("Hand runtime " + tag + " stopped"); }
    const socket = new WebSocket(endpoint, { headers: { "x-nanocodex-owner-id": owner }, maxPayload: 256 * MiB });
    tap(stack, socket, tag);
    return socket;
  } } });
  stack.attachments.push(attachment);
  await bounded(attachment.connect(), "Hand " + tag + " connect", 15_000);
  return attachment;
}

function tap(stack, socket, tag) {
  const attempt = ++stack.attempts;
  socket.attempt = attempt; socket.tag = tag; stack.sockets.push(socket);
  const record = entry => stack.wire.push({ at: Date.now(), attempt, tag, ...entry });
  const send = socket.send.bind(socket);
  socket.send = (data, ...rest) => {
    let text = typeof data === "string" ? data : String(data);
    const head = text.slice(0, 200);
    const type = /^\{"type":"([a-z_]+)"/.exec(head)?.[1];
    const callId = /"call_id":"([^"]+)"/.exec(head)?.[1];
    const sourceId = callId && stack.sources.get(callId);
    if (type === "result" && sourceId) {
      const count = (stack.resultSends.get(sourceId) ?? 0) + 1;
      stack.resultSends.set(sourceId, count);
      const rule = stack.rules.get(sourceId);
      let mutated = false;
      if (rule?.mutateOn === count) { text = rule.mutate(text); mutated = true; }
      const entry = { dir: "hand", type, call_id: callId, source: sourceId, send_index: count, bytes: Buffer.byteLength(text), sha256: sha256(text), mutated };
      if (rule?.holdSend && count === 1) {
        record({ ...entry, event: "held" });
        rule.held = true;
        rule.release = () => {
          if (socket.readyState !== WebSocket.OPEN) { record({ ...entry, event: "release_skipped_closed" }); return; }
          record({ ...entry, event: "released" });
          send(text, ...rest);
        };
        return;
      }
      record(entry);
    } else record({ dir: "hand", type, call_id: callId, source: sourceId, bytes: Buffer.byteLength(text) });
    if (socket.partitioned) { record({ dir: "hand", event: "dropped_partitioned", type }); return; }
    return send(text, ...rest);
  };
  const emit = socket.emit.bind(socket);
  socket.emit = (event, ...args) => {
    if (event === "message") {
      const text = String(args[0]);
      const frame = text.length < MiB ? JSON.parse(text) : { type: "large" };
      if (frame.type === "call") {
        const found = /'([A-Za-z0-9_.-]+)' >> effects\.log/.exec(text)?.[1];
        if (found) stack.sources.set(frame.call_id, found);
      }
      const sourceId = frame.call_id && stack.sources.get(frame.call_id);
      if (socket.partitioned) { record({ dir: "broker", type: frame.type, call_id: frame.call_id, source: sourceId, event: "lost_partitioned" }); return true; }
      const rule = sourceId && stack.rules.get(sourceId);
      if (frame.type === "ack" && rule?.dropAck > 0) {
        rule.dropAck--;
        record({ dir: "broker", type: "ack", call_id: frame.call_id, source: sourceId, event: "ack_dropped_then_terminated" });
        setImmediate(() => socket.terminate());
        return true;
      }
      record({ dir: "broker", type: frame.type, call_id: frame.call_id, source: sourceId,
        ...(frame.type === "recover" ? { call_ids: frame.call_ids, sources: frame.call_ids.map(id => stack.sources.get(id)) } : {}),
        ...(frame.type === "close" || frame.type === "error" ? { frame } : {}) });
    } else if (event === "close") {
      record({ event: "close", code: args[0], reason: String(args[1] ?? ""), partitioned: !!socket.partitioned });
      if (socket.partitionDelivered) return true;
    }
    return emit(event, ...args);
  };
  /** Hand-side partition: the Hand sees its socket die (and reconnects) while the broker still holds the open TCP socket. */
  socket.partition = () => {
    socket.partitioned = true; socket.partitionDelivered = true;
    record({ event: "hand_side_partition" });
    emit("close", 1006, Buffer.from(""));
  };
}

async function http(stack, name, method, path, body, { maxTime = 120 } = {}) {
  const seq = String(++stack.httpSeq).padStart(3, "0");
  const prefix = join(stack.dir, "http", seq + "-" + name);
  const args = ["-sS", "-X", method, "--max-time", String(maxTime), "-H", "x-nanocodex-owner-id: " + owner,
    "-D", prefix + ".response-headers.txt", "-o", prefix + ".response-body.json", "-w", "%{http_code} %{time_total}"];
  if (body !== undefined) {
    await writeFile(prefix + ".request.json", JSON.stringify(body, null, 2));
    args.push("-H", "content-type: application/json", "--data-binary", "@" + prefix + ".request.json");
  }
  args.push(new URL(path, stack.base).href);
  await appendFile(join(stack.dir, "curl-commands.sh"), "curl " + args.map(arg => JSON.stringify(arg)).join(" ") + "\n");
  const started = Date.now();
  let status = 0, curlError, timeTotal;
  try { const { stdout } = await execFileAsync("curl", args, { maxBuffer: MiB }); [status, timeTotal] = stdout.trim().split(" ").map(Number); }
  catch (error) { curlError = String(error.stderr || error.message).trim(); status = Number(String(error.stdout ?? "").split(" ")[0]) || 0; }
  const text = await readFile(prefix + ".response-body.json", "utf8").catch(() => "");
  let json; try { json = JSON.parse(text); } catch { /* recorded as text */ }
  stack.http.push({ seq, name, method, path, status, curl_error: curlError, started_at: started, elapsed_ms: Date.now() - started,
    curl_time_total_s: timeTotal, response_bytes: Buffer.byteLength(text),
    summary: json ? { error: json.error, admission: json.admission, receipt: json.receipt, success: json.success,
      status: json.structured_result?.status, exit_code: json.structured_result?.exit_code,
      output_bytes: typeof json.structured_result?.output === "string" ? Buffer.byteLength(json.structured_result.output) : undefined } : undefined });
  return { status, json, text, curlError };
}

async function refreshToken(stack) {
  const snapshot = await http(stack, "snapshot", "POST", "/snapshot", { owner_id: owner, machine_id: machine });
  assert.equal(snapshot.status, 200, snapshot.text.slice(0, 400));
  const token = snapshot.json.machines?.find(entry => entry.machine.id === machine)?.tools.find(tool => tool.name === "exec_command")?.route_token;
  assert.ok(token, "exec_command route published: " + snapshot.text.slice(0, 400));
  stack.token = token;
  return token;
}

const command = (id, file) => "printf '%s\\n' '" + id + "' >> effects.log; cat " + file;
async function prepare(stack, id, kind, bytes) {
  const content = payload(kind, bytes);
  await writeFile(join(stack.handDir, id + ".txt"), content);
  stack.expected.set(id, content.toString("utf8"));
  return command(id, id + ".txt");
}
function invoke(stack, id, cmd, { yieldMs = 5_000, name = "invoke-" + id } = {}) {
  const token = stack.tokens.get(id) ?? stack.token;
  stack.tokens.set(id, token);
  return http(stack, name, "POST", "/invoke", { owner_id: owner, name: "exec_command", session_id: session, call_id: id,
    route_token: token, machine_id: machine, model: "fixture-model",
    input: { cmd, shell: "/bin/sh", login: false, yield_time_ms: yieldMs } });
}
function receipt(stack, id, name = "receipt-" + id) {
  return http(stack, name, "POST", "/invoke-receipt", { owner_id: owner, name: "exec_command", session_id: session, call_id: id,
    route_token: stack.tokens.get(id) ?? stack.token, machine_id: machine, wait_ms: 0 });
}
function assertExact(response, id, stack, what) {
  const expected = stack.expected.get(id);
  assert.equal(response.status, 200, what + ": HTTP " + response.status + " " + (response.curlError ?? "") + " " + response.text.slice(0, 600));
  assert.equal(response.json.success, true, what + ": " + response.text.slice(0, 600));
  assert.equal(response.json.structured_result.exit_code, 0, what);
  assert.ok(response.json.structured_result.output === expected,
    what + ": output differs (" + Buffer.byteLength(response.json.structured_result.output ?? "") + " vs " + Buffer.byteLength(expected) + " bytes)");
}
/**
 * After a protocol fence (different resend), a replacement runtime or an owner
 * retirement, the call's pinned generation is gone, so a receipt read through
 * its original route may be unresolved (409, admission retained); this is the
 * existing receipt-authority rule for every result size. It must never be
 * "missing" or return other bytes; the ledger and archive prove the retained bytes.
 */
async function receiptRetained(stack, id, what) {
  const response = await receipt(stack, id, "receipt-retained-" + id);
  stack.observations["receipt_retained_" + id] = { status: response.status, body: response.status === 200 ? "exact-checked" : response.text.slice(0, 300) };
  if (response.status === 200) { assertExact(response, id, stack, what); return; }
  assert.equal(response.status, 409, what + ": " + response.text.slice(0, 300));
  assert.equal(response.json.error, "receipt_unresolved"); assert.equal(response.json.admission, "retained");
}
function assertAmbiguous(response, what) {
  assert.equal(response.status, 200, what + ": " + response.text.slice(0, 600));
  assert.equal(response.json.success, false, what + ": " + response.text.slice(0, 600));
  assert.equal(response.json.structured_result?.status, "ambiguous", what + ": " + response.text.slice(0, 600));
}
async function inspect(stack) {
  const response = await fetch(new URL("/__fixture/inspect", stack.base), { headers: { "x-nanocodex-owner-id": owner } });
  assert.equal(response.status, 200);
  return response.json();
}
async function row(stack, id) { return (await inspect(stack)).calls.find(call => call.source_call_id === id); }
async function effects(stack) {
  try { return (await readFile(join(stack.handDir, "effects.log"), "utf8")).trim().split("\n").filter(Boolean); }
  catch (error) { if (error.code === "ENOENT") return []; throw error; }
}
async function archived(stack, prefix = "hosted-tools/") {
  const listed = await stack.bucket.list({ prefix });
  return listed.objects.map(object => ({ key: object.key, size: object.size }));
}
async function plan(stack, action, kind = "plan") {
  const id = String(++stack.planSeq).padStart(4, "0");
  await stack.bucket.put("__fault/" + kind + "/" + id, JSON.stringify({ action }));
  return id;
}
const held = (stack, id) => waitFor("R2 operation " + id + " held", async () => (await stack.bucket.head("__fault/held/" + id)) !== null);
const release = (stack, id) => stack.bucket.put("__fault/release/" + id, "1");
const callFrames = (stack, id) => stack.wire.filter(entry => entry.dir === "broker" && entry.type === "call" && entry.source === id);
const acks = (stack, id) => stack.wire.filter(entry => entry.dir === "broker" && entry.type === "ack" && entry.source === id && !entry.event);
const resultSends = (stack, id) => stack.wire.filter(entry => entry.dir === "hand" && entry.type === "result" && entry.source === id && entry.event !== "held");
const liveRoute = async stack => (await inspect(stack)).routes.find(route => route.lease_id);
function reference(text) {
  assert.ok(typeof text === "string" && text.startsWith(ARCHIVE_PREFIX), "expected an archive reference, got " + String(text).slice(0, 200));
  return JSON.parse(text).hosted_tool_archive;
}
async function assertArchiveObject(stack, ref, id, field) {
  assert.equal(ref.field, field);
  const object = await stack.bucket.get(ref.key);
  assert.ok(object, "archive object exists: " + ref.key);
  const bytes = Buffer.from(await object.arrayBuffer());
  assert.equal(bytes.byteLength, ref.utf8_bytes, "object size equals the referenced UTF-8 length");
  assert.equal(sha256(bytes), ref.sha256, "object SHA-256 equals the reference");
  assert.ok(ref.key.endsWith("/" + ref.sha256 + ".json"), "content-addressed key");
  const outcome = JSON.parse(bytes.toString("utf8"));
  assert.deepEqual(Object.keys(outcome), ["status", "output"]);
  assert.ok(outcome.output.structured_result.output === stack.expected.get(id), id + " archived bytes are the exact outcome");
  return { key: ref.key, bytes: bytes.byteLength, sha256: ref.sha256 };
}

/** Kills workerd (every Durable Object and socket) and starts it again on the same port with the same SQLite/R2 state. */
async function restartRuntime(stack) {
  const port = Number(new URL(stack.base).port);
  await stack.mf.dispose();
  stack.wire.push({ at: Date.now(), event: "workerd_stopped" });
  stack.mf = new Miniflare({ ...stack.mfOptions, port });
  assert.equal(String(await stack.mf.ready), stack.base);
  stack.bucket = await stack.mf.getR2Bucket("NANOCODEX_HISTORY");
  stack.wire.push({ at: Date.now(), event: "workerd_started" });
}

async function stopStack(stack, failure) {
  for (const gate of stack.gates.values()) gate.resolve();
  for (const socket of stack.sockets) { try { socket.terminate(); } catch { /* already closed */ } }
  await Promise.all(stack.attachments.map(attachment => bounded(attachment.close(), "attachment close", 3_000).catch(() => {})));
  await bounded(stack.tools?.close() ?? Promise.resolve(), "tools close", 5_000).catch(() => {});
  await bounded(stack.extraTools?.close() ?? Promise.resolve(), "tools close", 5_000).catch(() => {});
  await stack.native?.close().catch(() => {});
  let finalLedger;
  try { finalLedger = await inspect(stack); } catch (error) { finalLedger = { error: String(error) }; }
  let objects = [];
  try { objects = await archived(stack, ""); } catch { /* recorded below */ }
  await stack.mf?.dispose().catch(() => {});
  stack.runtimeLog?.end();
  const summary = { scenario: stack.name, outcome: failure ? "failed" : "passed", error: failure ? String(failure.stack ?? failure) : undefined,
    label, source, exec_timeout_ms: stack.execTimeoutMs, observations: stack.observations,
    effects: await effects(stack), http: stack.http, final_ledger: finalLedger, r2_objects: objects };
  await writeFile(join(stack.dir, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
  await writeFile(join(stack.dir, "wire.json"), JSON.stringify(stack.wire, null, 2) + "\n");
  await writeFile(join(stack.dir, "hand-attachment.jsonl"), attachmentLog.slice(stack.attachmentStart).map(entry => JSON.stringify(entry)).join("\n") + "\n");
  await writeFile(join(stack.dir, "README.md"), "Scenario: " + stack.name + "\nSource: " + JSON.stringify(source) + "\nStatus: "
    + (failure ? "FAIL: " + failure.message : "PASS") + "\nFiles: summary.json (assertions' observations, HTTP index, final ledger, R2 objects), "
    + "wire.json (Hand<->broker frames, closes, held/dropped/mutated frames), http/ (curl request, response headers and bodies), "
    + "curl-commands.sh, workerd.log (native workerd stdout/stderr incl. fixture R2 fault log), do-state/ (SQLite), r2-state/, hand/ (effects.log, payloads).\n");
  await appendFile(join(outputRoot, "index.jsonl"), JSON.stringify({ scenario: stack.name, outcome: summary.outcome, error: failure?.message }) + "\n");
}

/** Holds (pause) or permanently stops (stop) a Hand runtime's reconnects. */
function gate(stack, tag, stop) {
  let open; const entry = { stop, promise: new Promise(resolve => { open = resolve; }) };
  entry.resolve = () => { if (stack.gates.get(tag) === entry) stack.gates.delete(tag); open(); };
  stack.gates.set(tag, entry);
  return entry;
}

function journey(name, options, body) {
  test(name, { timeout: 300_000 }, async () => {
    const stack = await startStack(name.replace(/[^A-Za-z0-9]+/g, "-").replace(/^-|-$/g, "").toLowerCase().slice(0, 60), options);
    let failure;
    try { await body(stack); }
    catch (error) { failure = error; throw error; }
    finally { await stopStack(stack, failure); }
  });
}

const mutateAscii = text => { const index = text.indexOf("abcdefghijklmnopqrstuvwxyz012345"); assert.ok(index > 0); return text.slice(0, index) + "Abcdefghijklmnopqrstuvwxyz012345" + text.slice(index + 32); };
const gated = (id, file, marker) => "printf '%s\\n' '" + id + "' >> effects.log; while [ ! -f " + file + " ]; do sleep 0.02; done; printf " + marker;

journey("exact 8 MiB ASCII and Unicode results survive hibernation while small receipts stay inline", {}, async stack => {
  const plans = [["small-1", "ascii", 4096], ["ascii-8MiB", "ascii", 8 * MiB], ["mixed-8MiB", "mixed", 8 * MiB], ["small-2", "mixed", 4096]];
  for (const [id, kind, bytes] of plans) assertExact(await invoke(stack, id, await prepare(stack, id, kind, bytes)), id, stack, id + " first result");
  const ledger = await inspect(stack);
  stack.observations.rows = ledger.calls.map(({ result_json, receipt_json, ...rest }) => ({ ...rest, result_head: result_json?.slice(0, 300) }));
  stack.observations.small_rows = {};
  stack.observations.archive = {};
  for (const [id, , bytes] of plans) {
    const call = ledger.calls.find(entry => entry.source_call_id === id);
    assert.equal(call.state, "completed"); assert.equal(call.result_status, "completed"); assert.equal(call.result_success, 1);
    if (bytes > MiB) {
      assert.ok(call.result_bytes < 4096, id + " row keeps a small reference");
      const ref = reference(call.result_json);
      assert.ok(ref.key.includes("/results/" + encodeURIComponent(call.call_id) + "/"), "key is scoped to the transport call");
      stack.observations.archive[id] = await assertArchiveObject(stack, ref, id, "result");
    } else {
      assert.ok(!call.result_json.startsWith(ARCHIVE_PREFIX), id + " stays inline");
      const outcome = JSON.parse(call.result_json);
      assert.deepEqual(Object.keys(outcome), ["status", "output"]);
      assert.ok(outcome.output.structured_result.output === stack.expected.get(id), id + " inline row holds the exact outcome");
      assert.equal(call.result_json, JSON.stringify(outcome), id + " inline row is the normalized outcome text");
      stack.observations.small_rows[id] = call.result_json;
    }
  }
  assert.equal((await archived(stack)).length, 2, "only the two large outcomes are archived");
  const stats = await http(stack, "stats", "GET", "/hosted-tool-stats");
  assert.equal(stats.status, 200);
  const exec = stats.json.data.find(entry => entry.name === "exec_command" && entry.state === "completed");
  assert.equal(exec.calls, 4); assert.equal(exec.tool_failed, 0);
  stack.observations.stats = stats.json.data;
  // Owner restart: hibernate the broker, then replay identical call IDs over public HTTP.
  await stack.mf.unsafeEvictDurableObject("r2journey", "FixtureHands", { name: owner, webSockets: "hibernate" });
  for (const [id] of plans) {
    assertExact(await invoke(stack, id, command(id, id + ".txt"), { name: "replay-" + id }), id, stack, id + " replay after hibernation");
    assertExact(await receipt(stack, id), id, stack, id + " receipt after hibernation");
  }
  assert.deepEqual(await effects(stack), plans.map(([id]) => id), "each command ran exactly once");
  for (const [id] of plans) assert.equal(callFrames(stack, id).length, 1, id + " dispatched once");
});

journey("lost ACK: an identical resend is replayed, a different resend conflicts and never replaces the receipt", {}, async stack => {
  const routeBefore = await liveRoute(stack);
  const same = "lost-ack-identical", other = "lost-ack-different";
  stack.rules.set(same, { dropAck: 1 });
  assertExact(await invoke(stack, same, await prepare(stack, same, "mixed", 8 * MiB)), same, stack, "identical first result");
  await waitFor("replayed identical result acknowledged", () => acks(stack, same).length >= 1 && resultSends(stack, same).length >= 2);
  assert.ok(acks(stack, same)[0].attempt > resultSends(stack, same)[0].attempt, "ACK arrived on the reconnected socket");
  const routeAfter = await liveRoute(stack);
  assert.equal(routeAfter.lease_id, routeBefore.lease_id); assert.equal(routeAfter.generation, routeBefore.generation);
  stack.rules.set(other, { dropAck: 1, mutateOn: 2, mutate: mutateAscii });
  assertExact(await invoke(stack, other, await prepare(stack, other, "ascii", 8 * MiB)), other, stack, "different first result");
  const mutated = await waitFor("mutated resend sent", () => resultSends(stack, other).find(entry => entry.mutated));
  const closed = await waitFor("conflicting socket closed", () => stack.wire.find(entry => entry.event === "close" && entry.attempt === mutated.attempt));
  stack.observations.conflict_close = closed;
  assert.equal(acks(stack, other).filter(entry => entry.attempt === mutated.attempt).length, 0, "conflicting resend is never acknowledged");
  await receiptRetained(stack, other, "receipt keeps the original bytes");
  assertExact(await invoke(stack, other, command(other, other + ".txt"), { name: "replay-" + other }), other, stack, "replay keeps the original bytes");
  const objects = await archived(stack);
  const call = await row(stack, other);
  assert.equal(objects.filter(object => object.key.includes("/" + encodeURIComponent(call.call_id) + "/")).length, 1, "no object for the different bytes");
  assert.deepEqual(await effects(stack), [same, other]);
  stack.observations.wire_results = stack.wire.filter(entry => entry.type === "result" || entry.type === "ack" || entry.event === "close");
});

journey("late result after the deadline is archived as receipt evidence; identical duplicate is acked, different conflicts", { execTimeoutMs: 5_000 }, async stack => {
  for (const [id, kind, mutate] of [["late-identical", "mixed"], ["late-different", "ascii", mutateAscii]]) {
    stack.rules.set(id, { holdSend: true, dropAck: 1, ...(mutate ? { mutateOn: 2, mutate } : {}) });
    const pending = invoke(stack, id, await prepare(stack, id, kind, 8 * MiB), { yieldMs: 3_000 });
    await waitFor(id + " result held on the Hand", () => stack.rules.get(id).held);
    const settled = await bounded(pending, id + " deadline", 15_000);
    assertAmbiguous(settled, id + " HTTP settles ambiguous at its deadline");
    const before = await row(stack, id);
    assert.ok(Date.now() > before.deadline_at, "deadline passed"); assert.equal(before.state, "ambiguous");
    assert.equal(before.receipt_json, null);
    stack.rules.get(id).release();
    await waitFor(id + " late result resent after dropped ACK", () => resultSends(stack, id).length >= 2);
    if (mutate) {
      const sent = resultSends(stack, id).find(entry => entry.mutated);
      await waitFor("different late duplicate closed", () => stack.wire.find(entry => entry.event === "close" && entry.attempt === sent.attempt));
      assert.equal(acks(stack, id).filter(entry => entry.attempt === sent.attempt).length, 0, "different duplicate never acknowledged");
    } else await waitFor(id + " duplicate acknowledged", () => acks(stack, id).length >= 1);
    const after = await row(stack, id);
    assert.equal(after.state, "ambiguous");
    stack.observations[id] = { deadline_at: before.deadline_at, archive: await assertArchiveObject(stack, reference(after.receipt_json), id, "receipt") };
    if (mutate) await receiptRetained(stack, id, id + " receipt keeps the original late bytes");
    else {
      assertExact(await receipt(stack, id), id, stack, id + " receipt returns the exact late result");
      // An unreadable late-receipt archive is unresolved, never missing; restoring it resolves the same receipt.
      const key = reference(after.receipt_json).key;
      const original = Buffer.from(await (await stack.bucket.get(key)).arrayBuffer());
      await stack.bucket.delete(key);
      const unresolved = await receipt(stack, id, "receipt-late-archive-missing-" + id);
      assert.equal(unresolved.status, 409, unresolved.text); assert.equal(unresolved.json.error, "receipt_unresolved"); assert.equal(unresolved.json.admission, "retained");
      await stack.bucket.put(key, original);
      assertExact(await receipt(stack, id, "receipt-late-archive-restored-" + id), id, stack, id + " restored late receipt");
    }
  }
  const stats = await http(stack, "stats", "GET", "/hosted-tool-stats");
  assert.equal(stats.json.data.find(entry => entry.name === "exec_command" && entry.state === "ambiguous").late_receipts, 2);
  assert.deepEqual(await effects(stack), ["late-identical", "late-different"]);
});

journey("owner runtime eviction after dispatch: the Hand resends to the restarted broker and the large result lands once", {}, async stack => {
  const id = "evicted-8MiB";
  // Hibernation eviction cannot remove an object while its /invoke is in
  // flight (Miniflare waits for the request), so evict by restarting workerd.
  stack.rules.set(id, { holdSend: true });
  const pending = invoke(stack, id, await prepare(stack, id, "mixed", 8 * MiB));
  await waitFor("result held after the shell ran", () => stack.rules.get(id).held);
  const before = await row(stack, id);
  assert.equal(before.state, "dispatched");
  await restartRuntime(stack);
  const interrupted = await bounded(pending, "evicted invoke", 30_000);
  stack.observations.interrupted_invoke = { status: interrupted.status, body: interrupted.text.slice(0, 400), curl_error: interrupted.curlError };
  assert.notEqual(interrupted.json?.success, true, "the original request died with its runtime");
  await waitFor("ACK from the restarted broker", () => acks(stack, id).length >= 1);
  const after = await row(stack, id);
  assert.equal(after.state, "completed"); assert.equal(after.deadline_at, before.deadline_at, "original deadline kept");
  assertExact(await receipt(stack, id), id, stack, "receipt after eviction");
  assertExact(await invoke(stack, id, command(id, id + ".txt"), { name: "replay-" + id }), id, stack, "replay after eviction");
  stack.observations.archive = await assertArchiveObject(stack, reference(after.result_json), id, "result");
  assert.deepEqual(await effects(stack), [id]); assert.equal(callFrames(stack, id).length, 1);
  assert.ok(acks(stack, id).every(entry => entry.attempt > 1), "ACK only from the restarted broker");
});

journey("crash after the R2 object is stored but before the ledger CAS: the resend reuses the immutable object once", {}, async stack => {
  const id = "crash-after-put";
  const hold = await plan(stack, "store_then_hold");
  const pending = invoke(stack, id, await prepare(stack, id, "ascii", 8 * MiB));
  await held(stack, hold);
  const before = await row(stack, id);
  assert.equal(before.state, "dispatched", "no ledger write before put() returns");
  assert.equal(before.result_json, null);
  const prefix = "/results/" + encodeURIComponent(before.call_id) + "/";
  assert.equal((await archived(stack)).filter(object => object.key.includes(prefix)).length, 1, "complete object already durable");
  await restartRuntime(stack);
  const interrupted = await bounded(pending, "crashed invoke", 30_000);
  stack.observations.interrupted_invoke = { status: interrupted.status, body: interrupted.text.slice(0, 400), curl_error: interrupted.curlError };
  assert.notEqual(interrupted.json?.success, true);
  await waitFor("ACK from the restarted broker", () => acks(stack, id).length >= 1);
  const after = await row(stack, id);
  assert.equal(after.state, "completed");
  const log = await readFile(join(stack.dir, "workerd.log"), "utf8");
  const puts = log.split("\n").filter(line => line.includes('"fixture":"r2_fault"') && line.includes(prefix)).map(line => JSON.parse(line.slice(line.indexOf("{"))));
  stack.observations.puts = puts;
  assert.ok(puts.some(entry => entry.event === "put_done" && entry.stored === true), "first put stored the object before the crash");
  assert.ok(puts.some(entry => entry.event === "put_done" && entry.stored === false), "resend put was a conditional no-op (null), then head-verified");
  stack.observations.archive = await assertArchiveObject(stack, reference(after.result_json), id, "result");
  assert.equal((await archived(stack)).filter(object => object.key.includes(prefix)).length, 1, "exactly one object");
  assertExact(await receipt(stack, id), id, stack, "receipt after crash");
  assertExact(await invoke(stack, id, command(id, id + ".txt"), { name: "replay-" + id }), id, stack, "replay after crash");
  assert.deepEqual(await effects(stack), [id], "one effect"); assert.equal(callFrames(stack, id).length, 1, "one dispatch");
});

journey("finite Hand lease expiring with no live socket during a held put retains the late receipt, never success or ACK", {}, async stack => {
  const id = "leased-expiry-during-put", leasedMachine = "fixture-leased-hand";
  await stack.bucket.put("__lease/mode", "valid");
  const leasedTools = await createTools({ attachmentId: leasedMachine,
    machines: [{ id: leasedMachine, name: "Synthetic leased VM", workspace: stack.handDir, capabilities: ["shell"] }],
    tools: stack.native.tools.map(tool => ({ ...tool, timeoutMs: 20_000 })) });
  stack.extraTools = leasedTools;
  await attach(stack, "leased", { path: "/leased/attach", tools: leasedTools });
  const leasedInspect = async () => (await fetch(new URL("/leased/inspect", stack.base))).json();
  const leasedRow = async () => (await leasedInspect()).calls[0];
  const hold = await plan(stack, "hold");
  const pending = http(stack, "leased-invoke-" + id, "POST", "/leased/invoke",
    { call_id: id, input: { cmd: await prepare(stack, id, "mixed", 2 * MiB + 17), shell: "/bin/sh", login: false, yield_time_ms: 5_000 } });
  await held(stack, hold);
  assert.equal((await leasedRow()).state, "dispatched");
  // The Hand loses its socket and cannot reconnect; the authority stops renewing.
  const paused = gate(stack, "leased", true);
  const socket = stack.sockets.filter(entry => entry.tag === "leased").at(-1);
  socket.terminate();
  await waitFor("leased socket closed", () => stack.wire.find(entry => entry.event === "close" && entry.attempt === socket.attempt));
  await stack.bucket.put("__lease/mode", "expired");
  const route = (await leasedInspect()).routes.find(entry => entry.lease_id);
  stack.observations.route_at_close = route;
  assert.ok(route && route.lease_expires_at < Number.MAX_SAFE_INTEGER, "finite lease");
  await waitFor("lease expired", () => Date.now() > route.lease_expires_at + 200);
  const before = await leasedRow();
  stack.observations.row_before_release = { state: before.state, receipt: before.receipt_json?.slice(0, 80) ?? null };
  await release(stack, hold);
  const after = await waitFor("late receipt retained after expiry", async () => { const call = await leasedRow(); return call.receipt_json ? call : undefined; }, 15_000);
  stack.observations.row_after = { state: after.state, result_json: after.result_json?.slice(0, 200), receipt: after.receipt_json.slice(0, 120) };
  assert.equal(after.state, "ambiguous", "never terminal success after the lease expired");
  assert.ok(!String(after.result_json).startsWith(ARCHIVE_PREFIX), "result is not the archived success");
  stack.observations.archive = await assertArchiveObject(stack, reference(after.receipt_json), id, "receipt");
  assert.equal(acks(stack, id).length, 0, "no ACK");
  const settled = await bounded(pending, "leased invoke", 30_000);
  stack.observations.leased_invoke = { status: settled.status, body: settled.text.slice(0, 300) };
  assert.notEqual(settled.json?.success, true, "the caller never sees success");
  assert.deepEqual(await effects(stack), [id]);
  paused.resolve();
});

journey("R2 put failure closes transiently, keeps the generation and other in-flight work, and completes once", {}, async stack => {
  const routeBefore = await liveRoute(stack);
  const otherId = "put-fail-other-inflight", id = "put-fail-8MiB";
  const other = invoke(stack, otherId, gated(otherId, "release-other", "OTHER_DONE"), { yieldMs: 15_000 });
  await waitFor("other call running", async () => (await effects(stack)).includes(otherId));
  const failId = await plan(stack, "fail");
  assertExact(await invoke(stack, id, await prepare(stack, id, "ascii", 8 * MiB)), id, stack, "large result after one failed put");
  const close = stack.wire.find(entry => entry.event === "close" && entry.code === 1011);
  assert.ok(close, "storage failure closed the socket with 1011");
  stack.observations.store_failure_close = close;
  const routeAfter = await liveRoute(stack);
  assert.equal(routeAfter.lease_id, routeBefore.lease_id, "lease kept"); assert.equal(routeAfter.generation, routeBefore.generation, "generation kept");
  const resent = resultSends(stack, id).filter(entry => entry.attempt > close.attempt);
  assert.ok(resent.length >= 1, "the Hand resent its unacknowledged result on the reconnected socket");
  assert.ok(acks(stack, id).length >= 1 && acks(stack, id).every(entry => entry.attempt > close.attempt), "ACK only after the successful put, on the new socket");
  assert.equal((await row(stack, otherId)).state, "dispatched", "other in-flight call not made ambiguous");
  await writeFile(join(stack.handDir, "release-other"), "1");
  const otherResult = await bounded(other, "other call", 20_000);
  assert.equal(otherResult.json?.success, true, otherResult.text.slice(0, 400)); assert.equal(otherResult.json.structured_result.output, "OTHER_DONE");
  const log = await readFile(join(stack.dir, "workerd.log"), "utf8");
  assert.ok(log.includes('"event":"put_failed"') && log.includes('"id":"' + failId + '"'), "fixture recorded the injected failure");
  assert.deepEqual(await effects(stack), [otherId, id]);
  assert.equal(callFrames(stack, id).length, 1); assert.equal(callFrames(stack, otherId).length, 1);
  stack.observations.archive = await assertArchiveObject(stack, reference((await row(stack, id)).result_json), id, "result");
});

journey("slow R2 put racing the deadline and a transport loss", { execTimeoutMs: 6_000 }, async stack => {
  const late = "slow-put-deadline";
  const hold = await plan(stack, "hold");
  const pending = invoke(stack, late, await prepare(stack, late, "mixed", 8 * MiB), { yieldMs: 3_000 });
  await held(stack, hold);
  assertAmbiguous(await bounded(pending, "deadline during put", 20_000), "HTTP settles ambiguous at the deadline during the put");
  await release(stack, hold);
  await waitFor("late result acknowledged after the put", () => acks(stack, late).length === 1);
  const lateRow = await row(stack, late);
  assert.equal(lateRow.state, "ambiguous");
  stack.observations.deadline = await assertArchiveObject(stack, reference(lateRow.receipt_json), late, "receipt");
  assertExact(await receipt(stack, late), late, stack, "late receipt after slow put");

  const lost = "slow-put-transport-loss";
  const routeBefore = await liveRoute(stack);
  const hold2 = await plan(stack, "hold");
  const pending2 = invoke(stack, lost, await prepare(stack, lost, "ascii", 8 * MiB), { yieldMs: 3_000 });
  await held(stack, hold2);
  const first = stack.sockets.at(-1);
  first.terminate();
  await waitFor("Hand resent on its reconnected socket", () => resultSends(stack, lost).length >= 2);
  await release(stack, hold2);
  assertExact(await bounded(pending2, "transport loss during put", 20_000), lost, stack, "result after transport loss during put");
  await waitFor("ACK on the reconnected socket", () => acks(stack, lost).length >= 1);
  assert.ok(acks(stack, lost).every(entry => entry.attempt > first.attempt));
  const routeAfter = await liveRoute(stack);
  assert.equal(routeAfter.generation, routeBefore.generation); assert.equal(routeAfter.lease_id, routeBefore.lease_id);
  assert.deepEqual(await effects(stack), [late, lost]);
});

// Behavior check, not a proven old-fail: on resume the broker already deactivates
// the old attachment (close 1012 "runtime reattached"), so the first race is
// observed rather than forced. The second transient loss leaves no live socket
// when the put completes (the narrow 0c8a375c2 window); the epoch must survive.
journey("behavior check: same-runtime resume during a slow put, then a second transient loss with no live socket at put completion, keeps the epoch", {}, async stack => {
  const routeBefore = await liveRoute(stack);
  const otherId = "resume-other-inflight", id = "resume-during-put";
  const other = invoke(stack, otherId, gated(otherId, "release-resume-other", "RESUME_OTHER_DONE"), { yieldMs: 15_000 });
  await waitFor("other call running", async () => (await effects(stack)).includes(otherId));
  const hold = await plan(stack, "hold");
  const pending = invoke(stack, id, await prepare(stack, id, "mixed", 8 * MiB));
  await held(stack, hold);
  const old = stack.sockets.at(-1);
  old.partition();
  await waitFor("same runtime resumed and resent while the old socket is still open", () => resultSends(stack, id).some(entry => entry.attempt > old.attempt));
  stack.observations.old_socket_state_at_resume = old.readyState;
  // A second transient loss: the resumed socket also drops and the Hand cannot
  // reconnect until after the put completes (no live socket at completion).
  const resumed = stack.sockets.at(-1);
  assert.ok(resumed.attempt > old.attempt);
  const paused = gate(stack, "runtime-1", false);
  resumed.terminate();
  await waitFor("resumed socket closed", () => stack.wire.find(entry => entry.event === "close" && entry.attempt === resumed.attempt));
  await release(stack, hold);
  await waitFor("put completed with no live socket", async () => (await readFile(join(stack.dir, "workerd.log"), "utf8")).includes('"event":"put_done","n":1'));
  await delay(200);
  const routeDuring = await liveRoute(stack);
  assert.ok(routeDuring, "epoch retained while no socket is live");
  assert.equal(routeDuring.generation, routeBefore.generation);
  // Contract since 8fc6af299: the durable result is committed for its pinned call without an ACK.
  assert.equal((await row(stack, id)).state, "completed", "released archived result committed without ACK");
  assert.equal(acks(stack, id).length, 0, "nothing acknowledged on a dead socket");
  paused.resolve();
  assertExact(await bounded(pending, "resumed result", 20_000), id, stack, "exact result after resume during put");
  await waitFor("ACK on the resumed socket", () => acks(stack, id).length >= 1);
  assert.ok(acks(stack, id).every(entry => entry.attempt > resumed.attempt), "ACK only on the final resumed owner");
  assert.equal(stack.wire.filter(entry => entry.type === "ack" && entry.source === id && entry.event === "lost_partitioned").length, 0, "no ACK to the old socket");
  await delay(300);
  const routeAfter = await liveRoute(stack);
  assert.ok(routeAfter, "route still live");
  assert.equal(routeAfter.lease_id, routeBefore.lease_id); assert.equal(routeAfter.generation, routeBefore.generation);
  assert.equal((await row(stack, otherId)).state, "dispatched", "other in-flight call unaffected");
  await writeFile(join(stack.handDir, "release-resume-other"), "1");
  const otherResult = await bounded(other, "other call after resume", 20_000);
  assert.equal(otherResult.json?.success, true, otherResult.text.slice(0, 400)); assert.equal(otherResult.json.structured_result.output, "RESUME_OTHER_DONE");
  const next = "resume-next";
  assertExact(await invoke(stack, next, await prepare(stack, next, "ascii", 4096)), next, stack, "new work on the live generation");
  assert.deepEqual(await effects(stack), [otherId, id, next]);
  stack.observations.old_socket = { attempt: old.attempt, ready_state: old.readyState, closes: stack.wire.filter(entry => entry.event === "close") };
});

journey("slow R2 put racing Hand replacement and owner retirement retains the late receipt without re-running", { execTimeoutMs: 20_000 }, async stack => {
  const replaced = "slow-put-replacement";
  const hold = await plan(stack, "hold");
  const pending = invoke(stack, replaced, await prepare(stack, replaced, "mixed", 8 * MiB));
  await held(stack, hold);
  // The original Hand process dies; a new runtime of the same machine publishes.
  gate(stack, "runtime-1", true);
  stack.sockets.at(-1).terminate();
  await attach(stack, "runtime-2");
  assertAmbiguous(await bounded(pending, "replaced invoke", 20_000), "replacement settles the pinned call ambiguous");
  await release(stack, hold);
  const replacedRow = await waitFor("late receipt retained after replacement", async () => { const call = await row(stack, replaced); return call.receipt_json ? call : undefined; });
  assert.equal(replacedRow.state, "ambiguous");
  stack.observations.replacement = await assertArchiveObject(stack, reference(replacedRow.receipt_json), replaced, "receipt");
  await receiptRetained(stack, replaced, "receipt after replacement");
  assert.equal(callFrames(stack, replaced).length, 1, "replacement runtime never received the call");

  await refreshToken(stack);
  const retired = "slow-put-retire";
  const hold2 = await plan(stack, "hold");
  const pending2 = invoke(stack, retired, await prepare(stack, retired, "ascii", 8 * MiB));
  await held(stack, hold2);
  const forgot = await http(stack, "owner-forgets-hand", "POST", "/__fixture/forget");
  stack.observations.forget = forgot.json;
  assertAmbiguous(await bounded(pending2, "retired invoke", 20_000), "retirement settles the pinned call ambiguous");
  await release(stack, hold2);
  const retiredRow = await waitFor("late receipt retained after retirement", async () => { const call = await row(stack, retired); return call.receipt_json ? call : undefined; });
  stack.observations.retire = await assertArchiveObject(stack, reference(retiredRow.receipt_json), retired, "receipt");
  const retiredReceipt = await receipt(stack, retired);
  stack.observations.retired_receipt = { status: retiredReceipt.status, body: retiredReceipt.text.slice(0, 300) };
  assert.notEqual(retiredReceipt.json?.error, "receipt_missing", "a retained call is never reported missing");
  const replay = await invoke(stack, retired, command(retired, retired + ".txt"), { name: "replay-" + retired });
  stack.observations.retired_replay = { status: replay.status, body: replay.text.slice(0, 300) };
  assert.ok(!(replay.json?.success === true), "retired call is not re-run");
  assert.deepEqual(await effects(stack), [replaced, retired], "each command ran once");
});

journey("missing or corrupt archive objects are unresolved (409) and never re-run; restored objects resolve", {}, async stack => {
  const missing = "archive-missing", corrupt = "archive-corrupt";
  assertExact(await invoke(stack, missing, await prepare(stack, missing, "ascii", 8 * MiB)), missing, stack, missing);
  assertExact(await invoke(stack, corrupt, await prepare(stack, corrupt, "mixed", 8 * MiB)), corrupt, stack, corrupt);
  const keys = {}, originals = {};
  for (const id of [missing, corrupt]) {
    keys[id] = reference((await row(stack, id)).result_json).key;
    originals[id] = Buffer.from(await (await stack.bucket.get(keys[id])).arrayBuffer());
  }
  await stack.bucket.delete(keys[missing]);
  const damaged = Buffer.from(originals[corrupt]); damaged[damaged.length >> 1] ^= 0x01;
  await stack.bucket.put(keys[corrupt], damaged);
  for (const id of [missing, corrupt]) {
    const unresolved = await receipt(stack, id, "receipt-unreadable-" + id);
    assert.equal(unresolved.status, 409, unresolved.text); assert.equal(unresolved.json.error, "receipt_unresolved"); assert.equal(unresolved.json.admission, "retained");
    const replay = await invoke(stack, id, command(id, id + ".txt"), { name: "replay-unreadable-" + id });
    assertAmbiguous(replay, id + " replay with an unreadable archive");
    assert.match(JSON.stringify(replay.json), /not re-run/);
  }
  assert.deepEqual(await effects(stack), [missing, corrupt], "nothing re-ran");
  assert.equal(callFrames(stack, missing).length + callFrames(stack, corrupt).length, 2, "no readmission");
  for (const id of [missing, corrupt]) await stack.bucket.put(keys[id], originals[id]);
  for (const id of [missing, corrupt]) assertExact(await receipt(stack, id, "receipt-restored-" + id), id, stack, id + " restored");
  // Authority revoked while a delayed archive read is in flight: the read result is withheld.
  const readHold = await plan(stack, "hold", "getplan");
  const reading = receipt(stack, missing, "receipt-during-revocation");
  await held(stack, readHold);
  stack.observations.forget = (await http(stack, "owner-forgets-hand", "POST", "/__fixture/forget")).json;
  await release(stack, readHold);
  const revoked = await bounded(reading, "receipt during revocation", 20_000);
  stack.observations.revoked_receipt = { status: revoked.status, body: revoked.text.slice(0, 300) };
  assert.ok(!(revoked.json?.success === true), "no output after the owner revoked the Hand during the read");
  assert.notEqual(revoked.json?.error, "receipt_missing");
  assert.deepEqual(await effects(stack), [missing, corrupt]);
});

// Local archive backpressure on a command-recovery socket: frames beyond one
// write plus two waiters are dropped without ACK, the socket and epoch stay open,
// and each freed slot asks the same socket to resend one deferred result.
journey("concurrent large results beyond the archive queue are refused before ACK and all complete once", {}, async stack => {
  const ids = ["queue-1", "queue-2", "queue-3", "queue-4"];
  const hold = await plan(stack, "hold");
  const pending = [];
  for (const id of ids) pending.push(invoke(stack, id, await prepare(stack, id, "ascii", 2 * MiB + ids.indexOf(id))));
  await held(stack, hold);
  await waitFor("all four results sent", () => ids.every(id => resultSends(stack, id).length >= 1));
  const routeBefore = await liveRoute(stack);
  await delay(300);
  assert.equal(stack.wire.filter(entry => entry.dir === "broker" && entry.type === "ack").length, 0, "nothing acknowledged while the archive is held");
  await release(stack, hold);
  const results = await bounded(Promise.all(pending), "queued results", 60_000);
  results.forEach((result, index) => assertExact(result, ids[index], stack, ids[index]));
  const recovers = stack.wire.filter(entry => entry.dir === "broker" && entry.type === "recover");
  stack.observations.recover_frames = recovers;
  stack.observations.closes = stack.wire.filter(entry => entry.event === "close");
  assert.ok(recovers.length >= 1 && recovers.every(entry => entry.sources.every(source => ids.includes(source))), "overflowed results were requested again with recover frames");
  assert.equal(stack.wire.filter(entry => entry.event === "close").length, 0, "backpressure never closed the socket");
  const routeAfter = await liveRoute(stack);
  assert.equal(routeAfter.lease_id, routeBefore.lease_id); assert.equal(routeAfter.generation, routeBefore.generation);
  const log = await readFile(join(stack.dir, "workerd.log"), "utf8");
  stack.observations.puts = log.split("\n").filter(line => line.includes('"fixture":"r2_fault"') && line.includes('"event":"put"')).length;
  assert.deepEqual((await effects(stack)).sort(), ids);
  for (const id of ids) assert.equal(callFrames(stack, id).length, 1);
  assert.equal((await archived(stack)).length, 4);
  stack.observations.sends = Object.fromEntries(ids.map(id => [id, resultSends(stack, id).length]));
});
