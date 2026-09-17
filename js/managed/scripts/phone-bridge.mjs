/** Single-owner native telephone bridge. Run behind an HTTPS/WebSocket reverse proxy. */
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createHash, randomUUID, timingSafeEqual } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { chmodSync } from 'node:fs';
import { isAbsolute } from 'node:path';
import { pathToFileURL } from 'node:url';
import { WebSocketServer } from 'ws';
import { StringDecoder } from 'node:string_decoder';
import { createPhoneDelegation, stopPhoneDelegate } from './phone-delegation.mjs';
import {
  createTwilioVoiceCall, fetchTwilioVoiceCall, hangupTwilioVoiceCall,
  verifyTwilioWebhookSignature, isE164PhoneNumber,
} from '../src/twilio-voice.ts';

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const TERMINAL = new Set(['completed', 'busy', 'failed', 'no-answer', 'canceled']);
const STATUSES = new Set(['queued', 'initiated', 'ringing', 'in-progress', ...TERMINAL]);
const MAX_CONCURRENT_CALLS = 4; // Bound native voice processes on the shared container.
const MAX_BODY = 32 * 1024;
const MAX_AUDIO = 8000; // At most one second per frame, normally 20 ms.
const MAX_BUFFER = 128 * 1024;
const failure = (status, message) => Object.assign(new Error(message), { status });
function exact(value, fields) {
  return value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).every(key => fields.includes(key));
}
function safeEqual(a, b) {
  const left = Buffer.from(a ?? ''), right = Buffer.from(b ?? '');
  return left.length === right.length && timingSafeEqual(left, right);
}
async function body(request) {
  let size = 0; const chunks = [];
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_BODY) throw failure(413, 'request_too_large');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}
function audioPayload(value) {
  if (typeof value !== 'string' || value.length === 0 || value.length > Math.ceil(MAX_AUDIO / 3) * 4
    || value.length % 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) return false;
  const bytes = Buffer.from(value, 'base64');
  return bytes.length <= MAX_AUDIO && bytes.toString('base64') === value;
}

/** Credentials stay in the operator's native process; the child needs only existing voice auth. */
export function nativeVoice(binary, instructions, onEvent, onFailure, agentId) {
  const childEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TWILIO_') && (!key.startsWith('NANOCODEX_PHONE_') || ['NANOCODEX_PHONE_MANAGED_ORIGIN', 'NANOCODEX_PHONE_MANAGED_API_KEY'].includes(key))));
  const child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'ignore'], env: childEnv, shell: false });
  let buffer = '', stopped = false, settled = false;
  const decoder = new StringDecoder('utf8');
  let resolveReady, rejectReady;
  const ready = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
  const fail = () => {
    if (stopped) return;
    if (!settled) { settled = true; rejectReady(failure(503, 'voice_unavailable')); }
    onFailure();
  };
  const timer = setTimeout(() => { fail(); child.kill(); }, 20_000).unref();
  child.on('error', fail);
  child.on('exit', () => { clearTimeout(timer); fail(); });
  child.stdin.on('error', fail);
  child.stdout.on('data', chunk => {
    buffer += decoder.write(chunk);
    if (Buffer.byteLength(buffer) > MAX_BUFFER) { fail(); child.kill(); return; }
    let end;
    while ((end = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
      try {
        const event = JSON.parse(line);
        if (event.type === 'ready' && !settled) { settled = true; clearTimeout(timer); resolveReady(); }
        else if (event.type === 'error') fail();
        else Promise.resolve(onEvent(event)).catch(fail);
      } catch { fail(); child.kill(); }
    }
  });
  const send = event => {
    if (stopped || child.stdin.destroyed || child.stdin.writableLength > MAX_BUFFER) throw failure(503, 'voice_backpressure');
    child.stdin.write(JSON.stringify(event) + '\n');
  };
  send({ type: 'start', agent_id: agentId, instructions });
  return { ready, send, close() {
    if (stopped) return;
    stopped = true; clearTimeout(timer);
    if (!settled) { settled = true; rejectReady(failure(503, 'voice_stopped')); }
    child.stdin.end(JSON.stringify({ type: 'stop' }) + '\n');
    setTimeout(() => child.kill('SIGKILL'), 2000).unref();
  } };
}

export function createPhoneBridge({ env = process.env, database = env.NANOCODEX_PHONE_DATABASE,
  provider = { create: createTwilioVoiceCall, status: fetchTwilioVoiceCall, hangup: hangupTwilioVoiceCall },
  startVoice = nativeVoice, startDelegation = createPhoneDelegation, stopDelegate = stopPhoneDelegate } = {}) {
  const origin = new URL(env.NANOCODEX_PHONE_PUBLIC_ORIGIN);
  if (origin.protocol !== 'https:' || origin.username || origin.password || origin.pathname !== '/' || origin.search || origin.hash)
    throw new Error('NANOCODEX_PHONE_PUBLIC_ORIGIN must be an HTTPS origin');
  const prefix = env.NANOCODEX_PHONE_PUBLIC_PREFIX ?? '';
  if (!['', '/v1/phone/bridge'].includes(prefix)) throw new Error('Invalid phone bridge public prefix');
  const publicBase = origin.origin + prefix;
  const token = env.NANOCODEX_PHONE_BRIDGE_TOKEN;
  if (typeof token !== 'string' || token.length < 32 || /\s/.test(token)) throw new Error('Configure a bridge token of at least 32 characters');
  if (!env.TWILIO_AUTH_TOKEN || !/^AC[0-9a-f]{32}$/i.test(env.TWILIO_ACCOUNT_SID ?? '') || !isE164PhoneNumber(env.TWILIO_VOICE_FROM_NUMBER))
    throw new Error('Configure Twilio Voice account, auth token, and from number');
  if (!isAbsolute(env.NANOCODEX_PHONE_VOICE_BINARY ?? '')) throw new Error('Configure an absolute phone-voice binary path');
  if (!database || (database !== ':memory:' && !isAbsolute(database))) throw new Error('Configure an absolute phone database path');
  const db = new DatabaseSync(database);
  if (database !== ':memory:') chmodSync(database, 0o600);
  db.exec('PRAGMA journal_mode = DELETE; CREATE TABLE IF NOT EXISTS calls (id TEXT PRIMARY KEY, agent TEXT NOT NULL, operation TEXT NOT NULL, fingerprint TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(agent, operation))');
  const live = new Map();
  const probes = new Set();
  const unfinished = "json_extract(record, '$.status') NOT IN ('completed', 'busy', 'failed', 'no-answer', 'canceled')";
  const atCapacity = () => Number(db.prepare(`SELECT COUNT(*) AS count FROM calls WHERE ${unfinished}`).get().count) + probes.size >= MAX_CONCURRENT_CALLS;
  const stateUrl = env.NANOCODEX_PHONE_STATE_URL;
  if (stateUrl && stateUrl !== `${publicBase}/internal/state`) throw new Error('Invalid phone state endpoint');
  let stateTail = Promise.resolve();
  const publish = id => {
    if (!stateUrl) return Promise.resolve();
    const row = db.prepare('SELECT * FROM calls WHERE id = ?').get(id);
    stateTail = stateTail.then(async () => {
      const response = await fetch(stateUrl, { method: 'POST', redirect: 'error', signal: AbortSignal.timeout(15_000),
        headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body: JSON.stringify(row) });
      if (!response.ok) throw failure(503, 'durable_state_unavailable');
      await response.body?.cancel();
    });
    // Retain failure on the admission chain without creating an unhandled rejection.
    stateTail.catch(() => {});
    return stateTail;
  };
  const save = record => {
    db.prepare('UPDATE calls SET record = ? WHERE id = ?').run(JSON.stringify(record), record.call_id);
    return publish(record.call_id);
  };
  const hydrate = async () => {
    if (!stateUrl) return;
    let cursor;
    do {
      const response = await fetch(stateUrl + (cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''), {
        redirect: 'error', signal: AbortSignal.timeout(15_000), headers: { authorization: `Bearer ${token}` } });
      if (!response.ok) throw failure(503, 'durable_state_unavailable');
      let size = 0; const chunks = [];
      for await (const chunk of response.body) {
        size += chunk.length; if (size > 8 * 1024 * 1024) throw failure(503, 'invalid_durable_state'); chunks.push(chunk);
      }
      const page = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (!Array.isArray(page.calls) || page.calls.length > 10) throw failure(503, 'invalid_durable_state');
      for (const row of page.calls) {
        if (!UUID.test(row.id ?? '') || !UUID.test(row.agent ?? '') || !UUID.test(row.operation ?? '')
          || !/^[0-9a-f]{64}$/.test(row.fingerprint ?? '') || typeof row.record !== 'string'
          || JSON.parse(row.record).call_id !== row.id) throw failure(503, 'invalid_durable_state');
        db.prepare('INSERT OR REPLACE INTO calls VALUES (?, ?, ?, ?, ?)').run(row.id, row.agent, row.operation, row.fingerprint, row.record);
      }
      const next = page.next_cursor;
      if (next && (!UUID.test(next) || (cursor && next <= cursor))) throw failure(503, 'invalid_durable_state');
      cursor = next;
    } while (cursor);
  };
  const read = id => { const row = db.prepare('SELECT record FROM calls WHERE id = ?').get(id); return row && JSON.parse(row.record); };
  const snapshot = record => ({ call_id: record.call_id, status: record.status, transcript: record.transcript, transcript_truncated: record.transcript_truncated === true,
    ...(record.error ? { error: record.error } : {}),
    ...(record.delegate_agent_id ? { call_agent_id: record.delegate_agent_id } : {}), max_duration_seconds: record.max_duration_seconds });
  const delegateCleanup = new Map();
  const releaseDelegate = (id, controller) => {
    if (delegateCleanup.has(id)) return delegateCleanup.get(id);
    const task = (async () => {
      if (controller) await controller.close();
      const record = read(id);
      if (!record?.delegate_agent_id || record.delegate_cleaned) return;
      if (!controller) await stopDelegate(env, record.delegate_agent_id, record.delegate_session_id);
      const latest = read(id); latest.delegate_cleaned = true; await save(latest);
    })().finally(() => delegateCleanup.delete(id));
    task.catch(() => console.warn('Phone delegated-agent cleanup pending'));
    delegateCleanup.set(id, task); return task;
  };
  const cleanup = id => {
    const active = live.get(id);
    if (active) {
      live.delete(id); clearTimeout(active.timer); clearTimeout(active.attachTimer);
      active.voice?.close(); if (active.socket && active.socket !== true) active.socket.close(1000);
    }
    void releaseDelegate(id, active?.delegate);
  };
  const finishing = new Map();
  const finish = (id, reason, explicit = false) => {
    if (finishing.has(id)) return finishing.get(id);
    const promise = finishCall(id, reason, explicit).catch(async error => {
      cleanup(id);
      const record = read(id);
      if (record?.sid && !record.hangup_attempted) {
        record.hangup_attempted = true;
        db.prepare('UPDATE calls SET record = ? WHERE id = ?').run(JSON.stringify(record), id);
        await provider.hangup(env, record.sid).catch(() => {});
      }
      throw error;
    }).finally(() => finishing.delete(id));
    promise.catch(() => {});
    finishing.set(id, promise); return promise;
  };
  const finishCall = async (id, reason, explicit) => {
    let record = read(id);
    if (!record) return;
    // Durable intent survives a lost create response and callbacks delivering the SID later.
    record.stop_requested = true;
    if (reason) record.error = reason;
    await save(record); cleanup(id);
    if (TERMINAL.has(record.status)) return record;
    if (record.sid && (!record.hangup_attempted || explicit)) {
      record.hangup_attempted = true; await save(record);
      try {
        const result = await provider.hangup(env, record.sid);
        record = read(id);
        if (!TERMINAL.has(record.status)) record.status = result.status;
      } catch {
        record = read(id);
        if (!TERMINAL.has(record.status)) { record.status = 'unknown'; record.error = 'hangup_unconfirmed'; }
      }
    } else if (!record.sid) record.status = record.dial_requested ? 'unknown' : 'failed';
    await save(record); return record;
  };
  // A restart cannot resume a media peer. Release known calls; retain idempotency records.
  const recovering = hydrate().then(() => Promise.all(db.prepare(`SELECT record FROM calls`).all().map(async ({ record: text }) => {
    const record = JSON.parse(text);
    if (!TERMINAL.has(record.status)) await finish(record.call_id, 'bridge_restarted');
    if (record.delegate_agent_id && !record.delegate_cleaned) await releaseDelegate(record.call_id);
  })));
  recovering.catch(() => {});
  const eventFor = id => async event => {
    const active = live.get(id); if (!active) return;
    if (event.type === 'delegation') {
      if (typeof event.id !== 'string' || !/^[A-Za-z0-9_.:-]{1,256}$/.test(event.id)
        || typeof event.input !== 'string' || !event.input.trim() || Buffer.byteLength(event.input) > 8000
        || !Array.isArray(event.transcript) || Buffer.byteLength(JSON.stringify(event.transcript)) > 32768) return;
      let text;
      try {
        text = await active.delegate.run({ id: event.id, input: event.input, transcript: event.transcript });
      } catch { text = "I couldn't complete that lookup. Do not guess or claim it succeeded."; }
      if (live.get(id) === active && typeof text === 'string') {
        active.voice.send({ type: 'tool_result', id: event.id, text: text.trim() ? text.slice(0, 4000) : 'The lookup returned no usable answer.' });
      }
    } else if (event.type === 'transcript') {
      if (!['user', 'assistant'].includes(event.speaker) || typeof event.text !== 'string') return;
      const record = read(id);
      const entry = { speaker: event.speaker, text: event.text.slice(0, 4000) };
      const bytes = Buffer.byteLength(JSON.stringify(entry));
      if (record.transcript.length < 200 && (record.transcript_bytes ?? 0) + bytes <= 512 * 1024) {
        record.transcript.push(entry); record.transcript_bytes = (record.transcript_bytes ?? 0) + bytes;
      } else record.transcript_truncated = true;
      if (event.text.length > 4000) record.transcript_truncated = true;
      await save(record);
    } else if (event.type === 'ended') void finish(id);
    else if (event.type === 'audio' || event.type === 'clear') {
      if (!active.socket || !active.stream) return;
      if (active.socket.bufferedAmount > MAX_BUFFER) { void finish(id, 'media_backpressure'); return; }
      if (event.type === 'clear') {
        active.socket.send(JSON.stringify({ event: 'clear', streamSid: active.stream }));
        active.marks.clear();
      } else {
        if (!audioPayload(event.audio)) { void finish(id, 'invalid_voice_audio'); return; }
        if (active.marks.size >= 100) { void finish(id, 'playback_backpressure'); return; }
        active.socket.send(JSON.stringify({ event: 'media', streamSid: active.stream, media: { payload: event.audio } }));
        const name = String(++active.sequence); active.marks.add(name);
        active.socket.send(JSON.stringify({ event: 'mark', streamSid: active.stream, mark: { name } }));
      }
    }
  };
  const create = async value => {
    if (!exact(value, ['agent_id', 'operation_id', 'to', 'instructions', 'max_duration_seconds']) || !UUID.test(value.agent_id ?? '') || !UUID.test(value.operation_id ?? '')
      || !isE164PhoneNumber(value.to) || typeof value.instructions !== 'string' || !value.instructions.trim() || value.instructions.length > 8000)
      throw failure(400, 'invalid_call');
    const duration = value.max_duration_seconds ?? 180;
    if (!Number.isInteger(duration) || duration < 30 || duration > 600) throw failure(400, 'invalid_duration');
    const fingerprint = createHash('sha256').update(JSON.stringify([value.to, value.instructions, duration])).digest('hex');
    const previous = db.prepare('SELECT fingerprint, record FROM calls WHERE agent = ? AND operation = ?').get(value.agent_id, value.operation_id);
    if (previous) {
      if (previous.fingerprint !== fingerprint) throw failure(409, 'operation_conflict');
      return snapshot(JSON.parse(previous.record));
    }
    // Synchronous admission and insertion reserve capacity before any asynchronous work.
    // Preparing and uncertain provider outcomes retain their slots until reconciled.
    if (atCapacity()) throw failure(409, 'phone_busy');
    const id = randomUUID();
    const record = { call_id: id, status: 'preparing', transcript: [], max_duration_seconds: duration };
    db.prepare('INSERT INTO calls VALUES (?, ?, ?, ?, ?)').run(id, value.agent_id, value.operation_id, fingerprint, JSON.stringify(record));
    await publish(id);
    if (read(id).stop_requested) return snapshot(read(id));
    const active = { marks: new Set(), sequence: 0, goal: value.instructions, parentAgent: value.agent_id };
    live.set(id, active);
    try {
      active.delegate = startDelegation({ env, goal: active.goal, parent_agent_id: active.parentAgent, onAgentCreated: async (agentId, sessionId) => {
        if (!UUID.test(agentId) || !UUID.test(sessionId) || agentId === active.parentAgent) throw new Error('Invalid delegated agent');
        const record = read(id);
        if (record.delegate_agent_id && record.delegate_agent_id !== agentId) throw new Error('Delegated agent changed');
        record.delegate_agent_id = agentId; record.delegate_session_id = sessionId; record.delegate_cleaned = false; await save(record);
      } });
      await active.delegate.prepare();
      if (!live.has(id) || read(id).stop_requested) throw failure(503, 'voice_unavailable');
      active.voice = startVoice(env.NANOCODEX_PHONE_VOICE_BINARY, value.instructions, eventFor(id), () => { void finish(id, 'voice_disconnected'); }, value.agent_id);
      await active.voice.ready;
      if (!live.has(id) || read(id).stop_requested) throw failure(503, 'voice_unavailable');
      // Persist before the non-idempotent provider request, including its ambiguous failure window.
      const dialing = read(id);
      dialing.status = 'unknown'; dialing.dial_requested = true; await save(dialing);
      const streamUrl = `${publicBase.replace(/^https:/, 'wss:')}/media/${id}/`;
      const result = await provider.create(env, { to: value.to, callId: id, streamUrl, statusCallbackUrl: `${publicBase}/status/${id}`, maxDurationSeconds: duration });
      // Webhooks may already have advanced the durable status during this request.
      const latest = read(id);
      latest.sid = result.sid;
      if (latest.status === 'unknown') latest.status = result.status;
      await save(latest);
      if (!live.has(id) || latest.stop_requested) { await finish(id, 'voice_disconnected'); return snapshot(read(id)); }
      active.timer = setTimeout(() => { void finish(id, 'duration_limit'); }, (duration + 45) * 1000).unref();
      active.attachTimer = setTimeout(() => { if (!active.stream) void finish(id, 'media_unavailable'); }, 45_000).unref();
      return snapshot(latest);
    } catch {
      return snapshot(await finish(id, 'call_start_failed_or_unknown'));
    }
  };
  const owned = (id, agent) => {
    if (!UUID.test(id) || !UUID.test(agent ?? '')) throw failure(404, 'not_found');
    const row = db.prepare('SELECT record FROM calls WHERE id = ? AND agent = ?').get(id, agent);
    if (!row) throw failure(404, 'not_found');
    return JSON.parse(row.record);
  };
  const server = createServer(async (request, response) => {
    try {
      await recovering;
      await stateTail;
      const url = new URL(request.url, origin);
      let result;
      const callback = url.pathname.match(/^\/status\/([0-9a-f-]{36})$/i);
      if (callback && request.method === 'POST' && !url.search) {
        if (request.headers['content-type']?.split(';')[0] !== 'application/x-www-form-urlencoded') throw failure(415, 'invalid_content_type');
        const fields = new URLSearchParams(await body(request));
        if (!await verifyTwilioWebhookSignature(env, `${publicBase}${url.pathname}`, fields, request.headers['x-twilio-signature'] ?? null)) throw failure(403, 'invalid_signature');
        const record = read(callback[1]);
        const sid = fields.get('CallSid'), status = fields.get('CallStatus');
        if (!record || fields.get('AccountSid') !== env.TWILIO_ACCOUNT_SID || !/^CA[0-9a-f]{32}$/i.test(sid ?? '') || (record.sid && record.sid !== sid) || !STATUSES.has(status)) throw failure(400, 'invalid_callback');
        const sequence = Number(fields.get('SequenceNumber'));
        if (!Number.isSafeInteger(sequence) || sequence < 0 || !fields.has('SequenceNumber')) throw failure(400, 'invalid_sequence');
        if (sequence > (record.callback_sequence ?? -1) && !TERMINAL.has(record.status)) {
          record.sid = sid; record.status = status; record.callback_sequence = sequence; await save(record);
          if (TERMINAL.has(status)) cleanup(record.call_id);
          else if (record.stop_requested) await finish(record.call_id);
        }
        response.writeHead(204); response.end(); return;
      }
      if (!safeEqual(request.headers.authorization, `Bearer ${token}`)) throw failure(401, 'unauthorized');
      if (request.method === 'GET' && url.pathname === '/health' && !url.search) result = { status: 'ready', max_concurrent_calls: MAX_CONCURRENT_CALLS, delegation: true };
      else if (request.method === 'POST' && url.pathname === '/check' && !url.search) {
        let value; try { value = JSON.parse(await body(request)); } catch { throw failure(400, 'invalid_json'); }
        if (!exact(value, ['agent_id', 'delegation']) || !UUID.test(value.agent_id ?? '') || (value.delegation !== undefined && typeof value.delegation !== 'boolean')) throw failure(400, 'invalid_request');
        if (atCapacity()) throw failure(409, 'phone_busy');
        const probe = {}; probes.add(probe);
        try {
          probe.voice = startVoice(env.NANOCODEX_PHONE_VOICE_BINARY, 'Connectivity check. Remain silent.', () => {}, () => {}, value.agent_id);
          await probe.voice.ready; result = { status: 'voice_ready' };
          if (value.delegation) {
            probe.delegate = startDelegation({ env, parent_agent_id: value.agent_id,
              goal: 'Run a read-only public-web connectivity test. Find the official Twilio Programmable Voice documentation with web search and report its title and URL. Do not access private account data or perform any writes.',
              onAgentCreated: async agentId => { result.call_agent_id = agentId; } });
            await probe.delegate.prepare();
            result.delegation_result = await probe.delegate.run({ id: 'connectivity-check', input: 'Search the web for the official Twilio Programmable Voice documentation and return its title and URL.', transcript: [] });
          }
        } finally { probe.voice?.close(); try { await probe.delegate?.close(); } finally { probes.delete(probe); } }
      } else if (request.method === 'POST' && url.pathname === '/calls' && !url.search) {
        if (request.headers['content-type']?.split(';')[0] !== 'application/json') throw failure(415, 'invalid_content_type');
        let value; try { value = JSON.parse(await body(request)); } catch (error) { throw error.status ? error : failure(400, 'invalid_json'); }
        result = await create(value);
      } else {
        const match = url.pathname.match(/^\/calls\/([0-9a-f-]{36})(\/hangup)?$/i);
        if (!match) throw failure(404, 'not_found');
        if (request.method === 'GET' && !match[2] && [...url.searchParams.keys()].every(key => key === 'agent_id') && url.searchParams.getAll('agent_id').length === 1) {
          let record = owned(match[1], url.searchParams.get('agent_id'));
          if (record.sid && !TERMINAL.has(record.status)) {
            try { const observed = await provider.status(env, record.sid); record = read(record.call_id); if (!TERMINAL.has(record.status)) record.status = observed.status; await save(record); if (TERMINAL.has(record.status)) cleanup(record.call_id); }
            catch { record.error = 'status_unavailable'; }
          }
          result = snapshot(record);
        } else if (request.method === 'POST' && match[2] && !url.search) {
          let value; try { value = JSON.parse(await body(request)); } catch { throw failure(400, 'invalid_json'); }
          if (!exact(value, ['agent_id'])) throw failure(400, 'invalid_request');
          owned(match[1], value.agent_id); result = snapshot(await finish(match[1], undefined, true));
        } else throw failure(405, 'method_not_allowed');
      }
      response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' }); response.end(JSON.stringify(result));
    } catch (error) {
      response.writeHead(error.status ?? 500, { 'content-type': 'application/json', 'cache-control': 'no-store' });
      response.end(JSON.stringify({ error: error.status ? error.message : 'phone_bridge_failed' }));
    }
  });
  server.requestTimeout = 45_000; server.headersTimeout = 10_000;
  const sockets = new WebSocketServer({ noServer: true, maxPayload: 16 * 1024, perMessageDeflate: false });
  server.on('upgrade', async (request, socket, head) => {
    try {
      const match = request.url?.match(/^\/media\/([0-9a-f-]{36})\/$/i);
      if (!match || !live.has(match[1]) || live.get(match[1]).socket) throw new Error();
      const publicUrl = `${publicBase}${request.url}`;
      const signature = request.headers['x-twilio-signature'] ?? null;
      // Both URLs are fixed operator configuration, never supplied Host/Forwarded headers.
      if (!await verifyTwilioWebhookSignature(env, publicUrl, new URLSearchParams(), signature)
        && !await verifyTwilioWebhookSignature(env, publicUrl.replace(/^https:/, 'wss:'), new URLSearchParams(), signature)) throw new Error();
      // Claim before yielding again so two valid upgrade requests cannot replace a stream.
      const active = live.get(match[1]); if (!active || active.socket) throw new Error();
      active.socket = true;
      sockets.handleUpgrade(request, socket, head, ws => {
        active.socket = ws;
        const startTimer = setTimeout(() => ws.close(1008), 5000).unref();
        ws.on('error', () => { if (live.has(match[1])) void finish(match[1], 'media_disconnected'); });
        ws.on('close', () => { clearTimeout(startTimer); if (live.has(match[1])) void finish(match[1]); });
        ws.on('message', async (data, binary) => {
          try {
            if (binary) throw new Error();
            const event = JSON.parse(data.toString());
            if (event.event === 'connected') return;
            if (event.event === 'start' && !active.stream) {
              const start = event.start, record = read(match[1]);
              if (start?.accountSid !== env.TWILIO_ACCOUNT_SID || start?.customParameters?.callId !== match[1]
                || !/^CA[0-9a-f]{32}$/i.test(start?.callSid ?? '') || (record.sid && start.callSid !== record.sid)
                || !/^MZ[0-9a-f]{32}$/i.test(start?.streamSid ?? '') || event.streamSid !== start.streamSid
                || start.mediaFormat?.encoding !== 'audio/x-mulaw' || start.mediaFormat?.sampleRate !== 8000 || start.mediaFormat?.channels !== 1) throw new Error();
              record.sid = start.callSid; record.status = 'in-progress'; active.stream = start.streamSid; await save(record);
               clearTimeout(startTimer); clearTimeout(active.attachTimer);
            } else if (event.streamSid !== active.stream || !active.stream) throw new Error();
            else if (event.event === 'media') {
              if (event.media?.track !== 'inbound' || !audioPayload(event.media?.payload)) throw new Error();
              active.voice.send({ type: 'audio', audio: event.media.payload });
            } else if (event.event === 'mark') active.marks.delete(event.mark?.name);
            else if (event.event === 'stop') void finish(match[1]);
            else if (event.event !== 'dtmf') throw new Error();
          } catch { void finish(match[1], 'invalid_media'); }
        });
      });
    } catch { socket.write('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n'); socket.destroy(); }
  });
  return { server, async close() {
    await recovering;
    for (const probe of probes) { probe.voice?.close(); await probe.delegate?.close(); }
    await Promise.all([...live.keys()].map(id => finish(id, 'bridge_shutdown')));
    await Promise.all([...finishing.values()]);
    await Promise.all([...delegateCleanup.values()]);
    for (const ws of sockets.clients) ws.terminate();
    sockets.close();
    await new Promise(resolve => server.close(resolve));
    db.close();
  } };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.umask(0o077);
  const bridge = createPhoneBridge();
  const port = Number(process.env.NANOCODEX_PHONE_PORT ?? 8788);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Invalid phone bridge port');
  bridge.server.listen(port, process.env.NANOCODEX_PHONE_HOST ?? '127.0.0.1', () => console.info('Phone bridge listening'));
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => { void bridge.close().then(() => process.exit(0)); });
}
