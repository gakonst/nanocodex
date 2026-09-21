import { test } from 'node:test';
import assert from 'node:assert/strict';
import { summarizeProviderObservations, providerObservationKey } from '../src/provider-telemetry.ts';
import { runProviderProbes, PROVIDER_PROBE_PROMPT_VERSION, PROVIDER_PROBE_TTFT_DEFINITION } from '../src/provider-probes.ts';
const sample = (timestamp, fullResponseMs, outcome = 'success') => ({ timestamp, source:'live',workerColo:'LHR',clientIngressColo:'ATH',backend:'openrouter',model:'test/model',effort:null,outcome,status:200,headersMs:1,fullResponseMs,generationTtftMs:null,clientDeliveryMs:null,elapsedMs:fullResponseMs??50 });
test('censors failures, excludes stale/future data and requires sufficient successes', () => {
 const result=summarizeProviderObservations([sample(1,999),sample(99,10),sample(100,30),sample(100,null,'timeout'),sample(200,1)],100,{windowMs:10,minimumSamples:3,alpha:.5});
 assert.equal(result.sampleCount,3); assert.equal(result.censoredCount,1); assert.equal(result.fullResponseP50Ms,20); assert.equal(result.fullResponseEwmaMs,20); assert.equal(result.usable,false);
 assert.notEqual(providerObservationKey(sample(100,10)),providerObservationKey({...sample(100,10),workerColo:'SFO'}));
});
test('SQLite store atomically caps durable budget, bounds retained data and projects fields',async()=>{
 const { DatabaseSync }=await import('node:sqlite');
 const { SqliteProviderTelemetryStore }=await import('../src/provider-telemetry.ts');
 const db=new DatabaseSync(':memory:');
 const sql={exec(query,...bindings){const stmt=db.prepare(query);return stmt.columns().length?stmt.all(...bindings):(stmt.run(...bindings),[]);}};
 const store=new SqliteProviderTelemetryStore(sql);
 assert.equal(store.reserveProbe('2026-09-20',2),true);assert.equal(store.reserveProbe('2026-09-20',2),true);assert.equal(new SqliteProviderTelemetryStore(sql).reserveProbe('2026-09-20',2),false);
 assert.equal(store.reserveProbe('2026-09-21',2),true);
 for(let i=0;i<515;i++)store.append({...sample(i,10),prompt:'private',key:'secret'});
 const rows=store.read();assert.equal(rows.length,512);assert.equal(rows[0].timestamp,3);assert.doesNotMatch(JSON.stringify(rows),/private|secret/);db.close();
});
test('live observer measures headers/body separately using monotonic time and persists once',async()=>{
 const {beginLiveProviderObservation}=await import('../src/provider-telemetry.ts');
 let mono=10;const samples=[];
 const observer=beginLiveProviderObservation(sample(1,2),{append:x=>{samples.push(x);}},{wallNow:()=>1000,monotonicNow:()=>mono});
 mono=20;observer.headers(200);mono=40;assert.equal(await observer.finish('success'),true);
 assert.equal(await observer.finish('success'),false);
 assert.equal(samples.length,1);assert.equal(samples[0].headersMs,10);assert.equal(samples[0].fullResponseMs,30);assert.equal(samples[0].timestamp,1000);assert.equal(samples[0].generationTtftMs,null);assert.equal(samples[0].clientDeliveryMs,null);
 const failed=beginLiveProviderObservation(sample(1,2),{append(){throw Error('storage unavailable');}});
 failed.headers(200);assert.equal(await failed.finish('success'),false);
});


const encoder = new TextEncoder();
const target = (overrides = {}) => ({ backend: 'openrouter', model: 'test/model', effort: 'high', key: 'synthetic-secret', ...overrides });
const delta = (value, finish_reason = null) => `data: ${JSON.stringify({ choices: [{ index: 0, delta: value, finish_reason }] })}\n\n`;
const stop = delta({}, 'stop');
const done = 'data: [DONE]\n\n';
const complete = delta({ content: 'OK' }) + stop + done;
const sse = text => new Response(text, { headers: { 'content-type': 'text/event-stream; charset=utf-8' } });
async function probe(overrides = {}) {
  const observations = [];
  const options = { enabled: true, dailyRequestLimit: 100, workerColo: null, targets: [target()],
    store: { reserveProbe: () => true, append: x => { observations.push(x); } },
    fetch: async () => sse(complete), ...overrides };
  const attempted = await runProviderProbes(options);
  return { attempted, observations };
}
function chunks(parts, onRead = () => {}, onCancel = () => {}) {
  let index = 0;
  return new ReadableStream({
    pull(controller) {
      if (index === parts.length) return controller.close();
      onRead(index);
      const part = parts[index++];
      controller.enqueue(typeof part === 'string' ? encoder.encode(part) : part);
    }, cancel: onCancel,
  }, { highWaterMark: 0 });
}

test('disabled probes and invalid budgets cannot reserve or dispatch', async () => {
  for (const options of [{ enabled: false }, { enabled: 'true' }, ...[0, 4097, NaN, 1.5].map(dailyRequestLimit => ({ dailyRequestLimit })),
    ...[0, 46, NaN, 1.5].map(maxTargetsPerRun => ({ maxTargetsPerRun })),
    ...[-1, Infinity, 1.5].map(startIndex => ({ startIndex })),
    ...[0, 15, 2049, NaN].map(maxCompletionTokens => ({ maxCompletionTokens }))]) {
    const result = await probe({ ...options, store: { reserveProbe() { assert.fail('reserved'); }, append() { assert.fail('appended'); } },
      fetch() { assert.fail('dispatched'); } });
    assert.equal(result.attempted, 0);
  }
});

test('only fixed backends with valid target configuration can issue probes', async () => {
  for (const invalid of [{ backend: 'https://evil.example' }, { model: '../bad?model=secret' }, { model: null },
    { key: undefined }, { key: ' ' }, { key: 'bad\nheader' }, { effort: undefined }, { effort: 'HIGH' },
    { backend: 'workers_ai', key: undefined }]) {
    const result = await probe({ targets: [target(invalid)],
      store: { reserveProbe() { assert.fail('reserved'); }, append() { assert.fail('appended'); } }, fetch() { assert.fail('dispatched'); } });
    assert.equal(result.attempted, 0);
  }
});

test('daily reservation precedes requests, stops at the cap, and stores only measurements', async () => {
  let reservations = 0, calls = 0; const observations = [];
  const attempted = await runProviderProbes({ enabled: true, dailyRequestLimit: 1, workerColo: 'LHR',
    targets: [target(), target({ backend: 'vercel' })],
    store: { reserveProbe(day, limit) { assert.match(day, /^\d{4}-\d{2}-\d{2}$/); assert.equal(limit, 1); return ++reservations <= 1; },
      append(x) { observations.push(x); } },
    fetch: async (url, init) => { calls++; assert.equal(reservations, 1); assert.equal(url, 'https://openrouter.ai/api/v1/chat/completions');
      assert.equal(init.redirect, 'manual'); return sse(delta({ reasoning: 'synthetic-private-reasoning' }) + complete); } });
  assert.equal(attempted, 1); assert.equal(calls, 1);
  const row = observations[0];
  assert.equal(row.outcome, 'success'); assert.equal(row.effort, 'high'); assert.equal(row.workerColo, 'LHR');
  assert.equal(row.clientIngressColo, null); assert.equal(row.clientDeliveryMs, null);
  assert.ok(row.generationTtftMs >= row.headersMs); assert.ok(row.fullResponseMs >= row.generationTtftMs);
  assert.doesNotMatch(JSON.stringify(observations), /synthetic-secret|synthetic-private|Reply|nonce|ttft-v1/);
});

test('TTFT measures first generated reasoning event, excluding headers, roles and heartbeats', async () => {
  let clock = 0, cancelled = false;
  const frames = [delta({ role: 'assistant', content: '' }), ': keepalive\n\n', 'event: ping\n\n',
    delta({ reasoning_details: [{ type: 'reasoning.encrypted', data: 'opaque' }] }),
    delta({ reasoning_content: 'thinking' }), delta({ content: 'OK' }), stop, done];
  const result = await probe({ now: () => 123456, monotonicNow: () => clock,
    fetch: async () => { clock = 5; return sse(chunks(frames, i => { clock = 10 + i * 10; }, () => { cancelled = true; })); } });
  const row = result.observations[0];
  assert.equal(row.timestamp, 123456); assert.equal(row.headersMs, 5); assert.equal(row.generationTtftMs, 50);
  assert.equal(row.fullResponseMs, 80); assert.equal(row.outcome, 'success'); assert.equal(cancelled, true);
  assert.equal(PROVIDER_PROBE_TTFT_DEFINITION, 'first_nonempty_text_or_reasoning_delta');
});

test('SSE handles one-byte chunks, split UTF-8, multiline data, CRLF and CR framing', async () => {
  const wire = '\ufeff: hello\r\n\r\n' + 'data: {"choices":\r\ndata: [{"index":0,"delta":{"content":"✓"},"finish_reason":null}]}\r\n\r\n'
    + stop.replaceAll('\n', '\r') + done.replaceAll('\n', '\r\n');
  const result = await probe({ fetch: async () => sse(chunks([...encoder.encode(wire)].map(x => new Uint8Array([x])))) });
  assert.equal(result.observations[0].outcome, 'success'); assert.notEqual(result.observations[0].generationTtftMs, null);
});

test('plaintext reasoning variants qualify for TTFT, including reasoning-only completed streams', async () => {
  for (const generated of [{ reasoning: 'reason' }, { reasoning_content: 'reason' },
    { reasoning_details: [{ type: 'reasoning.text', text: 'reason' }] },
    { reasoning_details: [{ type: 'reasoning.summary', summary: 'reason' }] }, { content: ' ' }]) {
    const result = await probe({ fetch: async () => sse(delta(generated) + stop + done) });
    assert.equal(result.observations[0].outcome, 'success'); assert.notEqual(result.observations[0].generationTtftMs, null);
  }
});

test('content-free usage frames and repeated stop metadata are valid before DONE', async () => {
  for (const usage of [{ choices: [], usage: { completion_tokens: 2 } },
    { choices: [{ index: 0, delta: { role: 'assistant', content: '' }, finish_reason: 'stop' }], usage: { completion_tokens: 2 } }]) {
    const result = await probe({ fetch: async () => sse(delta({ content: 'OK' }) + stop + `data: ${JSON.stringify(usage)}\n\n` + done) });
    assert.equal(result.observations[0].outcome, 'success');
  }
});

test('partial, malformed, empty and provider-error streams never become successful TTFT samples', async () => {
  const cases = [
    '', 'OK', done, delta({ content: 'OK' }), delta({ content: 'OK' }) + stop,
    delta({ content: 'OK' }) + done, delta({ content: 'OK' }) + stop + 'data: [DONE]',
    delta({ role: 'assistant', content: '' }) + stop + done,
    delta({ reasoning_details: [{ type: 'reasoning.encrypted', data: 'opaque' }] }) + stop + done,
    delta({ content: 'OK' }) + delta({}, 'content_filter') + done,
    delta({ content: 'OK' }) + delta({}, 'error') + done,
    delta({ content: 'OK' }) + delta({}, 'tool_calls') + done,
    delta({ content: 'OK' }) + 'data: {"error":{"message":"synthetic-private-error"}}\n\n' + stop + done,
    delta({ content: 'OK' }) + 'event: error\n\n' + stop + done,
    delta({ content: 'OK' }) + 'event: error\ndata: {"message":"synthetic-private-error"}\n\n' + stop + done,
    delta({ content: 'OK' }) + 'data: {"choices":[],"usage":{},"success":false}\n\n' + stop + done,
    delta({ content: 'OK' }) + 'data: {"choices":[],"usage":{},"errors":["synthetic-private-error"]}\n\n' + stop + done,
    'data: not-json\n\n' + complete,
    'data: []\n\n' + complete,
    'data: {"choices":{}}\n\n' + complete,
    'data: {"choices":[]}\n\n' + complete,
    'data: {"choices":[{"index":0,"delta":null}]}\n\n' + complete,
    'data: {"choices":[{"index":1,"delta":{"content":"bad"}}]}\n\n' + complete,
    delta({ content: 123 }) + complete,
    delta({ role: 'user' }) + complete,
    delta({ content: 'OK' }) + stop + delta({}, 'length') + done,
    delta({ reasoning_content: {} }) + complete,
    delta({ reasoning_details: [{ type: 'bad' }] }) + complete,
    delta({ tool_calls: [{ function: { name: 'bad' } }] }) + complete,
    delta({ content: 'OK' }) + stop + delta({ content: 'late text' }) + done,
    complete + 'data: malformed\n\n', complete + 'data: partial',
    'event: ping\ndata: {"error":"synthetic-private-error"}\n\n' + complete,
  ];
  for (const wire of cases) {
    const result = await probe({ fetch: async () => sse(wire) });
    assert.equal(result.observations[0].outcome, 'protocol_error', `unexpected success for fixture ${cases.indexOf(wire)}`);
    assert.equal(result.observations[0].generationTtftMs, null); assert.equal(result.observations[0].fullResponseMs, null);
    assert.doesNotMatch(JSON.stringify(result.observations), /synthetic-private-error/);
  }
});

test('invalid UTF-8, including incomplete bytes after DONE, is censored', async () => {
  for (const wire of [new Uint8Array([0xff]), new Uint8Array([...encoder.encode(complete), 0xc3])]) {
    const result = await probe({ fetch: async () => sse(chunks([wire])) });
    assert.equal(result.observations[0].outcome, 'protocol_error');
  }
});

test('all configured model and effort targets can rotate across bounded ticks', async () => {
  const targets = Array.from({ length: 11 }, (_, i) => target({ model: `test/model${i}`, effort: ['low', 'medium', 'high'][i % 3] }));
  const seen = [];
  let cursor = 0;
  for (let tick = 0; tick < 4; tick++) {
    const result = await probe({ targets, maxTargetsPerRun: 3, startIndex: cursor,
      fetch: async (_, init) => { const body = JSON.parse(init.body); seen.push([body.model, body.reasoning.effort]); return sse(complete); } });
    assert.equal(result.attempted, 3); cursor += result.attempted;
  }
  assert.deepEqual(seen.slice(0, 11), targets.map(x => [x.model, x.effort])); assert.deepEqual(seen[11], seen[0]);
  assert.equal((await probe({ targets })).attempted, 8);
  assert.equal((await probe({ targets, maxTargetsPerRun: 32 })).attempted, 11);
});

test('requests preserve exact model and effort with bounded completion budgets and unique versioned prompts', async () => {
  const prompts = [];
  for (const backend of ['openrouter', 'vercel', 'workers_ai']) {
    for (const effort of ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', null]) {
      const model = backend === 'workers_ai' ? '@cf/zai-org/glm-4.7-flash' : 'test/exact-model';
      const inspect = (body, signal) => {
        assert.equal(body.stream, true); assert.equal(signal.aborted, false); assert.equal(body.messages.length, 1);
        assert.match(body.messages[0].content, new RegExp(`^[a-f0-9-]{36} ${PROVIDER_PROBE_PROMPT_VERSION}`));
        prompts.push(body.messages[0].content);
        assert.equal(body.max_tokens ?? body.max_completion_tokens, 128);
        assert.equal(backend === 'openrouter' ? body.reasoning?.effort : body.reasoning_effort, effort ?? undefined);
      };
      const result = await probe({ targets: [target({ backend, model, effort, ...(backend === 'workers_ai' ? { key: undefined } : {}) })],
        ai: { async run(actualModel, input, options) { assert.equal(actualModel, model); inspect(input, options.signal); return chunks([complete]); } },
        fetch: async (url, init) => { assert.equal(url, backend === 'openrouter' ? 'https://openrouter.ai/api/v1/chat/completions' : 'https://ai-gateway.vercel.sh/v1/chat/completions');
          assert.equal(init.redirect, 'manual'); const body = JSON.parse(init.body); assert.equal(body.model, model); inspect(body, init.signal);
          if (backend === 'openrouter') assert.equal(body.provider.require_parameters, true);
          return sse(complete); } });
      assert.equal(result.observations[0].effort, effort); assert.equal(result.observations[0].model, model);
      assert.equal(result.observations[0].outcome, 'success');
      if (backend === 'workers_ai') { assert.equal(result.observations[0].headersMs, null); assert.equal(result.observations[0].status, null); }
    }
  }
  assert.equal(new Set(prompts).size, prompts.length);
  await probe({ maxCompletionTokens: 512, fetch: async (_, init) => { assert.equal(JSON.parse(init.body).max_tokens, 512); return sse(complete); } });
});

test('HTTP errors and manual redirects cancel unread bodies and never follow Location', async () => {
  for (const status of [301, 302, 307, 308, 401, 429, 500]) {
    let cancelled = false, calls = 0;
    const result = await probe({ fetch: async (_, init) => { calls++; assert.equal(init.redirect, 'manual');
      return new Response(new ReadableStream({ cancel() { cancelled = true; return new Promise(() => {}); } }),
        { status, headers: { location: 'https://evil.example' } }); } });
    assert.equal(calls, 1); assert.equal(cancelled, true); assert.equal(result.observations[0].status, status);
    assert.equal(result.observations[0].outcome, 'http_error'); assert.equal(result.observations[0].generationTtftMs, null);
  }
});

test('non-stream responses and wrong media types cannot supply streaming TTFT', async () => {
  for (const response of [new Response(complete), new Response(null, { headers: { 'content-type': 'text/event-stream' } }),
    Response.json({ choices: [{ message: { content: 'OK' }, finish_reason: 'stop' }] })]) {
    const result = await probe({ fetch: async () => response });
    assert.equal(result.observations[0].outcome, 'protocol_error');
  }
  const result = await probe({ targets: [target({ backend: 'workers_ai' })], ai: { async run() { return { response: 'buffered' }; } } });
  assert.equal(result.observations[0].outcome, 'protocol_error');
});

test('byte limit includes heartbeats and cancels oversized streams without awaiting cancellation', { timeout: 2000 }, async () => {
  let cancelled = false;
  const result = await probe({ fetch: async () => sse(chunks([':' + 'x'.repeat(65_536)], () => {}, () => {
    cancelled = true; return new Promise(() => {});
  })) });
  assert.equal(cancelled, true); assert.equal(result.observations[0].outcome, 'protocol_error');
  assert.equal(result.observations[0].generationTtftMs, null);
});

test('deadline bounds fetch, binding and stalled streams even when abort/cancel is ignored', { timeout: 2000 }, async () => {
  for (const kind of ['fetch', 'binding', 'body', 'body-after-token']) {
    let cancelled = false, signal;
    const result = await probe({ timeoutMs: 5,
      targets: [target({ backend: kind === 'binding' ? 'workers_ai' : 'vercel' })],
      ai: { run(_, __, options) { signal = options.signal; return new Promise(() => {}); } },
      fetch: async (_, init) => {
        signal = init.signal;
        if (kind === 'fetch') return new Promise(() => {});
        return sse(new ReadableStream({ start(c) { if (kind === 'body-after-token') c.enqueue(encoder.encode(delta({ content: 'OK' }))); },
          cancel() { cancelled = true; return new Promise(() => {}); } }));
      } });
    assert.equal(result.observations[0].outcome, 'timeout'); assert.equal(result.observations[0].generationTtftMs, null);
    assert.equal(result.observations[0].fullResponseMs, null); assert.equal(signal.aborted, true);
    if (kind.startsWith('body')) assert.equal(cancelled, true);
  }
});

test('late fetch and binding streams are cancelled after timeout', { timeout: 2000 }, async () => {
  for (const backend of ['vercel', 'workers_ai']) {
    let resolve, cancelled = false;
    const pending = new Promise(r => { resolve = r; });
    const result = await probe({ timeoutMs: 5, targets: [target({ backend })], fetch: () => pending, ai: { run: () => pending } });
    assert.equal(result.observations[0].outcome, 'timeout');
    const stream = new ReadableStream({ cancel() { cancelled = true; } });
    resolve(backend === 'workers_ai' ? stream : sse(stream));
    await new Promise(r => setImmediate(r));
    assert.equal(cancelled, true); assert.equal(result.observations.length, 1);
  }
});

test('network/read errors are censored without persisting exception messages', async () => {
  for (const mode of ['fetch', 'reader']) {
    const result = await probe({ fetch: async () => {
      if (mode === 'fetch') throw Error('synthetic-private-error');
      return sse(new ReadableStream({ pull() { throw Error('synthetic-private-error'); } }));
    } });
    assert.equal(result.observations[0].outcome, 'network_error'); assert.equal(result.observations[0].fullResponseMs, null);
    assert.doesNotMatch(JSON.stringify(result.observations), /synthetic-private-error/);
  }
});

test('budget failures fail closed and telemetry failures cannot trigger retries', async () => {
  assert.equal((await probe({ store: { reserveProbe() { throw Error('unavailable'); }, append() { assert.fail('append'); } },
    fetch() { assert.fail('fetch'); } })).attempted, 0);
  let calls = 0;
  assert.equal((await probe({ store: { reserveProbe: () => true, append() { throw Error('unavailable'); } },
    fetch: async () => { calls++; return sse(complete); } })).attempted, 1);
  assert.equal(calls, 1);
});

test('valid length terminals measure TTFT only when generated text arrived', async () => {
  const result = await probe({ fetch: async () => sse(delta({ reasoning: 'thinking' }) + delta({}, 'length') + done) });
  assert.equal(result.observations[0].outcome, 'success'); assert.notEqual(result.observations[0].generationTtftMs, null);
  const empty = await probe({ fetch: async () => sse(delta({}, 'length') + done) });
  assert.equal(empty.observations[0].outcome, 'protocol_error');
});

test('a configured full catalog sweep is bounded at 45 and accepts a recurring daily budget', async () => {
  const targets = Array.from({ length: 50 }, (_, i) => target({ model: 'test/model' + i }));
  const result = await probe({ targets, maxTargetsPerRun: 45, dailyRequestLimit: 2048 });
  assert.equal(result.attempted, 45);
  assert.equal(result.observations.length, 45);
});

test('Workers AI accepts its empty usage trailer only after terminal generation', async () => {
  const trailer='data: '+JSON.stringify({response:'',usage:{completion_tokens:2}})+'\n\n';
  const valid=delta({content:'OK'})+stop+trailer+done;
  const result=await probe({targets:[target({backend:'workers_ai',key:undefined})],ai:{run:async()=>chunks([valid])}});
  assert.equal(result.observations[0].outcome,'success');
  assert.notEqual(result.observations[0].generationTtftMs,null);
  for(const malformed of [trailer+complete,delta({content:'OK'})+stop+trailer.replace('"response":""','"response":"unexpected"')+done]) {
    const rejected=await probe({targets:[target({backend:'workers_ai',key:undefined})],ai:{run:async()=>chunks([malformed])}});
    assert.equal(rejected.observations[0].outcome,'protocol_error');
  }
  const gateway=await probe({fetch:async()=>sse(valid)});
  assert.equal(gateway.observations[0].outcome,'protocol_error');
});

const responseEvent = (type, fields = {}) => `event: ${type}\ndata: ${JSON.stringify({ type, ...fields })}\n\n`;
const responseText = responseEvent('response.output_text.delta', { delta: 'OK' });
const responseComplete = responseEvent('response.completed', { response: { status: 'completed', error: null } });
const frontierProbe = (parts, overrides = {}) => probe({ targets: [target({ backend: 'cloudflare', model: 'openai/gpt-6-astra', key: undefined })],
  ai: { run: async () => chunks(parts) }, ...overrides });

test('Cloudflare frontier uses Responses binding payload and fresh synthetic input at every effort', async () => {
  const inputs = [];
  for (const effort of ['low', 'medium', 'high']) {
    const result = await frontierProbe([], { targets: [target({backend:'cloudflare',model:'openai/gpt-6-astra',effort,key:undefined})],
      fetch() { assert.fail('HTTP credentials must not be needed'); }, ai: { async run(model, body, options) {
        assert.equal(model, 'openai/gpt-6-astra');
        assert.deepEqual(body, {input:body.input,stream:true,max_output_tokens:128,reasoning:{effort}});
        assert.match(body.input, /^[0-9a-f-]{36} ttft-v1\. Reply with only OK\.$/);
        inputs.push(body.input);
        assert.equal(options.signal.aborted, false);
        return chunks([responseText, responseComplete]);
      } } });
    assert.equal(result.observations[0].outcome, 'success');
    assert.equal(result.observations[0].headersMs, null); assert.equal(result.observations[0].status, null);
    assert.doesNotMatch(JSON.stringify(result.observations), /Reply|synthetic-secret/);
  }
  assert.equal(new Set(inputs).size, 3);
});

test('native Responses TTFT excludes created, metadata, empty deltas and done snapshots', async () => {
  let clock = 0, cancelled = false;
  const metadata = [responseEvent('response.created', {response:{status:'in_progress'}}),
    responseEvent('response.output_item.added', {item:{type:'reasoning',encrypted_content:'opaque'}}),
    responseEvent('response.function_call_arguments.delta', {delta:'tool metadata'}),
    responseEvent('response.output_text.done', {text:'snapshot is not a delta'}),
    responseEvent('response.output_text.delta', {delta:''})];
  const frames = [...metadata, responseEvent('response.reasoning_summary_text.delta',{delta:'thinking'}), responseText, responseComplete];
  const result = await frontierProbe([], {monotonicNow:()=>clock,
    ai:{run:async()=>chunks(frames, i=>{clock=(i+1)*10;},()=>{cancelled=true;return new Promise(()=>{});})}});
  assert.equal(result.observations[0].outcome,'success');
  assert.equal(result.observations[0].generationTtftMs,60); assert.equal(result.observations[0].fullResponseMs,80);
  assert.equal(cancelled,true);
  const empty = await frontierProbe([...metadata,responseComplete]);
  assert.equal(empty.observations[0].outcome,'protocol_error'); assert.equal(empty.observations[0].generationTtftMs,null);
});

test('Responses handles partial UTF-8 chunks and multiline CRLF/CR frames without requiring DONE', async () => {
  const wire = '\ufeff: heartbeat\r\n\r\n' + 'event: response.reasoning_text.delta\r\ndata: {"type":"response.reasoning_text.delta",\r\ndata: "delta":"✓"}\r\n\r\n' + responseComplete.replaceAll('\n','\r');
  const result = await frontierProbe([...encoder.encode(wire)].map(x=>new Uint8Array([x])));
  assert.equal(result.observations[0].outcome,'success');
  assert.notEqual(result.observations[0].generationTtftMs,null);
  assert.equal((await frontierProbe([responseText+responseComplete+done])).observations[0].outcome,'success');
});

test('Responses requires valid terminals and censors provider errors, cancellation and partial reads', async () => {
  const invalid = [responseText, responseText+responseComplete.trimEnd(), responseComplete,
    responseText+responseEvent('response.completed',{response:{status:'in_progress'}}),
    responseText+responseEvent('response.failed',{response:{status:'failed',error:{message:'synthetic-private-error'}}}),
    responseText+responseEvent('response.incomplete',{response:{status:'incomplete',incomplete_details:{reason:'content_filter'}}}),
    responseText+responseEvent('error',{code:'upstream_error',message:'synthetic-private-error'}),
    responseText+responseComplete+responseText,
    'event: response.created\ndata: {"type":"response.output_text.delta","delta":"bad"}\n\n'+responseComplete,
    responseEvent('response.output_text.delta',{delta:{text:'invalid'}})+responseComplete,
    complete];
  for (const wire of invalid) {
    const result = await frontierProbe([wire]);
    assert.equal(result.observations[0].outcome,'protocol_error',wire);
    assert.equal(result.observations[0].generationTtftMs,null); assert.equal(result.observations[0].fullResponseMs,null);
    assert.doesNotMatch(JSON.stringify(result.observations),/synthetic-private-error/);
  }
  const cancelled = await frontierProbe([responseText,responseEvent('response.cancelled',{response:{status:'cancelled'}})]);
  assert.equal(cancelled.observations[0].outcome,'cancelled'); assert.equal(cancelled.observations[0].generationTtftMs,null);
  const limited = await frontierProbe([responseText,responseEvent('response.incomplete',{response:{status:'incomplete',incomplete_details:{reason:'max_output_tokens'}}})]);
  assert.equal(limited.observations[0].outcome,'success');
});

test('Cloudflare deadline bounds binding, partial stream and cancellation and cancels late arrivals', {timeout:2000}, async () => {
  for (const kind of ['binding','partial']) {
    let signal, resolve, cancelled = false;
    const result = await frontierProbe([], {timeoutMs:5, ai:{run:(_,__,options)=>{
      signal=options.signal;
      if(kind==='binding') return new Promise(r=>{resolve=r;});
      return Promise.resolve(new ReadableStream({start(c){c.enqueue(encoder.encode(responseText));},cancel(){cancelled=true;return new Promise(()=>{});}}));
    }}});
    assert.equal(result.observations[0].outcome,'timeout'); assert.equal(result.observations[0].generationTtftMs,null);
    assert.equal(signal.aborted,true);
    if(kind==='binding') { resolve(new ReadableStream({cancel(){cancelled=true;}})); await new Promise(r=>setImmediate(r)); }
    assert.equal(cancelled,true);
  }
});

test('live binding observations accept protocol success without an invented status',async()=>{
  const {beginLiveProviderObservation}=await import('../src/provider-telemetry.ts');
  for(const backend of ['cloudflare','workers_ai','openrouter']) {
    const rows=[];
    const observer=beginLiveProviderObservation({...sample(1,2),backend},{append:x=>rows.push(x)});
    await observer.finish('success');
    assert.equal(rows[0].outcome,backend==='openrouter'?'http_error':'success');
    assert.equal(rows[0].status,null);assert.equal(rows[0].headersMs,null);assert.equal(rows[0].generationTtftMs,null);
  }
});
