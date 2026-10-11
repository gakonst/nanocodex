import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { mkdir, readFile, realpath, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { fetch } from "./support/miniflare-fetch.mjs";

// Run with node --test test/hand-communication-journey.test.mjs from js/managed.
// Only SOURCE_ROOT changes the implementation under measurement. LABEL names
// independent baseline/candidate runs; generated evidence stays in output/.
const checkout = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const repo = resolve(process.env.NANOCODEX_BENCHMARK_SOURCE_ROOT ?? checkout);
const label = process.env.NANOCODEX_BENCHMARK_LABEL ?? "baseline";
assert.match(label, /^[A-Za-z0-9_.-]+$/);
const output = join(checkout, "output/hand-communication-journey", label, `${Date.now()}-${process.pid}`);
const require = createRequire(join(repo, "js/managed/package.json"));
const { build } = require("esbuild");
const { Miniflare } = require("miniflare");
const { WebSocket } = require("ws");
const { createTools } = await import(pathToFileURL(join(repo, "js/nanocodex/tools/Tools.mjs")));
const { createNodeProcessTools } = await import(pathToFileURL(join(repo, "js/nanocodex-tools/tools/nodeProcess.mjs")));
const owner = "00000000-0000-4000-8000-000000000003";
const otherOwner = "00000000-0000-4000-8000-000000000099";
const thread = "00000000-0000-7000-8000-000000000001";
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function distribution(values) {
  const sorted = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!sorted.length) return { count: 0 };
  const quantile = q => sorted[Math.max(0, Math.ceil(sorted.length * q) - 1)];
  return { count: sorted.length, min: sorted[0], p50: quantile(.5), p95: quantile(.95),
    p99: quantile(.99), max: sorted.at(-1), mean: sorted.reduce((a, b) => a + b, 0) / sorted.length };
}

test("measure real Brain–Hand shell journeys with preserved correlated phases", { timeout: 120_000 }, async () => {
  await mkdir(join(output, "hand"), { recursive: true });
  const observations = [], workerLines = [], wire = [], journeys = [], blocked = [], ownership = [];
  let externalCalls = 0, attachment, mf, native, tools, base;
  const external = createServer((request, response) => {
    if (request.url !== "/cua") { response.writeHead(404).end(); return; }
    externalCalls++;
    blocked.push(response);
  });
  external.listen(0, "127.0.0.1");
  await once(external, "listening");
  const externalURL = `http://127.0.0.1:${external.address().port}/cua`;
  const originalInfo = console.info;
  console.info = (record, ...rest) => {
    if (record?.type === "hand.attachment") observations.push(record);
    else originalInfo(record, ...rest);
  };
  const capture = line => {
    workerLines.push(line);
    const start = line.indexOf('{"type":');
    if (start >= 0) { try { observations.push(JSON.parse(line.slice(start))); } catch {} }
  };
  async function within(promise, name) {
    let timer;
    try { return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Journey deadline: ${name}; evidence ${output}`)), 30_000);
    })]); } finally { clearTimeout(timer); }
  }
  const release = () => { for (const response of blocked.splice(0)) response.end("CUA_RELEASED"); };
  async function request(path, body, authorization = "Bearer fixture") {
    const started = performance.now();
    const response = await fetch(new URL(path, base), { method: "POST",
      headers: { authorization, "content-type": "application/json" }, body: JSON.stringify(body) });
    const value = await response.json();
    return { status: response.status, value, client_total_ms: performance.now() - started };
  }
  async function invoke(group, index, name = "exec_command", input, identity = `${group}-${index}`) {
    const callId = identity;
    const result = await within(request("/namespace", { name, call_id: callId,
      input: input ?? { workdir: "/fixture-hand", shell: "/bin/sh", login: false, yield_time_ms: 1000,
        cmd: `printf '%s\\n' '${callId}' >> effects.log; printf '%s' '${callId}'` } }), callId);
    journeys.push({ group, call_id: callId, client_total_ms: result.client_total_ms, status: result.status });
    assert.equal(result.status, 200, JSON.stringify(result.value));
    assert.equal(result.value.result.success, true, JSON.stringify(result.value));
    if (name === "exec_command") {
      assert.equal(result.value.result.structuredResult.output, callId);
      assert.equal(result.value.result.structuredResult.exit_code, 0);
    }
    return result;
  }
  async function checkOwnership(stage) {
    const snapshot = await request("/snapshot", { owner_id: owner });
    assert.equal(snapshot.status, 200);
    const machine = snapshot.value.machines.find(row => row.machine.id === "fixture-hand");
    const route = machine.tools.find(row => row.name === "exec_command").route_token;
    const wrongSnapshot = await request("/snapshot", { owner_id: otherOwner });
    const wrongInvoke = await request("/invoke", { owner_id: otherOwner, name: "exec_command", machine_id: "fixture-hand",
      session_id: "fixture-session", thread_id: thread, turn_id: "fixture-turn", call_id: `denied-${stage}`,
      route_token: route, input: { cmd: "printf SHOULD_NOT_RUN >> effects.log", shell: "/bin/sh", login: false } });
    const wrongClaimStatus = await within(new Promise((resolve, reject) => {
      const socket = new WebSocket(new URL("/tool-host", base).href.replace(/^http/, "ws"), { headers: { "x-nanocodex-owner-id": otherOwner } });
      let answered = false;
      socket.on("unexpected-response", (_request, response) => {
        answered = true; response.resume(); socket.terminate(); resolve(response.statusCode);
      });
      socket.on("open", () => { socket.close(); reject(new Error("Mismatched owner claimed the account")); });
      socket.on("error", error => { if (!answered) reject(error); });
    }), "owner mismatch handshake");
    assert.equal(wrongSnapshot.status, 404); assert.equal(wrongInvoke.status, 404); assert.equal(wrongClaimStatus, 404);
    const acceptedAgain = await request("/snapshot", { owner_id: owner });
    assert.equal(acceptedAgain.status, 200, "rejected owner must not replace durable ownership");
    const diagnosticURL = new URL(`/diagnostics?thread_id=${thread}&limit=1024`, base);
    const wrongDiagnostics = await fetch(diagnosticURL, { headers: { "x-nanocodex-owner-id": otherOwner } });
    assert.equal(wrongDiagnostics.status, 404); await wrongDiagnostics.body?.cancel();
    const persisted = [];
    let after = 0;
    while (true) {
      diagnosticURL.searchParams.set("after", String(after));
      const response = await fetch(diagnosticURL, { headers: { "x-nanocodex-owner-id": owner } });
      assert.equal(response.status, 200);
      const page = await response.json();
      assert.equal(page.available, true); assert.equal(page.write_failed, false);
      persisted.push(...page.events);
      if (page.next_after === after) break;
      after = page.next_after;
    }
    // The archived pre-optimization journal omitted this field. The current
    // candidate must retain it through the real owner-only diagnostics API.
    if (repo === checkout && stage === "after-restart") {
      const retainedBurst = persisted.filter(row => row.type === "hand.call.broker" && row.source_call_id?.startsWith("burst50-"));
      assert.equal(retainedBurst.length, 650);
      assert.ok(retainedBurst.every(row => Number.isFinite(row.runtime_generation)), "runtime_generation must survive public diagnostics after restart");
      for (const row of retainedBurst.filter(row => ["receipt", "host_progress"].includes(row.stage)))
        for (const field of ["frame_decode_ms", "lease_validation_ms", "message_to_handler_ms"])
          assert.ok(Number.isFinite(row[field]), `${field} must survive owner restart`);
      assert.ok(retainedBurst.filter(row => row.stage === "receipt").every(row => Number.isFinite(row.dispatch_to_message_ms)));
    }
    ownership.push({ stage, snapshot_owner_status: snapshot.status, wrong_snapshot_status: wrongSnapshot.status,
      wrong_invoke_status: wrongInvoke.status, wrong_claim_status: wrongClaimStatus, accepted_again_status: acceptedAgain.status,
      wrong_diagnostics_status: wrongDiagnostics.status, persisted_events: persisted.length,
      persisted_runtime_generation_records: persisted.filter(row => Number.isFinite(row.runtime_generation)).length });
    await writeFile(join(output, `account-diagnostics-${stage}.json`), JSON.stringify(persisted, null, 2) + "\n");
  }
  try {
    const sourceCommit = await readFile(join(repo, ".benchmark-source-commit"), "utf8")
      .then(value => value.trim()).catch(() => execFileSync("git", ["-C", repo, "rev-parse", "HEAD"], { encoding: "utf8" }).trim());
    const source = `
import {DurableObject} from 'cloudflare:workers';
import {AccountHostedTools,AccountHostedToolsProvider} from './src/account-hosted-tools.ts';
import {createNamespaceExecutionRuntime} from './src/namespace-tools.ts';
export {AccountHostedTools};
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class Namespace extends DurableObject {
 constructor(ctx,env){super(ctx,env);
  this.provider=new AccountHostedToolsProvider(env.HANDS,'${owner}',()=>true,'${thread}');
  this.runtime=createNamespaceExecutionRuntime(c=>this.provider.machines(c),(id,name,c)=>this.provider.machineTool(id,name,c),undefined,undefined,()=> 'synthetic-authority',undefined,'${thread}');
 }
 async fetch(request){
  // The sole auth fixture replaces the external identity service.
  if(request.headers.get('authorization')!=='Bearer fixture')return Response.json({error:'unauthorized'},{status:401});
  const body=await request.json();
  await this.provider.refreshOptional(1000);
  const context={sessionId:'fixture-session',parentCallId:'fixture-cell',callId:body.call_id,model:'fixture-model',turnId:'fixture-turn',signal:request.signal};
  try{return Response.json({result:await this.runtime.tools[body.name].handler(body.input,context)});}
  catch(error){return Response.json({error:error.message},{status:500});}
 }
}
export default {fetch(request,env){const path=new URL(request.url).pathname;
 if(path==='/namespace')return env.NAMESPACE.getByName('fixture').fetch(request);
 return env.HANDS.getByName('${owner}').fetch(new Request('https://hand.internal'+path+new URL(request.url).search,request));}};
`;
    // Resolve the broker from the selected TS source, never stale generated dist.
    const bundle = await build({ stdin: { contents: source, resolveDir: join(repo, "js/managed") },
      bundle: true, write: false, metafile: true, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
      external: ["cloudflare:*", "node:*"], alias: {
        "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"),
        "nanocodex-tools/internal/hosted-machine": join(repo, "js/nanocodex-tools/tools/hostedMachine.mjs"),
        "nanocodex-tools": join(repo, "js/nanocodex-tools/src/index.ts"),
        "node-rsa": join(repo, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs"),
      }, logLevel: "silent" });
    const toolsRequire = createRequire(join(repo, "js/nanocodex/package.json"));
    const attachmentImport = await realpath(toolsRequire.resolve("nanocodex-tools/attachment"));
    assert.equal(attachmentImport, await realpath(join(repo, "js/nanocodex-tools/tools/attachment.mjs")), "Node attachment imported outside measured source root");
    const sourceFiles = ["js/managed/src/account-hosted-tools.ts", "js/managed/src/namespace-tools.ts",
      "js/managed/src/hosted-tools-broker.ts", "js/managed/src/hand-call-observation.ts", "js/managed/src/diagnostic-journal.ts",
      "js/nanocodex-tools/src/hosted/broker-core.ts", "js/nanocodex-tools/src/hosted/protocol.ts",
      "js/nanocodex-tools/tools/attachment.mjs", "js/nanocodex-tools/tools/nodeProcess.mjs",
      "js/nanocodex-tools/tools/processOutput.mjs"];
    const hashes = Object.fromEntries(await Promise.all(sourceFiles.map(async path =>
      [path, createHash("sha256").update(await readFile(join(repo, path))).digest("hex")])));
    const workspaceInputs = Object.keys(bundle.metafile.inputs).map(path => resolve(path)).filter(path => path.includes("/js/nanocodex"));
    assert.ok(workspaceInputs.every(path => path.startsWith(`${repo}/`)), JSON.stringify(workspaceInputs));
    const metadata = { label, source_root: repo, source_commit: sourceCommit, source_sha256: hashes,
      source_dirty: await readFile(join(repo, ".benchmark-source-status"), "utf8").then(value => value.trim())
        .catch(() => execFileSync("git", ["-C", repo, "status", "--short", "--", ...sourceFiles], { encoding: "utf8" }).trim()),
      attachment_import: attachmentImport, worker_workspace_inputs: workspaceInputs, node: process.version,
      transport: "actual loopback HTTP + Node ws reverse publisher + workerd SQLite Durable Objects",
      fixture: "synthetic owner; external identity and CUA service only; /bin/sh login=false",
      clocks: "local monotonic durations; transit_return_overhead_ms is broker roundtrip minus host elapsed, never one-way network time",
      discovery: "provider.refreshOptional(1000): initial discovery once, reuse fresh snapshot/captured cell across warm calls",
      command: `NANOCODEX_BENCHMARK_SOURCE_ROOT=${repo} NANOCODEX_BENCHMARK_LABEL=${label} node --test test/hand-communication-journey.test.mjs` };
    await writeFile(join(output, "metadata.json"), JSON.stringify(metadata, null, 2) + "\n");
    mf = new Miniflare({ name: "benchmark", port: 0, modules: true, script: bundle.outputFiles[0].text,
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      durableObjects: { HANDS: { className: "AccountHostedTools", useSQLite: true }, NAMESPACE: { className: "Namespace", useSQLite: true } },
      handleRuntimeStdio(stdout, stderr) {
        createInterface({ input: stdout }).on("line", capture); createInterface({ input: stderr }).on("line", capture);
      } });
    native = await createNodeProcessTools({ workspace: join(output, "hand") });
    tools = await createTools({ attachmentId: "fixture-hand", machines: [{ id: "fixture-hand", name: "Synthetic Hand",
      workspace: join(output, "hand"), capabilities: ["shell", "cua"] }],
      tools: { ...Object.fromEntries(native.tools.map(tool => [tool.name, tool])),
        mcp__cua_repl__js: { description: "Synthetic external CUA service", parameters: { type: "object", additionalProperties: true },
          supportsParallelToolCalls: false, async handler(_input, context) {
            return (await fetch(externalURL, { signal: context.signal })).text();
          } },
        mcp__cua_repl__js_reset: { description: "Synthetic external CUA reset", parameters: { type: "object", additionalProperties: true },
          supportsParallelToolCalls: false, handler() { return "RESET_OK"; } },
      } });
    base = String(await mf.ready);
    attachment = tools.attach({ endpoint: new URL("/tool-host", base).href.replace(/^http/, "ws"), transport: { async connect() {
      const socket = new WebSocket(new URL("/tool-host", base).href.replace(/^http/, "ws"), { headers: { "x-nanocodex-owner-id": owner } });
      const record = (direction, data) => {
        const frame = JSON.parse(String(data));
        // Deliberately omit tool input/output and endpoint paths from transcript.
        wire.push({ direction, type: frame.type, call_id: frame.call_id, stage: frame.stage,
          timing: frame.timing, outcome: frame.outcome?.status });
      };
      socket.on("message", data => record("brain-to-hand", data));
      const send = socket.send.bind(socket);
      socket.send = (...args) => { record("hand-to-brain", args[0]); return send(...args); };
      return socket;
    } } });
    await within(attachment.connect(), "reverse attachment ready");
    await checkOwnership("before-restart");
    const denied = await request("/namespace", { name: "exec_command", call_id: "denied", input: {} }, "Bearer other");
    assert.equal(denied.status, 401);
    await invoke("prime", 0);
    for (let index = 0; index < 20; index++) await invoke("warm", index);
    const pending = invoke("cua", 0, "mcp__cua_repl__js", { workdir: "/fixture-hand", code: "synthetic pending CUA" });
    await within((async () => { while (!blocked.length) await sleep(10); })(), "CUA entered external service");
    for (let index = 0; index < 10; index++) {
      await invoke("cua-pending", index);
      assert.equal(blocked.length, 1, "shell must finish while CUA remains pending");
    }
    const burstStarted = performance.now();
    await Promise.all(Array.from({ length: 50 }, (_, index) => invoke("burst50", index)));
    const burstElapsed = performance.now() - burstStarted;
    assert.equal(blocked.length, 1);
    release(); await pending;
    assert.equal(externalCalls, 1);
    // Repeat one completed effect identity through the public namespace API.
    await invoke("replay", 0, "exec_command", undefined, "warm-0");
    await mf.unsafeEvictDurableObject("benchmark", "AccountHostedTools", { name: owner, webSockets: "hibernate" });
    await checkOwnership("after-restart");
    const effects = (await readFile(join(output, "hand/effects.log"), "utf8")).trim().split("\n");
    assert.equal(effects.length, 81, "no duplicate shell effects");
    assert.equal(new Set(effects).size, 81);
    assert.equal(wire.filter(row => row.type === "call").length, 82, "81 shell calls + one external CUA call; replay adds none");
    // Runtime stdout is asynchronous; wait for observability completeness only.
    await within((async () => {
      while (observations.filter(row => row.type === "hand.call.provider").length < journeys.length) await sleep(10);
    })(), "all provider summaries captured");
    const shells = journeys.filter(row => ["warm", "cua-pending", "burst50"].includes(row.group));
    const uniqueShells = [...new Map(shells.map(row => [row.call_id, row])).values()];
    const samples = uniqueShells.map(journey => {
      const correlated = observations.filter(row => row.source_call_id === journey.call_id || row.call_id === journey.call_id);
      const provider = correlated.find(row => row.type === "hand.call.provider");
      const account = correlated.find(row => row.type === "hand.call.account");
      const broker = correlated.find(row => row.type === "hand.call.broker" && row.stage === "receipt");
      assert.ok(provider && account && broker, JSON.stringify({ call_id: journey.call_id, correlated }));
      assert.equal(provider.thread_id, thread); assert.equal(account.thread_id, thread); assert.equal(broker.thread_id, thread);
      for (const phase of ["namespace.route", "namespace.invoke", "account.ownership", "account.resolve", "account.handler", "account.fetch", "account.decode"])
        assert.ok(correlated.some(row => row.type === "hand.tool.stage" && row.stage === phase), `${journey.call_id} missing ${phase}`);
      // Archived runs predate the input/ownership timing split. When the
      // account advertises the new duration, its correlated phase must exist.
      if (account.input_decode_ms !== undefined) {
        assert.ok(Number.isFinite(account.input_decode_ms));
        assert.ok(correlated.some(row => row.type === "hand.tool.stage" && row.stage === "account.decode_input"), `${journey.call_id} missing account.decode_input`);
      }
      for (const stage of ["received", "admitted", "dispatched", "send_started", "sent", "ack_attempt", "ack_sent", "receipt", "terminal"])
        assert.ok(correlated.some(row => row.type === "hand.call.broker" && row.stage === stage), `${journey.call_id} missing broker ${stage}`);
      const hostFrames = wire.filter(row => row.call_id === broker.transport_call_id);
      for (const stage of ["received", "execution_started", "execution_finished", "result_prepared"]) {
        assert.ok(hostFrames.some(row => row.type === "diagnostic" && row.stage === stage), `${journey.call_id} missing host ${stage}`);
        assert.ok(correlated.some(row => row.type === "hand.call.broker" && row.stage === "host_progress" && row.host_stage === stage), `${journey.call_id} missing correlated host ${stage}`);
      }
      assert.ok(broker.host_timing);
      if (repo === checkout || broker.frame_decode_ms !== undefined) {
        for (const row of correlated.filter(row => row.type === "hand.call.broker" && ["receipt", "host_progress"].includes(row.stage))) {
          for (const field of ["frame_decode_ms", "lease_validation_ms", "message_to_handler_ms"])
            assert.ok(Number.isFinite(row[field]) && row[field] >= 0, `${journey.call_id} missing ${field}`);
        }
        assert.ok(Number.isFinite(broker.dispatch_to_message_ms) && broker.dispatch_to_message_ms >= 0);
        assert.ok(broker.dispatch_to_message_ms + broker.message_to_handler_ms <= broker.roundtrip_ms + .001,
          "message entry and pre-handler time must fit within dispatch-to-result time");
      }
      assert.equal(broker.success, true);
      return { ...journey, provider_total_ms: provider.total_ms, provider_fetch_ms: provider.fetch_ms,
        provider_decode_ms: provider.decode_ms, account_total_ms: account.total_ms,
        ...(account.input_decode_ms === undefined ? {} : { account_input_decode_ms: account.input_decode_ms }),
        account_ownership_ms: account.ownership_ms, account_resolve_ms: account.resolve_ms, account_handler_ms: account.handler_ms,
        broker_admission_ms: broker.admission_ms, broker_roundtrip_ms: broker.roundtrip_ms,
        broker_settlement_ms: broker.settlement_ms,
        broker_dispatch_to_message_ms: broker.dispatch_to_message_ms,
        broker_frame_decode_ms: broker.frame_decode_ms,
        broker_lease_validation_ms: broker.lease_validation_ms,
        broker_message_to_handler_ms: broker.message_to_handler_ms,
        transit_return_overhead_ms: broker.transit_return_overhead_ms,
        ...Object.fromEntries(Object.entries(broker.host_timing).map(([key, value]) => [`hand_${key}`, value])),
        namespace_route_ms: correlated.find(row => row.stage === "namespace.route")?.duration_ms,
        namespace_invoke_ms: correlated.find(row => row.stage === "namespace.invoke")?.duration_ms,
      };
    });
    const groups = Object.fromEntries(["warm", "cua-pending", "burst50"].map(group => {
      const rows = samples.filter(row => row.group === group);
      const fields = Object.keys(rows[0]).filter(key => key.endsWith("_ms"));
      return [group, Object.fromEntries(fields.map(key => [key, distribution(rows.map(row => row[key]))]))];
    }));
    const burstRecords = observations.filter(row => row.type === "hand.call.broker" && row.source_call_id?.startsWith("burst50-"));
    const phaseCounts = Object.fromEntries([...new Set(burstRecords.map(row => row.stage))]
      .map(stage => [stage, burstRecords.filter(row => row.stage === stage).length]));
    const summary = { outcome: "passed", source_commit: sourceCommit, label, groups, burst_elapsed_ms: burstElapsed,
      discovery: metadata.discovery, ownership, burst_broker_phase_counts: phaseCounts,
      burst_runtime_generation_records: burstRecords.filter(row => Number.isFinite(row.runtime_generation)).length,
      assertions: { unique_shell_effects: effects.length, wire_calls: 82, external_cua_calls: 1, denied_status: 401,
        replay_no_duplicate_effects: true, shell_completes_with_cua_pending: true, preserved_correlated_phases: true,
        account_owner_mismatch_before_after_restart: true },
      evidence: output };
    await writeFile(join(output, "samples.json"), JSON.stringify(samples, null, 2) + "\n");
    await writeFile(join(output, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
    console.log(JSON.stringify({ outcome: summary.outcome, source_commit: sourceCommit, label,
      assertions: summary.assertions, evidence: output,
      provider_total_ms: Object.fromEntries(Object.entries(groups).map(([group, values]) => [group, values.provider_total_ms])),
      burst_elapsed_ms: burstElapsed }));
  } finally {
    release();
    try {
      if (attachment) await within(attachment.close(), "attachment close");
      await tools?.close(); await native?.close(); await mf?.dispose();
    } finally {
      external.closeAllConnections(); await new Promise(resolve => external.close(resolve));
      console.info = originalInfo;
      await writeFile(join(output, "observations.json"), JSON.stringify(observations, null, 2) + "\n");
      await writeFile(join(output, "websocket-phases.json"), JSON.stringify(wire, null, 2) + "\n");
      await writeFile(join(output, "journeys.json"), JSON.stringify(journeys, null, 2) + "\n");
      await writeFile(join(output, "worker-runtime.log"), workerLines.join("\n") + "\n");
    }
  }
});
