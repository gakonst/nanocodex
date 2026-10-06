import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Log, LogLevel, Miniflare } from "miniflare";

// Real SDK, Rust WASM, Durable Object SQLite, service-binding fetch and
// WebSockets. Only the external credential/provider service is synthetic.
// The small HTTP fixture exposes the public SDK turn/shutdown operations; it
// does not substitute for hosted account authentication or admission coverage.
const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/model-pause-journey", `${Date.now()}-${process.pid}`);
const sessionId = "00000000-0000-7000-8000-000000000001";
const turnId = "00000000-0000-7000-8000-000000000002";
const egressId = "22222222-2222-4222-8222-222222222222";
const source = `
import { DurableObject } from 'cloudflare:workers';
import * as Agent from '../nanocodex/cloudflare/Agent.mjs';
import wasm from './nanocodex.wasm';
import { performanceSocketEvent, performanceSocketTiming } from './src/performance.ts';
import { DiagnosticJournal } from './src/diagnostic-journal.ts';
import { transportObservation } from './src/transport-observation.ts';
import { watchManagedAgentFamilyEvents } from './src/agent-event-watcher.ts';
// Workerd's local stdout inspects console objects across several lines. Keep
// their exact emitted fields as JSON lines for this journey's transcript;
// deployed Workers receive the original structured console objects.
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);
export class FixtureAgent extends DurableObject {
  async fetch(request) {
    if (!this.agent) {
      this.journal = new DiagnosticJournal(this.ctx.storage, 'managed');
      this.agent = await Agent.create(wasm, this, {
        eventPersistence: 'caller', tools: { fixture: { description: 'Return a synthetic fixture result.',
          parameters: { type: 'object', properties: {}, additionalProperties: false },
          handler: () => 'TOOL_OK',
        } }, instructions: 'Synthetic diagnostics fixture.',
        [Symbol.for('nanocodex.cloudflare.internalConfiguration')]: {
          model: 'gpt-6.1-sol', thinking: 'low', reasoning_mode: 'standard', fast_mode: false,
        },
        [Symbol.for('nanocodex.cloudflare.internalRuntime')]: {
          waitForPreconnect: false, preserveRootTransport: true,
          onSocketTiming: event => performanceSocketTiming('${sessionId}', event),
          onSocketEvent: event => {
            performanceSocketEvent('${sessionId}', { ...event, body: 'synthetic-private-marker',
              close_reason: 'synthetic-private-marker', authorization: 'synthetic-private-marker' }, '${turnId}', record => this.journal.record(record));
            if (event.event === 'request.first_output') throw Error('synthetic-private-observer-error');
          },
        },
      });
      this.watcher = watchManagedAgentFamilyEvents(this.agent, {
        replay() {},
        observe(event) { console.info(JSON.stringify({ type: 'managed.agent.transport', session_id: '${sessionId}',
          ...transportObservation(event, '${turnId}') })); },
      });
    }
    if (request.method === 'GET') return Response.json(this.journal.page('${sessionId}', 0, 100));
    if (request.method === 'POST') {
      const turn = this.agent.turn.prompt({ input: await request.text() });
      const result = await turn.result();
      return Response.json({ final_message: result.finalMessage });
    }
    if (request.method === 'DELETE') {
      this.watcher.off(); await this.agent.session.shutdown(); return new Response(null, {status: 204});
    }
    return new Response(null, {status: 404});
  }
}
export default { fetch(request, env) { return env.AGENTS.getByName('synthetic-agent').fetch(request); } };
`;
const provider = `export default { fetch(request) {
  if(request.headers.get('upgrade') !== 'websocket') return new Response(null, {status: 400});
  const [client, server] = Object.values(new WebSocketPair()); server.accept();
  let index = 0;
  server.addEventListener('close', () => server.close(1000, 'fixture close acknowledged'));
  server.addEventListener('message', event => {
    const call = ++index, id = 'resp_synthetic_' + call;
    server.send(JSON.stringify({type:'response.created',response:{id,status:'in_progress'}}));
    server.send(JSON.stringify({type:'responsesapi.websocket_timing',response_id:id,timing_metrics:{
      pre_inference_ms:210,engine_queue_max_ms:120,engine_service_ttft_total_ms:80,
      authorization:'synthetic-private-marker',arbitrary_ms:999,
    }}));
    const finish = () => {
      server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:true,output:[{
        type:'message',role:'assistant',content:[{type:'output_text',text:'PAUSE_OK'}]
      }],usage:{input_tokens:2,output_tokens:1,total_tokens:3}}}));
    };
    if (call === 1) {
      server.send(JSON.stringify({type:'response.output_item.added',output_index:0,
        item:{type:'reasoning',id:'rs_synthetic',summary:[]}}));
      server.send(JSON.stringify({type:'response.reasoning_summary_text.delta',output_index:0,summary_index:0,delta:''}));
      server.send(JSON.stringify({type:'response.output_text.delta',output_index:1,delta:''}));
      setTimeout(() => {
        server.send(JSON.stringify({type:'response.reasoning_summary_text.delta',output_index:0,summary_index:0,delta:'Inspect fixture.'}));
      }, 1_600);
      setTimeout(() => {
        server.send(JSON.stringify({type:'response.output_text.delta',output_index:0,delta:'PAUSE_OK'})); finish();
      }, 3_200);
    } else if (call === 2) {
      const item = {type:'function_call',id:'fc_synthetic',name:'fixture',call_id:'call_synthetic',arguments:'{}'};
      server.send(JSON.stringify({type:'response.output_item.added',output_index:0,item:{...item,arguments:''}}));
      server.send(JSON.stringify({type:'response.function_call_arguments.delta',output_index:0,item_id:item.id,delta:''}));
      setTimeout(() => {
        for (const delta of ['{', '}']) server.send(JSON.stringify({
          type:'response.function_call_arguments.delta',output_index:0,item_id:item.id,delta,
        }));
        server.send(JSON.stringify({type:'response.output_item.done',output_index:0,item}));
        server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:false,output:[item],
          usage:{input_tokens:2,output_tokens:1,total_tokens:3}}}));
      }, 150);
    } else {
      if (!JSON.stringify(JSON.parse(event.data).input).includes('TOOL_OK')) throw Error('Missing fixture tool result');
      // More than a second of continuous output must not be called silence.
      const text = 'PAUSE_OK'; let offset = 0;
      const stream = setInterval(() => {
        server.send(JSON.stringify({type:'response.output_text.delta',output_index:0,delta:text[offset++]}));
        if (offset === text.length) { clearInterval(stream); finish(); }
      }, 250);
    }
  });
  return new Response(null,{status:101,webSocket:client,headers:{
    'x-request-id':'req_synthetic_provider','x-nanocodex-egress-request-id':'${egressId}',
    authorization:'synthetic-private-marker',
  }});
} };`;

test("a silent model request has live correlated diagnostics before completion and keeps its socket", { timeout: 70_000 }, async () => {
  await mkdir(output, { recursive: true });
  const records = [], raw = [];
  const capture = message => {
    raw.push(message);
    const start = message.indexOf('{"type":');
    if (start >= 0) {
      try { records.push(JSON.parse(message.slice(start))); } catch { /* Preserve original log below. */ }
    }
  };
  class TranscriptLog extends Log {
    logWithLevel(level, message) {
      capture(message);
      if (level <= LogLevel.ERROR) super.logWithLevel(level, message);
    }
  }
  const bundle = await build({
    stdin: { contents: source, resolveDir: root }, bundle: true, write: false, format: "esm", platform: "node",
    target: "es2022", conditions: ["workerd"], external: ["cloudflare:*", "node:*", "./nanocodex.wasm"],
    alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") }, logLevel: "warning",
  });
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat"] };
  const mf = new Miniflare({ log: new TranscriptLog(LogLevel.INFO), handleRuntimeStdio(stdout, stderr) {
    createInterface({ input: stdout }).on("line", capture);
    createInterface({ input: stderr }).on("line", capture);
  }, workers: [
    { ...common, name: "agent", modules: [
      { type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text },
      { type: "CompiledWasm", path: "nanocodex.wasm", contents: await readFile(join(root, "../nanocodex/pkg-web/nanocodex_bg.wasm")) },
    ], durableObjects: { AGENTS: { className: "FixtureAgent", useSQLite: true } }, serviceBindings: { NANOCODEX: "provider" } },
    { ...common, name: "provider", modules: true, script: provider },
  ] });
  let firstFinished = false;
  let firstFailure;
  const waitFor = async predicate => {
    const deadline = Date.now() + 40_000;
    while (!predicate()) {
      if (Date.now() >= deadline) throw Error("diagnostic did not arrive: " + JSON.stringify(records));
      await new Promise(resolve => setTimeout(resolve, 20));
    }
  };
  try {
    const first = mf.dispatchFetch("https://fixture.internal/turns", { method: "POST", body: "synthetic-private-prompt-marker" })
      .then(async response => {
        if (response.status !== 200) throw Error(`Fixture turn HTTP ${response.status}: ${await response.text()}`);
        firstFinished = true; return response.json();
      }).catch(error => { firstFailure = error; });
    await waitFor(() => firstFailure || records.some(x => x.stage === "transport.provider.timing"));
    if (firstFailure) throw firstFailure;
    assert.equal(firstFinished, false, "provider timing was withheld until completion");
    await waitFor(() => firstFailure || records.some(x => x.stage === "transport.request.waiting"));
    if (firstFailure) throw firstFailure;
    assert.equal(firstFinished, false, "observation ended the valid silent request");
    const waiting = records.find(x => x.stage === "transport.request.waiting");
    assert.equal(waiting.model_call_index, 1);
    assert.equal(waiting.egress_request_id, egressId);
    assert.equal(waiting.provider_request_id, "req_synthetic_provider");
    assert.equal(waiting.queued_message_count, 0);
    assert.ok(waiting.received_message_count >= 2);
    assert.ok(waiting.last_message_age_ms >= 900);
    assert.ok(waiting.elapsed_ms >= 900 && waiting.elapsed_ms < 2000);
    assert.ok(waiting.first_output_ms < 1000);
    assert.equal(waiting.first_reasoning_delta_ms, undefined, "empty reasoning delta counted as text");
    assert.equal(waiting.first_answer_delta_ms, undefined, "empty answer delta counted as text");
    assert.equal((await first).final_message, "PAUSE_OK");
    const second = await mf.dispatchFetch("https://fixture.internal/turns", { method: "POST", body: "synthetic follow-up" });
    assert.equal(second.status, 200); assert.equal((await second.json()).final_message, "PAUSE_OK");
    const completed = records.filter(x => x.type === "managed.agent.transport" && x.message_type === "model.call.completed");
    assert.equal(completed.length, 3);
    assert.ok(completed[0].time_to_first_event_ms < 2000);
    // The existing Rust output milestone includes the empty item declaration.
    assert.ok(completed[0].time_to_first_output_ms < 1000);
    const firstEvent = records.find(x => x.stage === "transport.request.first_message");
    assert.equal(firstEvent.provider_event_type, "response.created");
    const outputEvent = records.find(x => x.stage === "transport.request.first_output");
    assert.equal(outputEvent.provider_event_type, "response.output_item.added");
    assert.equal(outputEvent.output_kind, "item");
    const reasoningEvent = records.find(x => x.stage === "transport.request.first_reasoning_delta");
    const answerEvents = records.filter(x => x.stage === "transport.request.first_answer_delta");
    assert.equal(reasoningEvent.provider_event_type, "response.reasoning_summary_text.delta");
    assert.ok(reasoningEvent.elapsed_ms >= 1500 && reasoningEvent.elapsed_ms < 3000);
    assert.equal(answerEvents.length, 2, "answer milestone must occur once per request, including socket reuse");
    assert.ok(answerEvents[0].elapsed_ms >= 3000);
    assert.equal(answerEvents[0].provider_event_type, "response.output_text.delta");
    const toolEvents = records.filter(x => x.stage === "transport.request.first_tool_delta");
    assert.equal(toolEvents.length, 1, "tool milestone must occur only once for multiple argument deltas");
    assert.equal(toolEvents[0].provider_event_type, "response.function_call_arguments.delta");
    assert.equal(toolEvents[0].socket_request_index, 2);
    assert.ok(toolEvents[0].elapsed_ms >= 100, "empty tool delta counted as input");
    const toolFinished = records.find(x => x.stage === "transport.request.finished" && x.socket_request_index === 2);
    assert.equal(toolFinished.first_tool_delta_ms, toolEvents[0].elapsed_ms);
    assert.equal(toolFinished.first_answer_delta_ms, undefined);
    const finished = records.find(x => x.stage === "transport.request.finished");
    assert.equal(finished.first_reasoning_delta_ms, reasoningEvent.elapsed_ms);
    assert.equal(finished.first_answer_delta_ms, answerEvents[0].elapsed_ms);
    const persisted = await (await mf.dispatchFetch("https://fixture.internal/diagnostics")).json();
    assert.equal(persisted.write_failed, false);
    const savedAnswer = persisted.events.find(x => x.stage === "transport.request.first_answer_delta");
    assert.equal(savedAnswer.provider_event_type, answerEvents[0].provider_event_type);
    const savedTool = persisted.events.find(x => x.stage === "transport.request.first_tool_delta");
    assert.equal(savedTool.provider_event_type, toolEvents[0].provider_event_type);
    const savedFinished = persisted.events.find(x => x.stage === "transport.request.finished");
    assert.equal(savedFinished.first_reasoning_delta_ms, reasoningEvent.elapsed_ms);
    assert.equal(savedFinished.first_answer_delta_ms, answerEvents[0].elapsed_ms);
    assert.equal(completed[0].model_call_index, 1);
    assert.equal(records.filter(x => x.stage === "transport.socket.opened").length, 1);
    const timings = records.filter(x => x.stage === "transport.provider.timing");
    assert.equal(timings.length, 3);
    assert.deepEqual(timings.map(x => x.socket_request_index), [1, 2, 3]);
    assert.deepEqual(timings.map(x => x.response_id), ["resp_synthetic_1", "resp_synthetic_2", "resp_synthetic_3"]);
    assert.equal(records.filter(x => x.stage === 'transport.request.waiting' && x.socket_request_index >= 2).length, 0);
    assert.equal((await mf.dispatchFetch("https://fixture.internal/turns", { method: "DELETE" })).status, 204);
    assert.equal(records.filter(x => x.stage === "transport.socket.closed").length, 1);
    assert.doesNotMatch(JSON.stringify(records), /synthetic-private|arbitrary_ms|authorization|close_reason|body/);
    console.log(JSON.stringify({ evidence: output, observed: { first_event_ms: completed[0].time_to_first_event_ms,
      first_output_ms: completed[0].time_to_first_output_ms, reasoning_delta_ms: reasoningEvent.elapsed_ms,
      answer_delta_ms: answerEvents[0].elapsed_ms, tool_delta_ms: toolEvents[0].elapsed_ms, waiting, reused_socket: true, exact_answers: true } }));
  } finally {
    await mf.dispose();
    await writeFile(join(output, "records.json"), JSON.stringify(records, null, 2) + "\n");
    await writeFile(join(output, "runtime.log"), raw.join("\n") + "\n");
  }
});
