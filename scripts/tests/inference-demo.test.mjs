import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const script = fileURLToPath(new URL('../inference-demo.sh', import.meta.url));
const key = 'nci_SYNTHETIC_DEMO_TEST_SECRET';
const catalogue = { data: [
  { id: 'fixture-low', provider: 'fixture', model: 'canonical-model', thinking: 'low' },
  { id: 'fixture-high', provider: 'fixture', model: 'canonical-model', thinking: 'high' },
] };
const completed = {
  status: 'completed', model: 'canonical-model',
  route: { backend: 'fixture', model: 'canonical-model', provider_model: 'provider/model', thinking: 'low', family: 'code', confidence: 0.85, router_duration_ms: 23 },
  output: [{ type: 'message', content: [{ type: 'output_text', text: 'Fixture answer.' }] }],
  usage: { input_tokens: 21, output_tokens: 4, input_tokens_details: { cached_tokens: 2 }, output_tokens_details: { reasoning_tokens: 1 } },
};
async function fixture(t, respond = (_req, res) => res.end(JSON.stringify(completed))) {
  const requests = [];
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    requests.push({ method: req.method, path: req.url, headers: req.headers, body });
    res.setHeader('Content-Type', 'application/json');
    if (req.url === '/v1/models') res.end(JSON.stringify(catalogue));
    else await respond(req, res, body);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const dir = await mkdtemp(join(tmpdir(), 'inference-demo-test-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const run = (args = [], env = {}, bashArgs = []) => new Promise((resolve, reject) => {
    // /bin/bash is 3.2 on macOS; Linux also exercises the same portable script.
    const child = spawn('/bin/bash', [...bashArgs, script, '--no-color', ...args], {
      env: { ...process.env, NANOCODEX_INFERENCE_KEY: key, NANOCODEX_BASE_URL: `http://127.0.0.1:${server.address().port}/v1`, ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.on('error', reject);
    child.on('close', code => resolve({ code, stdout, stderr }));
  });
  return { requests, run, dir };
}

test('lists every candidate, runs exactly six stateless sequential prompts, and labels confidence/latency', async t => {
  let active = 0, maxActive = 0;
  const f = await fixture(t, async (_req, res) => {
    maxActive = Math.max(maxActive, ++active);
    await new Promise(resolve => setTimeout(resolve, 5));
    --active;
    res.end(JSON.stringify(completed));
  });
  const result = await f.run();
  assert.equal(result.code, 0, result.stderr);
  assert.equal(f.requests.length, 7);
  assert.equal(maxActive, 1);
  assert.match(result.stdout, /fixture-low/);
  assert.match(result.stdout, /fixture-high/);
  assert.match(result.stdout, /Classifier confidence: \[#{17}-{3}\] 85%/);
  assert.match(result.stdout, /Candidate confidence: unavailable/);
  assert.match(result.stdout, /HTTP TTFB is not model TTFT/);
  assert.match(result.stdout, /cached=2 reasoning=1/);
  for (const req of f.requests.slice(1)) {
    assert.equal(req.headers.authorization, `Bearer ${key}`);
    assert.deepEqual(Object.keys(JSON.parse(req.body)).sort(), ['input', 'max_output_tokens', 'model', 'store', 'stream']);
    assert.equal(JSON.parse(req.body).model, 'auto');
    assert.equal(JSON.parse(req.body).store, false);
    assert.equal(JSON.parse(req.body).stream, false);
  }
  assert.ok(!result.stdout.includes(key) && !result.stderr.includes(key));
});

test('models-only does not generate', async t => {
  const f = await fixture(t);
  const result = await f.run(['--models-only']);
  assert.equal(result.code, 0, result.stderr);
  assert.equal(f.requests.length, 1);
});

test('custom prompt options preserve order, JSON escaping, and sanitized private artifacts', async t => {
  const f = await fixture(t, (_req, res) => res.end(JSON.stringify({ ...completed,
    output_text: `\u001b[31mred\u001b[0m\u001b]0;evil-title\u0007 ${key}\u0008safe`,
  })));
  const promptsFile = join(f.dir, 'prompts.json');
  const output = join(f.dir, 'artifacts');
  await writeFile(promptsFile, JSON.stringify(['second\nline with "quotes" and \\slash', { label: 'Named', prompt: 'third' }]));
  const result = await f.run(['--prompt', 'first', '--prompts', promptsFile, '--model', 'fixture-high', '--max-output-tokens', '0042', '--output', output], {}, ['-x']);
  assert.equal(result.code, 0, result.stderr);
  assert.deepEqual(f.requests.slice(1).map(r => JSON.parse(r.body).input), ['first', 'second\nline with "quotes" and \\slash', 'third']);
  for (const r of f.requests.slice(1)) {
    assert.equal(JSON.parse(r.body).model, 'fixture-high');
    assert.equal(JSON.parse(r.body).max_output_tokens, 42);
  }
  assert.ok(!result.stdout.includes('\u001b'));
  assert.ok(!result.stdout.includes('evil-title'));
  assert.ok(!result.stdout.includes(key) && !result.stderr.includes(key));
  assert.match(result.stdout, /red \[REDACTED\]safe/);
  for (const file of await readdir(output)) {
    const data = await readFile(join(output, file), 'utf8');
    assert.ok(!data.includes(key), file);
    assert.doesNotThrow(() => JSON.parse(data));
  }
  assert.equal(JSON.parse(await readFile(join(output, 'summary.json'), 'utf8')).length, 3);
});

test('429 reports Retry-After and stops with no retries', async t => {
  const f = await fixture(t, (_req, res) => {
    res.writeHead(429, { 'Retry-After': '37' });
    res.end(JSON.stringify({ error: { message: 'slow down', code: 'rate_limit' } }));
  });
  const result = await f.run();
  assert.equal(result.code, 1, result.stderr);
  assert.equal(f.requests.length, 2);
  assert.match(result.stdout, /Retry-After: 37/);
  assert.match(result.stdout, /remaining prompts were not sent/);
  assert.match(result.stdout, /Summary/);
});

test('malformed JSON is a visible failure with a summary', async t => {
  const f = await fixture(t, (_req, res) => res.end('<html>broken</html>'));
  const result = await f.run(['--prompt', 'one']);
  assert.equal(result.code, 1, result.stderr);
  assert.match(result.stdout, /Malformed JSON/);
  assert.match(result.stdout, /invalid-json/);
  assert.equal(f.requests.length, 2);
});

test('timeout explicitly reports ambiguous outcome without retrying', async t => {
  const f = await fixture(t, () => {});
  const result = await f.run(['--prompt', 'one', '--timeout', '1']);
  assert.equal(result.code, 1, result.stderr);
  assert.match(result.stdout, /generation\/billing outcome is unknown/);
  assert.equal(f.requests.length, 2);
});

test('rejects invalid arguments and missing credentials before sending', async t => {
  const f = await fixture(t);
  for (const args of [['--max-output-tokens', '0'], ['--timeout', '-1'], ['--prompt'], ['--unknown']]) {
    assert.equal((await f.run(args)).code, 2);
  }
  const noKey = await f.run(['--models-only'], { NANOCODEX_INFERENCE_KEY: '' });
  assert.equal(noKey.code, 2);
  assert.match(noKey.stderr, /Set NANOCODEX_INFERENCE_KEY/);
  const help = await f.run(['--help'], { NANOCODEX_INFERENCE_KEY: '' });
  assert.equal(help.code, 0);
  assert.equal(f.requests.length, 0);
});

test('renders all real Jev distributions without confusing candidate and family confidence', async t => {
  const f = await fixture(t, (_req, res) => res.end(JSON.stringify({ ...completed, route: {
    ...completed.route, confidence: 0,
    diagnostics: { source: 'typesafe/jev', family_confidence: 0.73, candidate_confidence: 0.61,
      eligible_candidates: ['fixture-low', 'fixture-high'], proposed_candidate: 'fixture-low', chosen_candidate: 'fixture-high',
      candidate_probabilities: { 'fixture-low': 0.61, 'fixture-high': 0.39 },
      family_probabilities: { code: 0.73, other: 0.27 }, min_confidence: 0.7, confidence_status: 'low', fallback_basis: 'eligible_frontier',
    },
  } })));
  const result = await f.run(['--prompt', 'one']);
  assert.equal(result.code, 0, result.stderr);
  assert.match(result.stdout, /Classifier confidence: \[#{14}-{6}\] 73%/);
  assert.match(result.stdout, /Candidate confidence \(selector\): \[#{12}-{8}\] 61%/);
  assert.match(result.stdout, /\+ fixture-low +\[#{12}-{8}\] 61%/);
  assert.match(result.stdout, /\*  fixture-high +\[#{7}-{13}\] 39%/);
  assert.match(result.stdout, /other +\[#{5}-{15}\] 27%/);
  assert.equal(JSON.parse(f.requests[1].body).max_output_tokens, 2048);
});

test('invalid diagnostics confidence stays unavailable even when legacy route confidence is zero', async t => {
  const f = await fixture(t, (_req, res) => res.end(JSON.stringify({ ...completed, route: {
    ...completed.route, confidence: 0,
    diagnostics: { family_confidence: null, candidate_confidence: null, candidate_probabilities: null, family_probabilities: null,
      eligible_candidates: ['fixture-low', 'fixture-high'], proposed_candidate: null, chosen_candidate: 'fixture-high',
      min_confidence: 0.7, confidence_status: 'unavailable_or_invalid', fallback_basis: 'eligible_frontier',
    },
  } })));
  const result = await f.run(['--prompt', 'one']);
  assert.equal(result.code, 0, result.stderr);
  assert.match(result.stdout, /Classifier confidence: unavailable/);
  assert.match(result.stdout, /Candidate confidence \(selector\): unavailable/);
  assert.match(result.stdout, /fixture-low +unavailable/);
  assert.match(result.stdout, /fixture-high +unavailable/);
  assert.doesNotMatch(result.stdout, /\] 0%/);
});

test('unexpected JSON shapes do not break the final summary', async t => {
  const f = await fixture(t, (_req, res) => res.end(JSON.stringify({ status: 'failed', route: 'bad', usage: 'bad', output: [null, 'bad', { content: [null, 'bad'] }] })));
  const result = await f.run(['--prompt', 'one']);
  assert.equal(result.code, 1, result.stderr);
  assert.match(result.stdout, /Summary/);
  assert.match(result.stdout, /no text output returned/);
});
