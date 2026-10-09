import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';

// Only transport fragmentation/malformed-response injection happens here. All
// admission, idempotency, provider execution and subsequent work run in workerd.
export async function verifyLargeRunReceipt({ server, output, settings, providerCalls, sdk }) {
  let damage = false, sequence = 0;
  const transport = [], checks = [];
  const proxy = createServer(async (request, response) => {
    const controller = new AbortController();
    response.on('close', () => controller.abort());
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const headers = { ...request.headers }; delete headers.host; delete headers['content-length'];
      const upstream = await fetch(new URL(request.url, server.base), {
        method: request.method, headers, signal: controller.signal,
        ...(['GET', 'HEAD'].includes(request.method) ? {} : { body: Buffer.concat(chunks) }),
      });
      const outgoing = Object.fromEntries(upstream.headers);
      delete outgoing['content-length']; delete outgoing['transfer-encoding']; delete outgoing['content-encoding'];
      response.writeHead(upstream.status, outgoing);
      if (request.url === '/v1/agent-runs' && upstream.headers.get('content-type')?.includes('text/event-stream')) {
        let bytes = Buffer.from(await upstream.arrayBuffer());
        const index = ++sequence;
        await writeFile(join(output, `sdk-upstream-${index}.sse`), bytes);
        if (damage) {
          const boundary = bytes.indexOf('\n\n');
          const receipt = JSON.parse(bytes.subarray(0, boundary).toString().split('\ndata: ')[1]);
          receipt.unexpected_padding = 'x'.repeat(2 * 1024 * 1024);
          bytes = Buffer.concat([Buffer.from('event: run\ndata: ' + JSON.stringify(receipt) + '\n\n'), bytes.subarray(boundary + 2)]);
        }
        const record = { index, status: upstream.status, bytes: bytes.length, damaged: damage, chunks: 0 };
        transport.push(record);
        for (let offset = 0; offset < bytes.length && !controller.signal.aborted; offset += 16 * 1024) {
          response.write(bytes.subarray(offset, offset + 16 * 1024)); record.chunks++;
          await delay(1);
        }
      } else if (upstream.body) {
        for await (const chunk of upstream.body) {
          if (!response.write(chunk)) await once(response, 'drain', { signal: controller.signal });
        }
      }
      response.end();
    } catch (error) { if (!controller.signal.aborted) response.destroy(error); }
  });
  proxy.listen(0, '127.0.0.1'); await once(proxy, 'listening');
  const origin = `http://127.0.0.1:${proxy.address().port}`;
  const run = async (command, args, options = {}, input) => {
    const child = spawn(command, args, { timeout: 120_000, killSignal: 'SIGKILL', ...options, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    child.stdout.on('data', value => stdout += value); child.stderr.on('data', value => stderr += value);
    child.stdin.end(input);
    const [code, signal] = await once(child, 'close');
    return { code, signal, stdout, stderr };
  };
  const curl = async (label, body, key, streaming) => {
    const requestPath = join(output, `${label}.request.json`);
    await writeFile(requestPath, JSON.stringify(body));
    const args = ['--silent', '--show-error', '-N', '--max-time', '60', '-D', join(output, `${label}.headers`), '-H', `Authorization: Bearer ${server.token}`, '-H', 'Content-Type: application/json', '-H', `Accept: ${streaming ? 'text/event-stream' : 'application/json'}`, '-H', `Idempotency-Key: ${key}`, '--data-binary', '@-', new URL('/v1/agent-runs', server.base).href];
    const result = await run('curl', args, {}, JSON.stringify(body));
    await writeFile(join(output, `${label}.response`), result.stdout);
    assert.equal(result.code, 0, result.stderr);
    const headers = await readFile(join(output, `${label}.headers`), 'utf8');
    assert.match(headers, /^HTTP\/\S+ 200/m, headers);
    return streaming ? JSON.parse(result.stdout.split('\n\n')[0].split('\ndata: ')[1]) : JSON.parse(result.stdout);
  };
  const probe = async (label, input, key, reject) => {
    const path = join(output, `${label}.sdk.json`);
    // This fixture key has no authority outside this disposable local runtime.
    await writeFile(path, JSON.stringify({ origin, api_key: server.token, input, key, settings, reject }));
    const result = await run(sdk, ['worker_receipt::production_worker_receipt', '--exact', '--ignored', '--nocapture'], { env: { ...process.env, NANOCODEX_RECEIPT_JOURNEY: path } });
    await writeFile(join(output, `${label}.sdk.log`), result.stdout + result.stderr);
    return result;
  };
  try {
    const input = 'BENCH_SAMPLE_large_receipt: synthetic large prompt\n' + ' '.repeat(2 * 1024 * 1024) + '\nUnicode: λ 😀; escaped control:\t';
    const key = randomUUID(), before = providerCalls.length;
    const result = await probe('large-fresh', input, key, false);
    // Always reconcile a failed SDK call with its original key, including the
    // red run. The admitted turn must remain readable and execute only once.
    const receipt = await curl('large-recovery-json', { input, settings }, key, false);
    const replay = await curl('large-replay-sse', { input, settings }, key, true);
    assert.equal(receipt.input, input); assert.equal(replay.turn_id, receipt.turn_id);
    assert.equal(replay.agent_id, receipt.agent_id);
    assert.equal(providerCalls.length - before, result.code === 0 ? 2 : 1, 'one initial inference, plus SDK followup only after successful parse');
    checks.push({ scenario: 'large fresh/recovery/replay', agent: receipt.agent_id, turn: receipt.turn_id, sdk_code: result.code, provider_calls: providerCalls.length - before });
    assert.equal(result.code, 0, 'large fresh SDK failed; exact-key curl recovered its admitted turn: ' + result.stdout + result.stderr);
    assert.ok(result.stdout.includes(receipt.agent_id));

    damage = true;
    const small = 'BENCH_SAMPLE_receipt_bound: small valid prompt', smallKey = randomUUID(), secondBefore = providerCalls.length;
    const rejected = await probe('oversized-response', small, smallKey, true);
    assert.equal(rejected.code, 0, rejected.stdout + rejected.stderr);
    damage = false;
    const recovered = await curl('bounded-recovery-json', { input: small, settings }, smallKey, false);
    const usable = await probe('bounded-replay', small, smallKey, false);
    assert.equal(usable.code, 0, usable.stdout + usable.stderr);
    assert.equal(providerCalls.length - secondBefore, 2, 'malformed receipt recovery never repeats initial inference and permits followup');
    assert.ok(usable.stdout.includes(recovered.agent_id));
    checks.push({ scenario: 'bounded response/recovery/followup', agent: recovered.agent_id, turn: recovered.turn_id, provider_calls: 2 });
  } finally {
    proxy.closeAllConnections(); await new Promise(resolve => proxy.close(resolve));
    await writeFile(join(output, 'receipt-checks.json'), JSON.stringify({ checks, transport }, null, 2));
  }
}
