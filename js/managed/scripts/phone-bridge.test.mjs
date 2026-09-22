import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHmac, randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';
import { WebSocket } from 'ws';
import { createPhoneBridge } from './phone-bridge.mjs';

const agent = '11111111-1111-7111-8111-111111111111';
const otherAgent = '22222222-2222-7222-8222-222222222222';
const sid = 'CA' + '1'.repeat(32), stream = 'MZ' + '2'.repeat(32);
const env = { NANOCODEX_PHONE_PUBLIC_ORIGIN: 'https://phone.example', NANOCODEX_PHONE_BRIDGE_TOKEN: 't'.repeat(40),
  NANOCODEX_PHONE_VOICE_BINARY: '/fake/phone-voice', TWILIO_ACCOUNT_SID: 'AC' + '3'.repeat(32),
  TWILIO_AUTH_TOKEN: 'test-auth-token', TWILIO_VOICE_FROM_NUMBER: '+14155550100' };
const signature = (path, fields = new URLSearchParams()) => {
  let value = env.NANOCODEX_PHONE_PUBLIC_ORIGIN + path;
  for (const key of [...new Set(fields.keys())].sort()) for (const item of [...new Set(fields.getAll(key))].sort()) value += key + item;
  return createHmac('sha1', env.TWILIO_AUTH_TOKEN).update(value).digest('base64');
};
async function setup(t, overrides = {}) {
  let creates = 0, hangups = 0, voiceEvent;
  const input = [];
  const provider = { async create(_env, value) { creates++; return { sid, status: 'queued' }; },
    async status() { return { sid, status: 'in-progress' }; }, async hangup() { hangups++; return { sid, status: 'completed' }; }, ...overrides.provider };
  const bridge = createPhoneBridge({ startDelegation: overrides.startDelegation ?? (() => ({ async prepare() {}, async run() { return 'Test lookup result'; }, steer() {}, async close() {} })), stopDelegate: overrides.stopDelegate ?? (async () => {}), env: { ...env, ...overrides.env }, database: overrides.database ?? ':memory:', provider,
    startVoice: overrides.startVoice ?? ((_binary, _instructions, event) => { voiceEvent = event; return { ready: Promise.resolve(), send(event) { input.push(event); }, close() {} }; }) });
  bridge.server.listen(0, '127.0.0.1'); await once(bridge.server, 'listening');
  const origin = `http://127.0.0.1:${bridge.server.address().port}`;
  let closed = false;
  const close = async () => { if (!closed) { closed = true; await bridge.close(); } };
  t.after(close);
  const request = (path, options = {}) => fetch(origin + path, { ...options,
    headers: { authorization: `Bearer ${env.NANOCODEX_PHONE_BRIDGE_TOKEN}`, 'content-type': 'application/json', ...options.headers } });
  const callBody = { agent_id: agent, operation_id: randomUUID(), to: '+14155550123', instructions: 'Ask for opening hours.', max_duration_seconds: 60 };
  const call = value => request('/calls', { method: 'POST', body: JSON.stringify(value ?? callBody) });
  return { origin, request, call, callBody, provider, close, input, emit: event => voiceEvent(event), creates: () => creates, hangups: () => hangups };
}

test('requires bearer auth; rejects invalid dial before starting a provider call', async t => {
  const service = await setup(t);
  assert.equal((await service.request('/calls', { method: 'POST', headers: { authorization: 'Bearer wrong' }, body: '{}' })).status, 401);
  assert.equal((await service.call({ ...service.callBody, to: '911' })).status, 400);
  assert.equal((await service.call({ ...service.callBody, max_duration_seconds: 10000 })).status, 400);
  assert.equal(service.creates(), 0);
});

test('durable operation identity prevents duplicate dialing and rejects changed intent', async t => {
  const service = await setup(t);
  const first = await (await service.call()).json();
  const replay = await (await service.call()).json();
  assert.equal(first.call_id, replay.call_id); assert.equal(service.creates(), 1);
  assert.equal((await service.call({ ...service.callBody, instructions: 'Something different' })).status, 409);
  assert.equal((await service.request(`/calls/${first.call_id}?agent_id=${otherAgent}`)).status, 404);
  assert.equal((await service.request(`/calls/${first.call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: otherAgent }) })).status, 404);
  const ended = await (await service.request(`/calls/${first.call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) })).json();
  assert.equal(ended.status, 'completed'); assert.equal(service.hangups(), 1);
});

test('ambiguous create outcome is retained, including across restart', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'nanocodex-phone-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const database = join(directory, 'calls.sqlite');
  let attempts = 0;
  const service = await setup(t, { database, provider: { async create() { attempts++; throw new Error('upstream contains private data'); } } });
  const first = await (await service.call()).json();
  assert.equal(first.status, 'unknown'); assert.equal(first.error, 'call_start_failed_or_unknown');
  await service.call(); assert.equal(attempts, 1);
  assert.equal((await (await service.call({ ...service.callBody, operation_id: randomUUID() })).json()).status, 'unknown');
  assert.equal(attempts, 2);
  await service.close();
  const restarted = await setup(t, { database });
  const replay = await (await restarted.call(service.callBody)).json();
  assert.equal(replay.call_id, first.call_id); assert.equal(restarted.creates(), 0);
});

test('signed status callbacks bind account and SID; reordered callbacks cannot resurrect a call', async t => {
  const service = await setup(t);
  const call = await (await service.call()).json();
  const path = `/status/${call.call_id}`;
  const callback = async (status, sequence, account = env.TWILIO_ACCOUNT_SID, valid = true) => {
    const fields = new URLSearchParams({ AccountSid: account, CallSid: sid, CallStatus: status, SequenceNumber: String(sequence) });
    return service.request(path, { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded', 'x-twilio-signature': valid ? signature(path, fields) : 'wrong' }, body: fields.toString() });
  };
  assert.equal((await callback('completed', 3, undefined, false)).status, 403);
  assert.equal((await callback('completed', 3, 'AC' + '9'.repeat(32))).status, 400);
  assert.equal((await callback('completed', 3)).status, 204);
  assert.equal((await callback('ringing', 1)).status, 204);
  const state = await (await service.request(`/calls/${call.call_id}?agent_id=${agent}`)).json();
  assert.equal(state.status, 'completed');
});

test('signed WebSocket transports audio and interruption; no arbitrary stream can join', async t => {
  const service = await setup(t);
  const call = await (await service.call()).json();
  const path = `/media/${call.call_id}/`;
  const denied = new WebSocket(service.origin.replace('http:', 'ws:') + path);
  const denial = await new Promise(resolve => { denied.on('unexpected-response', (_req, response) => { resolve(response.statusCode); denied.terminate(); }); denied.on('error', () => {}); });
  assert.equal(denial, 403);
  const ws = new WebSocket(service.origin.replace('http:', 'ws:') + path, { headers: { 'x-twilio-signature': signature(path) } });
  await once(ws, 'open');
  ws.send(JSON.stringify({ event: 'start', streamSid: stream, start: { accountSid: env.TWILIO_ACCOUNT_SID,
    callSid: sid, streamSid: stream, customParameters: { callId: call.call_id }, mediaFormat: { encoding: 'audio/x-mulaw', sampleRate: 8000, channels: 1 } } }));
  const payload = Buffer.alloc(160, 255).toString('base64');
  ws.send(JSON.stringify({ event: 'media', streamSid: stream, media: { track: 'inbound', payload } }));
  // Wait for the inbound transport, not a fixed media timing assumption.
  for (let count = 0; service.input.length === 0 && count < 100; count++) await new Promise(resolve => setTimeout(resolve, 5));
  assert.deepEqual(service.input, [{ type: 'audio', audio: payload }]);
  const messages = [];
  ws.on('message', data => messages.push(JSON.parse(data.toString())));
  service.emit({ type: 'audio', audio: payload });
  service.emit({ type: 'clear' });
  service.emit({ type: 'transcript', speaker: 'assistant', text: 'Hello' });
  for (let count = 0; messages.length < 3 && count < 100; count++) await new Promise(resolve => setTimeout(resolve, 5));
  assert.deepEqual(messages.map(event => event.event), ['media', 'mark', 'clear']);
  const state = await (await service.request(`/calls/${call.call_id}?agent_id=${agent}`)).json();
  assert.deepEqual(state.transcript, [{ speaker: 'assistant', text: 'Hello' }]);
  ws.close(); await once(ws, 'close');
});

async function signedCallback(service, callId, status = 'ringing', sequence = 1) {
  const path = `/status/${callId}`;
  const fields = new URLSearchParams({ AccountSid: env.TWILIO_ACCOUNT_SID, CallSid: sid, CallStatus: status, SequenceNumber: String(sequence) });
  return service.request(path, { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded', 'x-twilio-signature': signature(path, fields) }, body: fields.toString() });
}

test('callback before failed create response still ends the now-known call', async t => {
  let service;
  service = await setup(t, { provider: { async create(_env, input) {
    assert.equal((await signedCallback(service, input.callId)).status, 204);
    throw new Error('lost create response');
  } } });
  const result = await (await service.call()).json();
  assert.equal(result.status, 'completed'); assert.equal(service.hangups(), 1);
});

test('late SID after failed create response is terminated once', async t => {
  const service = await setup(t, { provider: { async create() { throw new Error('lost create response'); } } });
  const result = await (await service.call()).json();
  assert.equal(result.status, 'unknown'); assert.equal(service.hangups(), 0);
  assert.equal((await signedCallback(service, result.call_id)).status, 204);
  assert.equal(service.hangups(), 1);
  assert.equal((await signedCallback(service, result.call_id, 'ringing', 2)).status, 204);
  assert.equal(service.hangups(), 1);
});

test('restart retains stop intent for a SID first learned from a later callback', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'nanocodex-phone-late-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const database = join(directory, 'calls.sqlite');
  const service = await setup(t, { database, provider: { async create() { throw new Error('lost create response'); } } });
  const result = await (await service.call()).json();
  await service.close();
  const restarted = await setup(t, { database });
  assert.equal((await signedCallback(restarted, result.call_id)).status, 204);
  assert.equal(restarted.hangups(), 1);
});

test('hangup during dialing is applied after the provider returns its SID', async t => {
  let admit, entered;
  const started = new Promise(resolve => { entered = resolve; });
  const service = await setup(t, { provider: { async create() { entered(); return new Promise(resolve => { admit = resolve; }); } } });
  const first = service.call();
  await started;
  const replay = await (await service.call()).json();
  assert.equal(replay.status, 'unknown');
  const stopped = await (await service.request(`/calls/${replay.call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) })).json();
  assert.equal(stopped.status, 'unknown');
  admit({ sid, status: 'queued' });
  assert.equal((await (await first).json()).status, 'completed');
  assert.equal(service.hangups(), 1);
});

test('WSS signature uses configured origin; a signature for another host is rejected', async t => {
  const service = await setup(t);
  const call = await (await service.call()).json();
  const path = `/media/${call.call_id}/`;
  const wrong = createHmac('sha1', env.TWILIO_AUTH_TOKEN).update(`wss://other.example${path}`).digest('base64');
  const denied = new WebSocket(service.origin.replace('http:', 'ws:') + path, { headers: { 'x-twilio-signature': wrong, host: 'other.example' } });
  const denial = await new Promise(resolve => { denied.on('unexpected-response', (_req, response) => { resolve(response.statusCode); denied.terminate(); }); denied.on('error', () => {}); });
  assert.equal(denial, 403);
  const signed = createHmac('sha1', env.TWILIO_AUTH_TOKEN).update(`wss://phone.example${path}`).digest('base64');
  const ws = new WebSocket(service.origin.replace('http:', 'ws:') + path, { headers: { 'x-twilio-signature': signed } });
  await once(ws, 'open'); ws.close(); await once(ws, 'close');
});


test('transcript persistence stays within the tool response byte budget', async t => {
  const service = await setup(t);
  const call = await (await service.call()).json();
  for (let index = 0; index < 200; index++) service.emit({ type: 'transcript', speaker: 'user', text: '声'.repeat(4000) });
  const response = await service.request(`/calls/${call.call_id}?agent_id=${agent}`);
  const text = await response.text();
  assert.ok(Buffer.byteLength(text) < 1024 * 1024);
  const snapshot = JSON.parse(text);
  assert.equal(snapshot.transcript_truncated, true);
  assert.ok(snapshot.transcript.length > 0);
  const listed = await (await service.request(`/calls?agent_id=${agent}`)).json();
  assert.equal(listed.calls[0].transcript_truncated, true);
  assert.ok(Buffer.byteLength(JSON.stringify(listed)) < 8000);
});

test('silent cloud check starts voice without dialing', async t => {
  const service = await setup(t);
  const response = await service.request('/check', { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
  assert.equal(response.status, 200);
  assert.equal((await response.json()).status, 'voice_ready');
  assert.equal(service.creates(), 0);
});

test('cloud checkpoints are acknowledged before dialing and hydrate after disk loss', async t => {
  const originalFetch = globalThis.fetch;
  const rows = new Map();
  const stateUrl = 'https://phone.example/internal/state';
  globalThis.fetch = async (url, init) => {
    if (String(url).startsWith(stateUrl)) {
      assert.equal(init.headers.authorization, `Bearer ${env.NANOCODEX_PHONE_BRIDGE_TOKEN}`);
      if (init.method === 'POST') { const row = JSON.parse(init.body); assert.equal(typeof row.record, 'string'); rows.set(row.id, row); return Response.json({ ok: true }); }
      return Response.json({ calls: [...rows.values()] });
    }
    return originalFetch(url, init);
  };
  t.after(() => { globalThis.fetch = originalFetch; });
  let creates = 0;
  const service = await setup(t, { env: { NANOCODEX_PHONE_STATE_URL: stateUrl }, provider: {
    async create() { creates++; assert.equal(JSON.parse([...rows.values()][0].record).dial_requested, true); return { sid, status: 'queued' }; }
  } });
  const first = await (await service.call()).json();
  await service.close();
  const fresh = await setup(t, { env: { NANOCODEX_PHONE_STATE_URL: stateUrl } });
  const replay = await (await fresh.call(service.callBody)).json();
  assert.equal(replay.call_id, first.call_id);
  assert.equal(creates, 1);
  assert.equal(fresh.creates(), 0);
});

test('failed cloud acknowledgement prevents provider dialing', async t => {
  const originalFetch = globalThis.fetch;
  const stateUrl = 'https://phone.example/internal/state';
  globalThis.fetch = async (url, init) => String(url).startsWith(stateUrl)
    ? (init.method === 'POST' ? new Response(null, { status: 503 }) : Response.json({ calls: [] }))
    : originalFetch(url, init);
  t.after(() => { globalThis.fetch = originalFetch; });
  const service = await setup(t, { env: { NANOCODEX_PHONE_STATE_URL: stateUrl } });
  assert.equal((await service.call()).status, 503);
  assert.equal(service.creates(), 0);
});


async function waitFor(predicate) {
  for (let count = 0; count < 200 && !predicate(); count++) await new Promise(resolve => setTimeout(resolve, 5));
  assert.ok(predicate(), 'expected asynchronous operation to reach its checkpoint');
}

function controlledVoices() {
  const voices = [];
  return { voices, startVoice(_binary, instructions, emit) {
    const { promise, resolve, reject } = Promise.withResolvers();
    const voice = { instructions, emit, ready: promise, resolve, reject, closed: 0, input: [],
      send(event) { this.input.push(event); }, close() { this.closed++; } };
    voices.push(voice);
    return voice;
  } };
}

async function expectBusy(response) {
  assert.equal(response.status, 409);
  assert.equal((await response.json()).error, 'phone_busy');
}

test('four simultaneous preparing calls reserve capacity; replay and completion preserve admission', async t => {
  const controlled = controlledVoices();
  const service = await setup(t, { startVoice: controlled.startVoice });
  const bodies = Array.from({ length: 4 }, () => ({ ...service.callBody, operation_id: randomUUID() }));
  const pending = bodies.map(body => service.call(body));
  try {
    await waitFor(() => controlled.voices.length === 4);
    assert.equal(service.creates(), 0);
    const replay = await (await service.call(bodies[0])).json();
    assert.equal(replay.status, 'preparing');
    await expectBusy(await service.call());
    assert.equal(controlled.voices.length, 4);
    controlled.voices.forEach(voice => voice.resolve());
    const calls = await Promise.all(pending.map(async response => (await response).json()));
    assert.equal(new Set(calls.map(call => call.call_id)).size, 4);
    assert.equal(calls[0].call_id, replay.call_id);
    assert.equal(service.creates(), 4);
    assert.equal((await (await service.call(bodies[0])).json()).call_id, replay.call_id);
    assert.equal(service.creates(), 4);
    const stopped = await service.request(`/calls/${calls[0].call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
    assert.equal((await stopped.json()).status, 'completed');
    const replacement = service.call();
    await waitFor(() => controlled.voices.length === 5);
    controlled.voices[4].resolve();
    assert.equal((await (await replacement).json()).status, 'queued');
    assert.equal(service.creates(), 5);
  } finally { controlled.voices.forEach(voice => voice.resolve()); await Promise.allSettled(pending); }
});

test('an unknown call consumes one slot, including after restart', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'nanocodex-phone-capacity-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const database = join(directory, 'calls.sqlite');
  const first = await setup(t, { database, provider: { async create() { throw new Error('ambiguous provider result'); } } });
  assert.equal((await (await first.call()).json()).status, 'unknown');
  await first.close();
  const service = await setup(t, { database });
  for (let index = 0; index < 3; index++) {
    assert.equal((await (await service.call({ ...service.callBody, operation_id: randomUUID() })).json()).status, 'queued');
  }
  await expectBusy(await service.call());
  assert.equal(service.creates(), 3);
  await service.close();
});

test('transcripts and voice lifetimes remain independent across concurrent calls', async t => {
  const controlled = controlledVoices();
  const service = await setup(t, { startVoice(...args) { const voice = controlled.startVoice(...args); voice.resolve(); return voice; } });
  const first = await (await service.call()).json();
  const second = await (await service.call({ ...service.callBody, operation_id: randomUUID() })).json();
  const [one, two] = controlled.voices;
  one.emit({ type: 'transcript', speaker: 'assistant', text: 'First call' });
  two.emit({ type: 'transcript', speaker: 'user', text: 'Second call' });
  const state = async call => (await service.request(`/calls/${call.call_id}?agent_id=${agent}`)).json();
  assert.deepEqual((await state(first)).transcript, [{ speaker: 'assistant', text: 'First call' }]);
  assert.deepEqual((await state(second)).transcript, [{ speaker: 'user', text: 'Second call' }]);
  await service.request(`/calls/${first.call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
  assert.equal(one.closed, 1);
  assert.equal(two.closed, 0);
  two.emit({ type: 'transcript', speaker: 'assistant', text: 'Still here' });
  assert.deepEqual((await state(second)).transcript, [{ speaker: 'user', text: 'Second call' }, { speaker: 'assistant', text: 'Still here' }]);
  assert.equal((await state(first)).status, 'completed');
  assert.notEqual((await state(second)).status, 'completed');
  assert.equal(service.hangups(), 1);
});

test('concurrent silent checks share capacity with calls and release successful and failed slots', async t => {
  const controlled = controlledVoices();
  const service = await setup(t, { startVoice: controlled.startVoice });
  const check = () => service.request('/check', { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
  const pending = Array.from({ length: 4 }, check);
  try {
    await waitFor(() => controlled.voices.length === 4);
    await expectBusy(await check());
    await expectBusy(await service.call());
    assert.equal(controlled.voices.length, 4);
    controlled.voices[0].resolve();
    assert.equal((await pending[0]).status, 200);
    assert.equal(controlled.voices[0].closed, 1);
    const callPending = service.call();
    await waitFor(() => controlled.voices.length === 5);
    controlled.voices[4].resolve();
    assert.equal((await (await callPending).json()).status, 'queued');
    await expectBusy(await check());
    controlled.voices[1].reject(new Error('fake voice startup failed'));
    assert.ok((await pending[1]).status >= 500);
    assert.equal(controlled.voices[1].closed, 1);
    const replacement = check();
    await waitFor(() => controlled.voices.length === 6);
    controlled.voices[5].resolve();
    assert.equal((await replacement).status, 200);
    assert.equal(controlled.voices[5].closed, 1);
    controlled.voices[2].resolve(); controlled.voices[3].resolve();
    assert.equal((await pending[2]).status, 200);
    assert.equal((await pending[3]).status, 200);
    assert.equal(service.creates(), 1);
  } finally { controlled.voices.forEach(voice => voice.resolve()); await Promise.allSettled(pending); }
});

test('simultaneous signed media streams route audio and interruption only to their own call', async t => {
  const controlled = controlledVoices();
  const callSids = ['CA' + '5'.repeat(32), 'CA' + '6'.repeat(32)];
  const streamSids = ['MZ' + '7'.repeat(32), 'MZ' + '8'.repeat(32)];
  let created = 0;
  const service = await setup(t, {
    startVoice(...args) { const voice = controlled.startVoice(...args); voice.resolve(); return voice; },
    provider: { async create() { return { sid: callSids[created++], status: 'queued' }; } }
  });
  const calls = [];
  for (let index = 0; index < 2; index++) calls.push(await (await service.call({ ...service.callBody, operation_id: randomUUID() })).json());
  const sockets = [], messages = [[], []];
  const payloads = [Buffer.alloc(160, 123).toString('base64'), Buffer.alloc(160, 234).toString('base64')];
  try {
    for (let index = 0; index < 2; index++) {
      const path = `/media/${calls[index].call_id}/`;
      const ws = new WebSocket(service.origin.replace('http:', 'ws:') + path, { headers: { 'x-twilio-signature': signature(path) } });
      sockets.push(ws);
      ws.on('message', data => messages[index].push(JSON.parse(data.toString())));
      await once(ws, 'open');
      ws.send(JSON.stringify({ event: 'start', streamSid: streamSids[index], start: {
        accountSid: env.TWILIO_ACCOUNT_SID, callSid: callSids[index], streamSid: streamSids[index],
        customParameters: { callId: calls[index].call_id }, mediaFormat: { encoding: 'audio/x-mulaw', sampleRate: 8000, channels: 1 }
      } }));
      ws.send(JSON.stringify({ event: 'media', streamSid: streamSids[index], media: { track: 'inbound', payload: payloads[index] } }));
    }
    await waitFor(() => controlled.voices.every(voice => voice.input.length === 1));
    for (let index = 0; index < 2; index++) {
      assert.deepEqual(controlled.voices[index].input, [{ type: 'audio', audio: payloads[index] }]);
      controlled.voices[index].emit({ type: 'audio', audio: payloads[index] });
    }
    controlled.voices[0].emit({ type: 'clear' });
    await waitFor(() => messages[0].length >= 3 && messages[1].length >= 2);
    assert.deepEqual(messages[0].map(event => event.event), ['media', 'mark', 'clear']);
    assert.deepEqual(messages[1].map(event => event.event), ['media', 'mark']);
    for (let index = 0; index < 2; index++) {
      assert.ok(messages[index].every(event => event.streamSid === streamSids[index]));
      assert.equal(messages[index][0].media.payload, payloads[index]);
    }
    const firstClosed = once(sockets[0], 'close');
    await service.request(`/calls/${calls[0].call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
    await firstClosed;
    assert.equal(sockets[1].readyState, WebSocket.OPEN);
    controlled.voices[1].emit({ type: 'clear' });
    await waitFor(() => messages[1].length >= 3);
    assert.deepEqual(messages[1][2], { event: 'clear', streamSid: streamSids[1] });
    assert.equal(controlled.voices[1].closed, 0);
  } finally {
    await Promise.all(sockets.filter(ws => ws.readyState !== WebSocket.CLOSED).map(async ws => {
      const closed = once(ws, 'close'); ws.close(); await closed;
    }));
  }
});

test('each call prepares a journaled agent thread and sends scoped tool results back to voice', async t => {
  const delegateAgent = '33333333-3333-7333-8333-333333333333';
  const delegateSession = '44444444-4444-7444-8444-444444444444';
  let prepared = false, closed = 0, received;
  const service = await setup(t, { startDelegation(options) {
    assert.equal(options.goal, 'Ask for opening hours.');
    assert.equal(options.parent_agent_id, agent);
    return {
      async prepare() { await options.onAgentCreated(delegateAgent, delegateSession); prepared = true; },
      async run(value) { received = value; return 'Open until five.'; },
      async close() { closed++; }
    };
  }, provider: { async create() { assert.equal(prepared, true); return { sid, status: 'queued' }; } } });
  const call = await (await service.call()).json();
  assert.equal(call.call_agent_id, delegateAgent);
  const request = { type: 'delegation', id: 'lookup-1', input: 'Find closing time', transcript: [{ role: 'user', text: 'When do you close?' }] };
  await service.emit(request);
  assert.deepEqual(received, { id: request.id, input: request.input, transcript: request.transcript });
  assert.deepEqual(service.input, [{ type: 'tool_result', id: 'lookup-1', text: 'Open until five.' }]);
  await service.request(`/calls/${call.call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
  await waitFor(() => closed === 1);
});

test('restart retries unfinished delegated work cleanup even after the telephone call ended', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'nanocodex-phone-delegate-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const database = join(directory, 'calls.sqlite');
  const delegateAgent = '33333333-3333-7333-8333-333333333333';
  const delegateSession = '44444444-4444-7444-8444-444444444444';
  let attempted = false;
  const service = await setup(t, { database, startDelegation: ({ onAgentCreated }) => ({
    async prepare() { await onAgentCreated(delegateAgent, delegateSession); },
    async run() { return 'unused'; }, async close() { attempted = true; throw new Error('temporary cleanup outage'); }
  }) });
  const call = await (await service.call()).json();
  await service.request(`/calls/${call.call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
  await waitFor(() => attempted);
  await service.close();
  const recovered = [];
  const restart = await setup(t, { database, async stopDelegate(_env, id, session) { recovered.push([id, session]); } });
  assert.equal((await restart.request('/health')).status, 200);
  assert.deepEqual(recovered, [[delegateAgent, delegateSession]]);
});


test('list isolates ownership and includes destinations; steering is idempotent and never redials', async t => {
  const service = await setup(t);
  const call = await (await service.call()).json();
  assert.deepEqual((await (await service.request(`/calls?agent_id=${agent}`)).json()).calls, [call]);
  assert.equal(call.to, service.callBody.to);
  assert.deepEqual(await (await service.request(`/calls?agent_id=${otherAgent}`)).json(), { calls: [] });
  const body = { agent_id: agent, operation_id: randomUUID(), instructions: 'Ask about Saturday hours too.' };
  const steer = value => service.request(`/calls/${call.call_id}/steer`, { method: 'POST', body: JSON.stringify(value) });
  assert.equal((await steer({ ...body, agent_id: otherAgent })).status, 404);
  const first = await (await steer(body)).json();
  assert.deepEqual(first.steering, { operation_id: body.operation_id, status: 'submitted' });
  assert.deepEqual((await (await steer(body)).json()).steering, first.steering);
  assert.equal((await steer({ ...body, instructions: 'Different' })).status, 409);
  assert.equal(service.input.filter(event => event.type === 'steer').length, 1);
  assert.equal(service.creates(), 1);
  await service.request(`/calls/${call.call_id}/hangup`, { method: 'POST', body: JSON.stringify({ agent_id: agent }) });
  assert.equal((await steer({ ...body, operation_id: randomUUID() })).status, 409);
  assert.deepEqual((await (await steer(body)).json()).steering, first.steering);
});

test('steering journals pending before delivery and submitted afterwards', async t => {
  const originalFetch = globalThis.fetch;
  const stateUrl = 'https://phone.example/internal/state';
  const receipts = [];
  globalThis.fetch = async (url, init) => {
    if (String(url).startsWith(stateUrl)) {
      if (init.method === 'POST') { receipts.push(JSON.parse(JSON.parse(init.body).record)); return Response.json({ ok: true }); }
      return Response.json({ calls: [] });
    }
    return originalFetch(url, init);
  };
  t.after(() => { globalThis.fetch = originalFetch; });
  let deliveries = 0;
  const service = await setup(t, { env: { NANOCODEX_PHONE_STATE_URL: stateUrl }, startVoice() {
    return { ready: Promise.resolve(), close() {}, send(event) {
      if (event.type === 'steer') { deliveries++; assert.equal(receipts.at(-1).steering.at(-1).status, 'pending'); }
    } };
  } });
  const call = await (await service.call()).json();
  const body = { agent_id: agent, operation_id: randomUUID(), instructions: 'Ask about weekend hours.' };
  const send = () => service.request(`/calls/${call.call_id}/steer`, { method: 'POST', body: JSON.stringify(body) });
  const responses = await Promise.all([send(), send()]);
  for (const response of responses) assert.equal((await response.json()).steering.status, 'submitted');
  assert.equal(deliveries, 1);
  assert.equal(receipts.at(-1).steering.at(-1).status, 'submitted');
  await service.close();
});


test('bridge rejects cumulative amendment overflow before delivery and preserves replay', async t => {
  const service = await setup(t);
  const call = await (await service.call()).json();
  const send = body => service.request(`/calls/${call.call_id}/steer`, { method: 'POST', body: JSON.stringify(body) });
  const first = { agent_id: agent, operation_id: randomUUID(), instructions: 'a'.repeat(8000) };
  assert.equal((await send(first)).status, 200);
  assert.equal((await send({ ...first, operation_id: randomUUID(), instructions: 'b'.repeat(8000) })).status, 200);
  const overflow = await send({ ...first, operation_id: randomUUID(), instructions: 'c'.repeat(400) });
  assert.equal(overflow.status, 409);
  assert.equal((await overflow.json()).error, 'steering_limit');
  assert.equal((await send(first)).status, 200);
  assert.equal(service.input.filter(event => event.type === 'steer').length, 2);
  assert.equal(service.creates(), 1);
});
