import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import test from 'node:test';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';

test('deployed large input completes, survives replay, and remains readable in both history directions', { timeout: 10 * 60_000 }, async () => {
  const key = process.env.NANOCODEX_DURABILITY_TEST_API_KEY;
  assert.ok(key, 'NANOCODEX_DURABILITY_TEST_API_KEY is required');
  const origin = process.env.NANOCODEX_MANAGED_URL ?? 'https://nanocodex.gakonst.workers.dev';
  const request = async (path, method = 'GET', body) => {
    const response = await fetch(origin + path, {
      method, headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(60_000),
    });
    assert.ok(response.ok, `${method} ${path}: HTTP ${response.status}`);
    return response.status === 204 ? undefined : response.json();
  };
  const created = await request('/v1/agents', 'POST', {
    settings: { model: 'gpt-5.6-luna', thinking: 'low', reasoning_mode: 'standard', fast_mode: false },
  });
  const base = `/v1/agents/${created.id ?? created.agent_id}`;
  const id = randomUUID();
  const marker = `LARGE_INPUT_${id.slice(0, 8)}`;
  // Low-token whitespace crosses HTTP and SQLite's old byte cutoffs without
  // manufacturing a prompt above the model's context window.
  const input = `This is a storage test. Reply exactly ${marker}. Use no tools.\n`
    + ' '.repeat(3 * 1024 * 1024) + `\nReply exactly ${marker}.`;
  console.info(`large input validation agent ${created.id ?? created.agent_id}; ${Buffer.byteLength(input)} bytes`);
  let passed = false;
  try {
    const accepted = await request(`${base}/turns`, 'POST', { id, input });
    const deadline = Date.now() + 7 * 60_000;
    for (;;) {
      const state = await request(base);
      if (!state.active_turns.includes(id)) break;
      assert.ok(Date.now() < deadline, 'large input did not settle');
      await delay(1000);
    }
    const completed = await request(`${base}/turns/${id}`);
    assert.equal(completed.state, 'completed');
    assert.equal(completed.input, input);
    assert.ok(completed.terminal.final_message.includes(marker));
    const replay = await request(`${base}/turns`, 'POST', { id, input });
    assert.equal(replay.accepted_cursor, accepted.accepted_cursor);
    assert.equal(replay.terminal_cursor, completed.terminal_cursor);
    assert.deepEqual(replay.terminal, completed.terminal);
    let after = '0';
    const events = [];
    for (;;) {
      const page = await request(`${base}/events/history?after=${after}&limit=128`);
      events.push(...page.data);
      if (!page.has_more) break;
      assert.notEqual(page.data.at(-1).cursor, after);
      after = page.data.at(-1).cursor;
    }
    const admission = events.find(event => event.type === 'turn_accepted' && (event.id ?? event.turn_id) === id);
    assert.equal(admission?.input, input);
    const latest = await request(`${base}/events/history?limit=128`);
    assert.ok(latest.data.some(event => event.type === 'turn_completed' && (event.id ?? event.turn_id) === id));
    if (process.env.NANOCODEX2_BINARY) {
      const binary = process.env.NANOCODEX2_BINARY;
      const binaryEnv = { ...process.env, NANOCODEX_API_KEY: key, NANOCODEX_MANAGED_URL: origin };
      await new Promise((resolve, reject) => {
        const child = spawn(binary, ['watch', created.id ?? created.agent_id, '--cursor', '0'], { env: binaryEnv });
        let pending = '', sawInput = false, settled = false;
        const finish = error => {
          if (settled) return;
          settled = true; clearTimeout(timer); child.kill('SIGTERM');
          error ? reject(error) : resolve();
        };
        const timer = setTimeout(() => finish(new Error('real binary did not replay the large frame')), 60_000);
        child.on('error', finish);
        child.on('exit', (code, signal) => { if (!settled) finish(new Error(`binary watch exited ${code ?? signal}`)); });
        child.stderr.resume();
        child.stdout.setEncoding('utf8');
        child.stdout.on('data', chunk => {
          pending += chunk;
          let boundary;
          while ((boundary = pending.indexOf('\n')) !== -1 && !settled) {
            const line = pending.slice(0, boundary); pending = pending.slice(boundary + 1);
            try {
              const event = JSON.parse(line);
              if (event.type === 'turn_accepted' && (event.id ?? event.turn_id) === id) {
                assert.equal(event.input, input); sawInput = true;
              }
              if (event.type === 'turn_completed' && (event.id ?? event.turn_id) === id) {
                assert.ok(sawInput, 'binary lost the oversized admission frame'); finish();
              }
            } catch (error) { finish(error); }
          }
        });
      });
      const result = await promisify(execFile)(binary, ['run', '--agent', created.id ?? created.agent_id,
        'Reply exactly BINARY_LARGE_HISTORY_OK. Use no tools.'], { env: binaryEnv, timeout: 300_000, maxBuffer: 64 * 1024 * 1024 });
      assert.match(result.stderr, /BINARY_LARGE_HISTORY_OK/);
      console.info('real nanocodex2 replayed the large SSE frame and completed its follow-up');
    }
    passed = true;
    console.info('3 MiB input: real completion, exact replay, and forward/backward history passed');
  } finally {
    if (passed) await request(base, 'DELETE');
    else console.info(`retained failed validation fixture ${base}`);
  }
});
