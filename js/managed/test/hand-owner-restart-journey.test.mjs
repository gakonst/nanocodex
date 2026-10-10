import assert from "node:assert/strict";
import { fork } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import WebSocket from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";
import { createNodeProcessTools } from "nanocodex-tools/node";
import { fetch } from "./support/miniflare-fetch.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000011";
const thread = "00000000-0000-7000-8000-000000000012";
const machine = "synthetic-owner-restart-hand";
const credential = "Bearer synthetic-owner-restart-admission";
const command = "pnpm --filter nanocodex-managed-service exec node --test test/hand-owner-restart-journey.test.mjs";

// Only external admission credentials are a fixture. Every account endpoint,
// owner claim, broker transition, journal, native command and socket is real.
const source = `
import { AccountHostedTools } from './src/account-hosted-tools.ts';
export { AccountHostedTools };
export default { fetch(request, env) {
  if (request.headers.get('authorization') !== '${credential}') return new Response(null, {status:401});
  return env.ACCOUNT.getByName('${owner}').fetch(request);
} };
`;

// The supervisor lives in a separate process group with workerd. The publisher
// lives in the test process and survives SIGKILL. Reuse the reported HTTP port
// and the same SQLite directory; no in-flight eviction or fabricated rows.
const childSource = `
import { Miniflare } from ${JSON.stringify(import.meta.resolve("miniflare"))};
import { readFile } from 'node:fs/promises';
const directory = process.argv[2], port = Number(process.argv[3]);
try {
  const mf = new Miniflare({port, modules:true,
    script:await readFile(directory+'/worker.mjs','utf8'),
    compatibilityDate:'2026-07-30', compatibilityFlags:['nodejs_compat','enable_request_signal'],
    durableObjects:{ACCOUNT:{className:'AccountHostedTools',useSQLite:true}},
    durableObjectsPersist:directory+'/sqlite',
    handleRuntimeStdio(stdout,stderr) { stdout.pipe(process.stdout); stderr.pipe(process.stderr); }
  });
  const base = await mf.ready;
  process.send({ready:true,base:base.href});
} catch(error) { process.send({error:error.stack}); process.exitCode=1; }
`;

test("a killed account broker owner recovers the surviving Hand journal without reexecution", { timeout: 45_000 }, async () => {
  const output = join(root, "../../output/hand-owner-restart-journey", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand");
  await mkdir(workspace, { recursive: true });
  const trace = [], wire = [], clients = [], localHost = [], runtime = [], sockets = [];
  const result = { command, inputs: {owner, thread, machine, shell:"/bin/sh", loss:"SIGKILL process group", persistent_sqlite:true},
    expected: {recovered_output:"ORIGINAL_FINISHED", original_effect:"R", fresh_output:"RECONNECTED_OK", dispatches:2, same_route:true}, observed:{} };
  let child, base, port = 0, connector, tools, native, failure, pending;
  const phase = (name, value = {}) => trace.push({phase:name, at:Date.now(), ...value});
  const bounded = async (promise, description, ms = 8_000) => {
    const timer = new AbortController();
    try {
      return await Promise.race([promise, delay(ms, undefined, {signal:timer.signal}).then(() => {
        throw Error(`${description} exceeded ${ms}ms`);
      })]);
    } finally { timer.abort(); }
  };
  const waitFor = async (predicate, description) => {
    const deadline = performance.now() + 8_000;
    while (performance.now() < deadline) {
      const value = await predicate(); if (value) return value; await delay(10);
    }
    throw Error(`${description} exceeded 8000ms`);
  };
  const start = async () => {
    const current = fork(join(output, "runtime-process.mjs"), [output, String(port)], {
      detached:true, stdio:["ignore", "pipe", "pipe", "ipc"],
    });
    child = current;
    current.stdout.on("data", chunk => runtime.push({pid:current.pid, stream:"stdout", text:String(chunk)}));
    current.stderr.on("data", chunk => runtime.push({pid:current.pid, stream:"stderr", text:String(chunk)}));
    const ready = await bounded(new Promise((resolve, reject) => {
      current.once("error", reject);
      current.once("exit", (code, signal) => reject(Error(`runtime exited before ready: ${code}/${signal}`)));
      current.once("message", message => message.ready ? resolve(message) : reject(Error(message.error)));
    }), "runtime readiness");
    base = new URL(ready.base);
    assert.ok(Number(base.port) > 0, "child must report a real HTTP port");
    if (port) assert.equal(Number(base.port), port);
    port = Number(base.port);
    phase("runtime_ready", {pid:current.pid, base:base.href});
  };
  const kill = async () => {
    const current = child;
    if (!current) return;
    const exited = new Promise(resolve => current.once("exit", (code, signal) => {
      phase("runtime_exit", {pid:current.pid, code, signal}); resolve({code, signal});
    }));
    process.kill(-current.pid, "SIGKILL");
    const exit = await bounded(exited, "SIGKILL runtime exit");
    child = undefined;
    assert.equal(exit.signal, "SIGKILL");
  };
  const request = async (path, body, extraHeaders = {}) => {
    const response = await fetch(new URL(path, base), {method:body === undefined ? "GET" : "POST",
      headers:{authorization:credential, "x-nanocodex-owner-id":owner, "content-type":"application/json", ...extraHeaders},
      body:body === undefined ? undefined : JSON.stringify(body), signal:AbortSignal.timeout(8_000)});
    const value = await response.json();
    return {status:response.status, value};
  };
  const api = async (path, body) => {
    const response = await request(path, body);
    assert.equal(response.status, 200, JSON.stringify({path, ...response})); return response.value;
  };
  const diagnostics = () => api(`/diagnostics?thread_id=${thread}&limit=1024`);
  const snapshot = () => api("/snapshot", {owner_id:owner});
  const route = state => state.machines.find(entry => entry.machine.id === machine)?.tools.find(tool => tool.name === "exec_command");
  const callFrames = () => wire.filter(row => row.direction === "broker" && row.frame.type === "call");
  const originalInfo = console.info;
  console.info = (record, ...rest) => {
    if (record?.type === "hand.attachment") clients.push(record);
    else originalInfo(record, ...rest);
  };
  try {
    const bundle = await build({stdin:{contents:source, resolveDir:root}, bundle:true, write:false,
      format:"esm", platform:"node", target:"es2022", conditions:["workerd"],
      banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},
      alias:{"node-rsa":root + "/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs"},
      external:["cloudflare:*", "node:*"], metafile:true, logLevel:"warning"});
    const candidateRoot=fileURLToPath(new URL("../../../",import.meta.url));
    const resolutions=Object.fromEntries(["nanocodex/tools","nanocodex-tools/attachment","nanocodex-tools/node"].map(name=>[name,fileURLToPath(import.meta.resolve(name))]));
    for(const path of Object.values(resolutions))assert.ok(path.startsWith(candidateRoot),`dependency escaped candidate: ${path}`);
    await writeFile(join(output,"source-resolution.json"),JSON.stringify({resolutions,bundleInputs:Object.keys(bundle.metafile.inputs)},null,2));
    await writeFile(join(output, "fixture-source.mjs"), source);
    await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
    await writeFile(join(output, "runtime-process.mjs"), childSource);
    await start();
    native = await createNodeProcessTools({workspace, onActivity:event => localHost.push({at:Date.now(), ...event})});
    tools = await createTools({tools:native.tools});
    const endpoint = new URL("/tool-host", base); endpoint.protocol = "ws:";
    connector = createAttachment(tools, {endpoint:endpoint.href, transport:{connect() {
      const attempt = sockets.length + 1;
      const socket = new WebSocket(endpoint, {headers:{authorization:credential, "x-nanocodex-owner-id":owner}});
      sockets.push(socket); wire.push({attempt, event:"connect", at:Date.now()});
      const send = socket.send.bind(socket);
      socket.send = (data, ...args) => {
        wire.push({attempt, direction:"host", at:Date.now(), frame:JSON.parse(String(data))});
        return send(data, ...args);
      };
      socket.on("message", data => wire.push({attempt, direction:"broker", at:Date.now(), frame:JSON.parse(String(data))}));
      socket.on("close", (code, reason) => wire.push({attempt, event:"close", at:Date.now(), code, reason:String(reason)}));
      socket.on("error", error => wire.push({attempt, event:"error", at:Date.now(), error:error.message}));
      return socket;
    }}}, {machines:[{id:machine, name:"Synthetic surviving Hand", workspace, capabilities:["shell"]}],
      attachmentId:machine, heartbeatMs:100, reconnectDelayMs:20, drainTimeoutMs:1000});
    const client = await bounded(connector.connect(), "initial publisher ready");
    assert.equal(client.connected, true);
    const initial = await snapshot(), oldRoute = route(initial);
    assert.ok(oldRoute?.route_token);
    const wrongOwner = "00000000-0000-4000-8000-000000000099";
    assert.equal((await request("/snapshot", {owner_id:wrongOwner})).status, 404);
    assert.equal((await request(`/diagnostics?thread_id=${thread}`, undefined, {"x-nanocodex-owner-id":wrongOwner})).status, 404);
    phase("initial_publication", {initial, oldRoute});

    const invocation = {owner_id:owner, name:"exec_command", machine_id:machine, session_id:thread, thread_id:thread,
      call_id:"owner-lost-call", model:"synthetic-model", route_token:oldRoute.route_token,
      input:{cmd:"printf R >> original.log; sleep 0.7; printf ORIGINAL_FINISHED > original-finished.log; printf ORIGINAL_FINISHED", shell:"/bin/sh", login:false, yield_time_ms:30000}};
    // Attach a rejection handler immediately: killing the real HTTP server is
    // expected to reject this request, even before the restart has begun.
    pending = api("/invoke", invocation).then(value => ({value}), error => ({error:error.message}));
    const before = await waitFor(async () => {
      const page = await diagnostics();
      const execution = page.events.find(event => event.source_call_id === invocation.call_id
        && event.stage === "host_progress" && event.host_stage === "execution_started");
      if (!execution) return;
      try { return await readFile(join(workspace, "original.log"), "utf8") === "R" ? {page, execution} : undefined; }
      catch (error) { if (error.code !== "ENOENT") throw error; }
    }, "actual broker execution-start progress and native effect");
    const sent = before.page.events.find(event => event.source_call_id === invocation.call_id && event.stage === "sent");
    assert.ok(sent, "actual dispatch must be visible before physical process loss");
    assert.equal(callFrames().length, 1);
    await assert.rejects(readFile(join(workspace, "original-finished.log")), {code:"ENOENT"},
      "kill must occur while the original shell is still executing");
    phase("execution_started_before_kill", before);
    await kill();
    const originalHttp = await bounded(pending, "original interrupted HTTP request");
    assert.ok(originalHttp.error, JSON.stringify(originalHttp));
    phase("original_http_interrupted", originalHttp);
    await start();
    const recovered = await waitFor(async () => {
      const state = await snapshot();
      if (!client.connected || !state.machines.find(entry => entry.machine.id === machine)?.online) return;
      const recovery = wire.find(row => row.direction === "broker" && row.frame.type === "recover"
        && row.frame.call_ids.includes(sent.transport_call_id));
      return recovery ? {state, recovery, page:await diagnostics()} : undefined;
    }, "surviving publisher ready and persisted call recovery request");
    const catalogs = wire.filter(row => row.direction === "host" && row.frame.type === "catalog");
    assert.ok(catalogs.length >= 2);
    assert.equal(catalogs.at(-1).frame.runtime_id, catalogs[0].frame.runtime_id);
    assert.notEqual(catalogs.at(-1).frame.connection_id, catalogs[0].frame.connection_id);
    phase("owner_restarted", recovered);

    const freshRoute = route(recovered.state);
    assert.ok(freshRoute?.route_token);
    assert.equal(freshRoute.route_token, oldRoute.route_token, "living runtime resumes retained ownership epoch");
    const dispatches = callFrames().length;
    const exactOld = await request("/invoke", invocation);
    assert.equal(exactOld.status, 200);
    assert.equal(exactOld.value.success, true);
    assert.equal(exactOld.value.structured_result.output, "ORIGINAL_FINISHED");
    assert.equal(exactOld.value.structured_result.exit_code, 0);
    const replay = await api("/invoke", invocation);
    assert.equal(replay.success, true); assert.equal(replay.structured_result.output, "ORIGINAL_FINISHED");
    assert.equal(callFrames().length, dispatches, "recover must never resend original command");
    const fresh = await api("/invoke", {...invocation, call_id:"fresh-after-owner-restart", route_token:freshRoute.route_token,
      input:{cmd:"printf F >> fresh.log; printf RECONNECTED_OK", shell:"/bin/sh", login:false, yield_time_ms:1000}});
    assert.equal(fresh.success, true); assert.equal(fresh.structured_result.output, "RECONNECTED_OK");
    assert.equal(fresh.structured_result.exit_code, 0);
    await waitFor(async () => {
      try { return await readFile(join(workspace, "original-finished.log"), "utf8") === "ORIGINAL_FINISHED"; }
      catch (error) { if (error.code !== "ENOENT") throw error; }
    }, "original native command completed outside killed runtime");
    assert.equal(await readFile(join(workspace, "original.log"), "utf8"), "R");
    assert.equal(await readFile(join(workspace, "fresh.log"), "utf8"), "F");
    assert.equal(callFrames().length, 2);
    const final = await diagnostics();
    assert.equal(final.available, true); assert.equal(final.write_failed, false);
    phase("replay_and_fresh_result", {exactOld, replay, fresh, final});
    result.observed = {recovered_output:replay.structured_result.output,
      original_http_error:originalHttp.error, original_effect:"R", original_completed:true,
      fresh_effect:"F", fresh_output:fresh.structured_result.output, dispatches:callFrames().length,
      same_runtime:true, new_connection:true, retained_call_ids:true, same_route:true, stale_route_status:exactOld.status};
    console.log(JSON.stringify({evidence:output, ...result.observed}));
  } catch (error) { failure = error; result.error = error.stack; throw error; }
  finally {
    try { if (connector) await bounded(connector.close(), "attachment cleanup", 2500); }
    finally {
      for (const socket of sockets) socket.terminate();
      try { await tools?.close(); await native?.close(); }
      finally {
        try { await kill(); }
        finally {
          console.info = originalInfo;
          await writeFile(join(output, "trace.json"), JSON.stringify({result, trace}, null, 2) + "\n");
          await writeFile(join(output, "wire.json"), JSON.stringify(wire, null, 2) + "\n");
          await writeFile(join(output, "client-records.json"), JSON.stringify(clients, null, 2) + "\n");
          await writeFile(join(output, "local-host.json"), JSON.stringify(localHost, null, 2) + "\n");
          await writeFile(join(output, "runtime.log"), runtime.map(row => `[${row.pid} ${row.stream}] ${row.text}`).join("") + "\n");
          await writeFile(join(output, "README.md"), `Run: \`${command}\`\n\nInputs: ${JSON.stringify(result.inputs)}\n\nExpected: ${JSON.stringify(result.expected)}\n\nObserved: ${JSON.stringify(result.observed)}\n\nStatus: ${failure ? "FAIL: " + failure.message : "PASS"}\n\nEvidence: trace.json (phases and actual account HTTP diagnostics/results), wire.json (real WebSocket frames), client-records.json (shipped attachment), local-host.json (native process activity), runtime.log (child/workerd output), hand/*.log (native effects), fixture-source.mjs and worker.mjs, runtime-process.mjs, sqlite/ (restarted persistent broker storage). Only admission credentials are stubbed. The Hand and native shell run outside the killed runtime group.\n`);
        }
      }
    }
  }
});
