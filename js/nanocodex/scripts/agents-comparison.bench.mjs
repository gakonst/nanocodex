// Bounded live benchmark. Sends only the fixed synthetic prompt; never records credentials.
import { writeFileSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import WebSocket from 'ws';
import { Agent, Transport } from '../node/index.mjs';

const options = Object.fromEntries(process.argv.slice(2).map(arg => arg.replace(/^--/, '').split('=')));
const names = ['gpt-5.6-luna', 'gpt-5.6-terra', 'gpt-5.6-sol', 'gpt-6-astra'];
const models = options.models ? options.models.split(',') : names;
const tiers = options.tiers ? options.tiers.split(',') : ['default', 'fast'];
const repetitions = Number(options.repetitions ?? 3);
if (models.length > 4 || new Set(models).size !== models.length || tiers.length > 2 || new Set(tiers).size !== tiers.length
    || models.some(x => !names.includes(x)) || tiers.some(x => !['default', 'fast'].includes(x)) || !Number.isInteger(repetitions) || repetitions < 1 || repetitions > 5) throw new Error('invalid bounded matrix');
if (!options.output || !process.env.OPENAI_API_KEY) throw new Error('--output and OPENAI_API_KEY are required');
const key = process.env.OPENAI_API_KEY;
const prompt = 'Return only the sum of 17 and 25 as decimal digits.';
const instructions = 'Follow the user request precisely. Keep the final answer short.';
const file = resolve(options.output); mkdirSync(resolve(file, '..'), { recursive: true });
const workspace = mkdtempSync(join(tmpdir(), 'nanocodex-comparison-'));
const report = { version: 1, started_at: new Date().toISOString(), prompt, instructions, reasoning: 'low', models, tiers, repetitions, seed: 20260911, records: [] };
const save = () => writeFileSync(file, JSON.stringify(report, (name, value) => ['encrypted_content', 'obfuscation'].includes(name) ? '[omitted]' : value, 2) + '\n');
const sleep = ms => new Promise(r => setTimeout(r, ms));
const headers = agent => ({ Authorization: `Bearer ${key}`, 'Content-Type': 'application/json', ...(agent ? { 'OpenAI-Beta': 'agents=v1' } : {}) });
const safeError = error => String(error?.message ?? error).replaceAll(key, '[REDACTED]').slice(0, 800);
const bodyText = response => (response?.output ?? []).flatMap(i => i.content ?? []).filter(c => c.type === 'output_text').map(c => c.text).join('');
async function request(path, method = 'GET', body) {
  const r = await fetch('https://api.openai.com/v1' + path, { method, headers: headers(path.startsWith('/agents')), ...(body ? { body: JSON.stringify(body) } : {}), signal: AbortSignal.timeout(20_000) });
  const text = await r.text(); let data; try { data = text ? JSON.parse(text) : null; } catch { data = null; }
  return { status: r.status, data };
}
function observe(row, event, started) {
  const elapsed_ms = performance.now() - started;
  row.first_event_ms ??= elapsed_ms;
  row.events.push({ elapsed_ms, event });
  if (event.type?.endsWith('output_text.delta') && event.delta) { row.ttft_ms ??= elapsed_ms; row.text += event.delta; }
  if (event.type?.endsWith('output_text.done')) row.first_complete_text_ms ??= elapsed_ms;
  const sid = event.session?.id ?? event.session_id;
  if (sid && !row.session_id) { row.session_id = sid; save(); }
  if (event.response?.usage) row.usage = event.response.usage;
  if (event.response?.service_tier) row.reported_tier = event.response.service_tier;
  if (event.type === 'response.completed') row.final_text = bodyText(event.response);
  if (event.turn && !event.turn.subagent_id) {
    row.turn_id = event.turn.id;
    if (event.turn.usage) row.usage = event.turn.usage;
  }
  if (['response.completed', 'response.failed', 'response.incomplete', 'error', 'agent.session.failed', 'agent.session.requires_action'].includes(event.type)
      || ['agent.session.turn.completed', 'agent.session.turn.failed', 'agent.session.turn.cancelled'].includes(event.type) && !event.turn?.subagent_id) {
    row.terminal = event.type; row.completion_ms = elapsed_ms; return true;
  }
  return false;
}
async function stream(row, path, body) {
  const started = performance.now();
  const response = await fetch('https://api.openai.com/v1' + path, { method: 'POST', headers: headers(path.startsWith('/agents')), body: JSON.stringify(body), signal: AbortSignal.timeout(120_000) });
  row.headers_ms = performance.now() - started; row.status = response.status;
  row.request_id = response.headers.get('x-request-id');
  if (!response.ok || !response.headers.get('content-type')?.includes('text/event-stream')) {
    const data = await response.json(); if (data.id && path.startsWith('/agents')) row.session_id = data.id;
    row.error = safeError(data.error?.message ?? 'expected event stream'); return;
  }
  const reader = response.body.getReader(); const decoder = new TextDecoder(); let pending = '';
  try {
    for (;;) {
      const part = await reader.read(); if (part.done) break;
      pending += decoder.decode(part.value, { stream: true });
      let boundary;
      while ((boundary = pending.indexOf('\n\n')) >= 0) {
        const frame = pending.slice(0, boundary); pending = pending.slice(boundary + 2);
        const data = frame.split('\n').filter(l => l.startsWith('data:')).map(l => l.slice(5).trimStart()).join('\n');
        if (!data || data === '[DONE]') continue;
        if (observe(row, JSON.parse(data), started)) return;
      }
    }
    row.stream_closed_before_terminal = true;
  } finally { await reader.cancel().catch(() => {}); }
}
const sockets = new Map();
async function wsTrial(row) {
  const id = row.model + ':' + row.tier;
  let socket = sockets.get(id);
  row.connection_reused = socket?.readyState === WebSocket.OPEN;
  if (!row.connection_reused) {
    const start = performance.now(); socket = new WebSocket('wss://api.openai.com/v1/responses', { headers: { Authorization: `Bearer ${key}` }, handshakeTimeout: 20_000 });
    await new Promise((r, j) => { socket.once('open', r); socket.once('error', j); });
    row.connection_ms = performance.now() - start; sockets.set(id, socket);
  }
  const started = performance.now();
  await new Promise((resolve, reject) => {
    const finish = error => { clearTimeout(timer); socket.off('message', message); socket.off('close', closed); socket.off('error', errored); error ? reject(error) : resolve(); };
    const message = data => { try { if (observe(row, JSON.parse(String(data)), started)) finish(); } catch (e) { finish(e); } };
    const closed = () => finish(new Error('socket closed before completion'));
    const errored = error => finish(error);
    const timer = setTimeout(() => { socket.terminate(); finish(new Error('request deadline')); }, 120_000);
    socket.on('message', message); socket.on('close', closed); socket.on('error', errored);
    socket.send(JSON.stringify({ type: 'response.create', ...responseBody(row) }));
  });
}
const responseBody = row => ({ model: row.model, service_tier: row.tier, instructions, input: prompt, reasoning: { effort: 'low' }, max_output_tokens: 128, store: false });
async function nanoTrial(row) {
  const setupStart = performance.now(); let agent, watcher, unwatch, turn, result;
  try {
    agent = await Agent.create({ model: row.model, thinking: 'low', fastMode: row.tier === 'fast', additionalInstructions: instructions,
      workspace, tools: [], mcp: false, transport: Transport.openAi({ apiKey: key, websocketWarmup: false }) });
    row.agent_setup_ms = performance.now() - setupStart;
    const started = performance.now();
    watcher = agent.events.watch();
    unwatch = watcher.onEvent(event => {
      const elapsed_ms = performance.now() - started;
      row.events.push({ elapsed_ms, event });
      if (event.type === 'assistant.delta' && event.payload?.text) row.ttft_ms ??= elapsed_ms;
      if (event.type === 'model.connection.completed') row.connection_ms = event.payload.duration_ns / 1e6;
      if (event.type === 'api.event' && event.payload.direction === 'inbound') {
        row.first_event_ms ??= elapsed_ms;
        if (event.payload.event.response?.service_tier) row.reported_tier = event.payload.event.response.service_tier;
      }
      if (event.type === 'model.call.started') row.model_started_ms ??= elapsed_ms;
      if (event.type === 'model.call.completed') {
        row.model_calls ??= []; row.model_calls.push(event.payload);
        row.first_event_ms ??= (row.model_started_ms ?? 0) + event.payload.time_to_first_event_ns / 1e6;
      }
    });
    turn = agent.turn.prompt({ input: prompt });
    let timer;
    try { result = await Promise.race([turn.result(), new Promise((_, reject) => { timer = setTimeout(() => { void turn.cancel(); reject(new Error('request deadline')); }, 120_000); })]); }
    finally { clearTimeout(timer); }
    row.completion_ms = performance.now() - started; row.final_text = result.finalMessage; row.terminal = 'turn_completed';
    row.nano_usage = await result.usage();
    if (row.model_calls?.length === 1) row.usage = row.model_calls[0].usage;
    row.observed_setup_plus_completion_ms = performance.now() - setupStart;
  } finally {
    result?.dispose(); turn?.dispose(); unwatch?.(); watcher?.off();
    if (agent) { await agent.session.shutdown(); agent.dispose(); }
  }
}
async function finalizeSession(row) {
  if (!row.session_id) return;
  const base = '/agents/sessions/' + row.session_id;
  try {
    for (const delay of [0, 2000, 5000]) {
      if (delay) await sleep(delay);
      const turns = await request(base + '/turns?limit=100&order=desc');
      const turn = turns.data?.data?.find(t => !t.subagent_id && (!row.turn_id || t.id === row.turn_id));
      if (turn) { row.saved_turn = turn; row.turn_id = turn.id; if (turn.usage) { row.usage = turn.usage; break; } }
    }
    const items = await request(base + '/items?limit=100&order=asc'); row.saved_items = items.data;
    const messages = items.data?.data?.filter(i => i.type === 'message' && i.role === 'assistant') ?? [];
    const final = messages.filter(i => i.phase === 'final_answer').at(-1) ?? messages.at(-1);
    if (final) row.final_text = (final.content ?? []).filter(c => ['output_text', 'text'].includes(c.type)).map(c => c.text).join('');
  } catch (error) { row.metadata_error = safeError(error); }
  finally {
    row.cleanup = [];
    for (let attempt = 0; attempt < 4; attempt++) {
      const deletion = await request(base, 'DELETE'); row.cleanup.push({ status: deletion.status }); save();
      if ([200, 204, 404].includes(deletion.status)) return;
      if (deletion.status !== 409) break;
      await request(base + '/events', 'POST', { events: [{ type: 'agent.session.input.cancel' }] });
      await sleep(1000 * (attempt + 1));
    }
    throw new Error('session cleanup failed');
  }
}
let seed = report.seed;
const random = () => { seed = (1664525 * seed + 1013904223) >>> 0; return seed / 2 ** 32; };
const jobs = [];
for (let repetition = 1; repetition <= repetitions; repetition++) {
  const round = models.flatMap(model => tiers.flatMap(tier => ['responses_http', 'responses_ws', 'openai_agents', 'nanocodex_node'].map(path => ({ model, tier, path, repetition }))));
  for (let i = round.length - 1; i > 0; i--) { const j = Math.floor(random() * (i + 1)); [round[i], round[j]] = [round[j], round[i]]; }
  jobs.push(...round);
}
try {
  for (const job of jobs) {
    const row = { ...job, started_at: new Date().toISOString(), events: [], text: '', ttft_ms: null, usage: null };
    report.records.push(row); save(); const started = performance.now();
    try {
      if (row.path === 'responses_http') await stream(row, '/responses', { ...responseBody(row), stream: true });
      else if (row.path === 'responses_ws') await wsTrial(row);
      else if (row.path === 'nanocodex_node') await nanoTrial(row);
      else await stream(row, '/agents/sessions', { agent: { model: row.model, instructions, reasoning: { effort: 'low' }, service_tier: row.tier, tools: [] }, environment: { type: 'none' }, input: prompt, stream: true });
    } catch (error) { row.error = safeError(error); }
    row.measurement_wall_ms = performance.now() - started;
    try { await finalizeSession(row); } catch (error) { row.cleanup_error = safeError(error); save(); throw error; }
    row.correct = (row.final_text ?? row.text).trim() === '42';
    save(); console.log(JSON.stringify({ done: report.records.length, total: jobs.length, ...job, ttft_ms: row.ttft_ms, completion_ms: row.completion_ms, correct: row.correct, error: row.error }));
    if (row.error && /401|quota|authentication|permission/i.test(row.error)) throw new Error('access failure; stopping matrix');
  }
  report.complete = true;
} finally {
  for (const socket of sockets.values()) socket.close();
  rmSync(workspace, { recursive: true, force: true });
  report.finished_at = new Date().toISOString(); save();
}
