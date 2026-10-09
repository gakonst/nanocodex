#!/usr/bin/env node
// Public HTTP, live inference. Credentials enter curl only through stdin.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { fileURLToPath } from 'node:url';

const origin = process.env.NANOCODEX_ORIGIN;
const token = process.env.NANOCODEX_API_KEY;
assert.ok(origin && token, 'Set NANOCODEX_ORIGIN and NANOCODEX_API_KEY; this creates synthetic sessions and uses live inference.');
const url = new URL(origin);
assert.ok(url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)), 'HTTPS origin required except loopback');
assert.ok(url.pathname === '/' && !url.search && !url.hash && !url.username && !url.password, 'Supply only an origin');
assert.ok(!/[\r\n]/.test(token), 'Invalid credential');
const samples = Number(process.env.NANOCODEX_LATENCY_SAMPLES ?? 3);
assert.ok(Number.isInteger(samples) && samples >= 1 && samples <= 100, 'NANOCODEX_LATENCY_SAMPLES must be 1..100');
const settings = { model: process.env.NANOCODEX_TEST_MODEL ?? 'gpt-6.1-sol', thinking: 'low', reasoning_mode: 'standard', fast_mode: false };
const root = fileURLToPath(new URL('../../../', import.meta.url));
const run = randomUUID();
const output = resolve(process.env.NANOCODEX_LATENCY_OUTPUT ?? resolve(root, 'output/managed-api-latency'), run);
await mkdir(output, { recursive: true });
const source = await readFile(fileURLToPath(import.meta.url));
const git = args => { try { return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim(); } catch { return null; } };
await writeFile(`${output}/run.json`, JSON.stringify({ run, origin, settings, samples, started_at: new Date().toISOString(), node: process.version,
  curl: execFileSync('curl', ['--version'], { encoding: 'utf8' }).split('\n')[0], source_sha: git(['rev-parse', 'HEAD']), source_status: git(['status', '--short']),
  script_sha256: createHash('sha256').update(source).digest('hex'), command: 'node js/managed/scripts/curl-api-latency.mjs',
  methodology: 'Real public HTTP curl and live inference. Stopwatch begins before curl spawn; admission is first complete run frame; first text is first nonempty current-turn assistant.delta.text; total ends at curl EOF. Fresh session does not imply cold Worker. Reconnect text may be replay. Repository SHA does not establish deployed version. No automatic retries after uncertainty.',
}, null, 2));
await writeFile(`${output}/harness.mjs`, source);
console.log(JSON.stringify({ output, run }));
const rows = [];
let sequence = 0;
const expectedText = 'SYNTHETIC_BASELINE_OK';
const input = `Synthetic latency measurement. Do not call tools, inspect context, or take actions. Reply with exactly ${expectedText}.`;

async function request(label, path, { body, key = randomUUID(), expected = 200, disconnect = false, stream = false } = {}) {
  assert.ok(path.startsWith('/v1/'));
  const base = `${output}/${String(++sequence).padStart(3, '0')}-${label}`;
  if (body !== undefined) await writeFile(`${base}.request.json`, JSON.stringify(body));
  const args = ['--silent', '--show-error', '--no-buffer', '--max-time', '120', '--dump-header', '-', '--config', '-',
    '-H', 'Content-Type: application/json', '-H', `Idempotency-Key: ${key}`, ...(stream ? ['-H', 'Accept: text/event-stream'] : []),
    ...(body === undefined ? [] : ['--request', 'POST', '--data-binary', `@${base}.request.json`]), new URL(path, origin).href];
  const started_at = new Date().toISOString(), start = performance.now();
  // Persist the intended identity before dispatch so a lost response can be reconciled.
  await writeFile(`${base}.request-meta.json`, JSON.stringify({ label, path, key, started_at, curl_argv: args, authorization: 'Bearer from stdin (not recorded)' }, null, 2));
  const child = spawn('curl', args, { stdio: ['pipe', 'pipe', 'pipe'] });
  const decoder = new StringDecoder('utf8');
  let raw = '', buffer = '', headerBuffer = '', headers = '', stderr = '', readingHeaders = true;
  let parseError, receipt, status, disconnected = false, admission_ms = null, first_text_ms = null, terminal_ms = null;
  const frames = [];
  function consume(text) {
    if (parseError) return;
    try {
      if (readingHeaders) {
        headerBuffer += text;
        while (true) {
          const boundary = /\r?\n\r?\n/.exec(headerBuffer);
          if (!boundary) return;
          const block = headerBuffer.slice(0, boundary.index + boundary[0].length);
          headerBuffer = headerBuffer.slice(block.length);
          headers += block;
          status = Number(block.match(/^HTTP\/\S+ (\d+)/)?.[1]);
          if (status < 200 || /^HTTP\/\S+ 200 Connection established/i.test(block)) continue;
          readingHeaders = false;
          text = headerBuffer;
          headerBuffer = '';
          break;
        }
      }
      raw += text;
      assert.ok(Buffer.byteLength(raw) <= 16 * 1024 * 1024, 'Response exceeded 16 MiB evidence limit');
      if (!stream) return;
      buffer += text;
      let boundary;
      while ((boundary = /\r?\n\r?\n/.exec(buffer))) {
        const frame = buffer.slice(0, boundary.index);
        buffer = buffer.slice(boundary.index + boundary[0].length);
        const lines = frame.split(/\r?\n/);
        const data = lines.filter(line => line.startsWith('data:')).map(line => line.slice(5).replace(/^ /, '')).join('\n');
        if (!data) continue;
        const event = lines.find(line => line.startsWith('event:'))?.slice(6).trim();
        const value = JSON.parse(data), at_ms = performance.now() - start;
        frames.push({ at_ms, event, value });
        if (event === 'run') {
          assert.ok(!receipt, 'Only one admission receipt per stream');
          receipt = value; admission_ms = at_ms;
          console.log(JSON.stringify({ label, started_at, admission_ms, session_id: value.session_id, turn_id: value.turn_id }));
          if (disconnect) { disconnected = true; child.kill('SIGTERM'); }
        }
        if (value.turn_id === receipt?.turn_id && value.event?.type === 'assistant.delta' && value.event.payload?.text) first_text_ms ??= at_ms;
        if (value.turn_id === receipt?.turn_id && ['turn_completed', 'turn_failed', 'turn_cancelled'].includes(value.type)) terminal_ms ??= at_ms;
      }
    } catch (error) { parseError = error; child.kill('SIGTERM'); }
  }
  child.stdout.on('data', bytes => consume(decoder.write(bytes)));
  child.stdout.on('end', () => consume(decoder.end()));
  child.stderr.on('data', bytes => { stderr += bytes; });
  child.stdin.on('error', () => {});
  child.stdin.end(`header = ${JSON.stringify(`Authorization: Bearer ${token}`)}\n`);
  const result = await new Promise(resolveResult => {
    child.once('error', error => { parseError = error; });
    child.once('close', (code, signal) => resolveResult({ code, signal }));
  });
  const total_ms = performance.now() - start, ended_at = new Date().toISOString();
  const headerValues = name => [...headers.matchAll(new RegExp(`^${name}:\\s*(.+)$`, 'gim'))].map(match => match[1].trim());
  const row = { label, path, key, started_at, ended_at, status, ...result, admission_ms, first_text_ms, terminal_ms, total_ms, receipt, disconnected,
    server_timing: headerValues('server-timing'), request_ids: Object.fromEntries(['cf-ray', 'x-request-id', 'request-id', 'x-nanocodex-request-id', 'x-nanocodex-turn-id'].map(name => [name, headerValues(name)])), evidence: base };
  rows.push(row);
  // Avoid recording a credential even if a misconfigured origin echoes it.
  const redact = text => text.replaceAll(token, '[REDACTED]');
  await Promise.all([
    writeFile(`${base}.headers`, redact(headers)), writeFile(`${base}.response`, redact(raw)), writeFile(`${base}.stderr`, redact(stderr)),
    writeFile(`${base}.frames.json`, redact(JSON.stringify(frames, null, 2))), writeFile(`${base}.receipt.json`, redact(JSON.stringify(row, null, 2))),
    writeFile(`${output}/summary.json`, redact(JSON.stringify(rows, null, 2))),
  ]);
  console.log(JSON.stringify({ label, status, admission_ms, first_text_ms, total_ms }));
  if (parseError) throw parseError;
  assert.equal(status, expected, `${label}: unexpected HTTP status; inspect ${base}.response`);
  if (!disconnected) assert.equal(result.code, 0, `${label}: curl did not finish; reconcile the recorded identity before retrying`);
  if (stream) {
    assert.ok(receipt?.agent_id && receipt?.turn_id, `${label}: missing run receipt`);
    assert.equal(frames.filter(f => f.value.event?.type === 'tool.call').length, 0, `${label}: unexpected tool call`);
    if (!disconnected) {
      assert.equal(buffer.trim(), '', `${label}: incomplete SSE frame`);
      const current = frames.filter(f => f.value.turn_id === receipt.turn_id);
      assert.equal(current.filter(f => f.value.type === 'turn_completed').length, 1, `${label}: completion missing or duplicated`);
      assert.equal(current.filter(f => ['turn_failed', 'turn_cancelled'].includes(f.value.type)).length, 0);
      const text = current.filter(f => f.value.event?.type === 'assistant.delta').map(f => f.value.event.payload.text ?? '').join('');
      assert.equal(text, expectedText, `${label}: exact current-turn output`);
      assert.notEqual(first_text_ms, null, `${label}: first text missing`);
    }
  }
  return { row, receipt, frames, raw };
}

try {
  for (let index = 0; index < samples; index++) {
    const fresh = await request(`fresh-${index}`, '/v1/agent-runs', { body: { input, settings }, stream: true, expected: 201 });
    await request(`warm-${index}`, `/v1/agents/${fresh.receipt.agent_id}/turns`, { body: { input }, stream: true, expected: 202 });
  }
  const key = randomUUID(), body = { input, settings };
  const detached = await request('disconnect', '/v1/agent-runs', { body, key, stream: true, expected: 201, disconnect: true });
  assert.ok(detached.row.disconnected, 'Actually disconnected after admission');
  const replay = await request('retry-same-key', '/v1/agent-runs', { body, key, stream: true });
  assert.equal(detached.receipt.agent_id, replay.receipt.agent_id);
  assert.equal(detached.receipt.turn_id, replay.receipt.turn_id);
  await request('changed-input-conflict', '/v1/agent-runs', { body: { ...body, input: input + ' changed' }, key, expected: 409 });
  const followup = await request('subsequent-work', `/v1/agents/${replay.receipt.agent_id}/turns`, { body: { input }, stream: true, expected: 202 });
  const history = [];
  let cursor = '0', complete = false;
  for (let page = 0; page < 32; page++) {
    const response = await request(`history-${page}`, `/v1/agents/${replay.receipt.agent_id}/events/history?after=${cursor}&limit=16`);
    const value = JSON.parse(response.raw);
    assert.ok(Array.isArray(value.data));
    for (const event of value.data) {
      assert.ok(BigInt(event.cursor) > BigInt(cursor), 'History cursors advance without duplicates');
      cursor = String(event.cursor); history.push(event);
    }
    if (!value.has_more) { complete = true; break; }
    assert.ok(value.data.length, 'Paginated history must advance');
  }
  assert.ok(complete, 'History exceeded 512-event evidence bound');
  for (const turn of [replay.receipt.turn_id, followup.receipt.turn_id]) {
    assert.equal(history.filter(event => event.turn_id === turn && event.type === 'turn_completed').length, 1, 'One retained completion per admitted turn');
  }
  const groups = Object.fromEntries(['fresh', 'warm'].map(prefix => {
    const group = rows.filter(row => row.label.startsWith(prefix + '-'));
    return [prefix, Object.fromEntries(['admission_ms', 'first_text_ms', 'total_ms'].map(field => {
      const values = group.map(row => row[field]).sort((a, b) => a - b);
      return [field, { n: values.length, min: values[0], median: (values[Math.floor((values.length - 1) / 2)] + values[Math.floor(values.length / 2)]) / 2, max: values.at(-1) }];
    }))];
  }));
  const assertions = { passed: true, samples, disconnect_same_agent_and_turn: true, changed_input_conflict: true, subsequent_work_completed: true,
    exact_text_and_no_tools: true, history_complete: true, history_events: history.length, groups, completed_at: new Date().toISOString() };
  await writeFile(`${output}/assertions.json`, JSON.stringify(assertions, null, 2));
  console.log(JSON.stringify(assertions));
} catch (error) {
  await writeFile(`${output}/failure.txt`, String(error.stack ?? error).replaceAll(token, '[REDACTED]'));
  console.error(`Journey failed; inspect ${output}/failure.txt. Reconcile saved request identities before retrying.`);
  process.exitCode = 1;
}
