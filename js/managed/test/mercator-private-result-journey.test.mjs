import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';
import { build } from 'esbuild';
import { createMcpRuntime } from '../../nanocodex/runtime/mcp-runtime.mjs';

const JOB = '11111111-1111-4111-a111-111111111111';
const ORDINARY = '22222222-2222-4222-a222-222222222222';
const UNKNOWN = '33333333-3333-4333-a333-333333333333';
const FREE_JOB = '44444444-4444-4444-a444-444444444444';
const SECRET = 'synthetic-private-token-MUST-NOT-LEAK';
const PAN = '4242424242424242';
const LASO = 'x402-laso-finance-9ad65ae7';
const plan = { nodes: [{ id: 'issue', serviceId: LASO, method: 'GET', path: '/get-card', input: { amount: 5, format: 'json' } }] };
const context = { callId: 'synthetic', parentCallId: '', sessionId: 'synthetic', model: 'synthetic', signal: new AbortController().signal };
const issuance = { user_id: 'synthetic-laso-owner', auth: { id_token: SECRET, refresh_token: SECRET, expires_in: '3600' }, card: { card_id: 'synthetic-exact-card', status: 'pending' } };
const terminal = (jobId, serviceId, result) => ({ job: { jobId, status: 'succeeded', payments: { legs: [{ serviceId, nodeId: 'issue' }] }, result: { issue: result } } });
const wrapped = data => ({ structuredContent: data, content: [{ type: 'text', text: SECRET }, { type: 'resource', resource: { uri: 'private://secret', mimeType: 'text/plain', text: PAN } }], _meta: { secret: SECRET } });

// Real HTTP MCP transport; only Mercator and the private broker are synthetic.
// A shared HTTP broker implements versioned CAS so independent session stores compete.
test('Mercator issuance, history, recovery and pointer privacy through HTTP MCP', async () => {
  const temp = await mkdtemp(join(tmpdir(), 'mercator-guard-'));
  await build({ entryPoints: [new URL('../src/mercator-private-result.ts', import.meta.url).pathname], outfile: join(temp, 'guard.mjs'), platform: 'node', format: 'esm', bundle: true, logLevel: 'silent' });
  const { mercatorPrivateResult, mercatorPrivateStore } = await import(pathToFileURL(join(temp, 'guard.mjs')).href);
  const calls = [], captures = [], visible = [];
  let returnedCard = issuance;
  const records = new Map();
  const broker = async (url, body) => {
    if (url.endsWith('provider-bindings')) {
      if (body.operation === 'read') return Response.json(records.get(body.key) ?? { value: null, version: 0 });
      if (body.reads.some(({ key, version }) => (records.get(key)?.version ?? 0) !== version)) return Response.json({ error: 'provider_binding_conflict' }, { status: 409 });
      for (const { key, value } of body.writes) records.set(key, { value, version: (records.get(key)?.version ?? 0) + 1 });
      return Response.json({ committed: true });
    }
    captures.push({ url, body });
    if (url.endsWith('provider-capture')) { assert.equal(body.payload.card.card_id, 'synthetic-exact-card'); assert.deepEqual(body.source, { provider: 'laso', schema: 'laso-us-card-v1', request_id: `${JOB}/issue`, transport: 'defaultMCP:https://mercator.sh/mcp', owner_id: 'synthetic-owner', operation_id: body.operation_id, job_id: JOB, node_id: 'issue', card_id: 'synthetic-exact-card' }); return Response.json({ status: 'captured', capture_id: 'syntheticcapture0000000001' }); }
    return Response.json({ status: 'saved', vault_id: 'syntheticvault000000000001', last4: '4242', secret: SECRET });
  };
  const server = createServer(async (req, res) => {
    if (req.method !== 'POST') return void res.writeHead(405).end();
    let body = ''; for await (const chunk of req) body += chunk;
    if (req.url.startsWith('/broker/')) {
      const response = await broker(req.url, JSON.parse(body));
      res.writeHead(response.status, { 'content-type': 'application/json' }).end(await response.text()); return;
    }
    const rpc = JSON.parse(body);
    if (rpc.id === undefined) return void res.writeHead(202).end();
    let result;
    if (rpc.method === 'initialize') result = { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'synthetic', version: '1' } };
    if (rpc.method === 'tools/list') result = { tools: ['create_job', 'get_job', 'get_job_details', 'list_jobs'].map(name => ({ name, inputSchema: { type: 'object' } })) };
    if (rpc.method === 'tools/call') {
      const { name, arguments: args } = rpc.params; calls.push({ name, args });
      if (name === 'create_job') {
        if (args.idempotency_key === 'ambiguous-dispatch') {
          res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ jsonrpc: '2.0', id: rpc.id, error: { code: -32603, message: SECRET } })); return;
        }
        if (args.idempotency_key === 'prepayment-key') {
          result = wrapped(calls.filter(c => c.args.idempotency_key === 'prepayment-key').length === 1 ? { payment: { status: 'required' }, next_action: SECRET } : { job: { jobId: '55555555-5555-4555-a555-555555555555', status: 'pending' } });
        } else if (args.idempotency_key === 'management-key') {
          result = wrapped({ management: 'topUp', next_action: 'Top up the Mercator wallet.' });
        } else result = wrapped(terminal(JOB, LASO, issuance));
      } else if (name === 'get_job') result = wrapped(args.job_id === FREE_JOB ? { job: { jobId: FREE_JOB, status: 'succeeded', payments: { legs: [] }, replayPlan: { nodes: [{ id: 'issue', serviceId: 'free-search' }] }, result: { issue: { answer: 'free-result' } } } } : args.job_id === ORDINARY ? terminal(ORDINARY, 'ordinary-search', { answer: 'ordinary-result' }) : terminal(args.job_id, LASO, returnedCard));
      else if (name === 'get_job_details') result = wrapped({ jobId: ORDINARY, nodeId: 'issue', result: 'ordinary-result' });
      else result = wrapped({ jobs: [{ jobId: UNKNOWN, status: 'succeeded', services: [LASO], title: SECRET, result: issuance }, { jobId: ORDINARY, status: 'succeeded', services: ['ordinary-search'], title: 'search' }] });
    }
    res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ jsonrpc: '2.0', id: rpc.id, result }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const url = `http://127.0.0.1:${server.address().port}/mcp`;
  const binding = { fetch(target, init) { return fetch(`${url.replace('/mcp', '')}/broker/${new URL(target).pathname.split('/').at(-1)}`, init); } };
  let runtime;
  const open = async (owner = 'synthetic-owner') => { runtime = await createMcpRuntime({ mercator: { url, privateResult: mercatorPrivateResult({ owner, binding, store: mercatorPrivateStore(binding, owner), authorize() {} }) } }); await runtime.settled(); };
  const invoke = async (name, args) => { const tool = runtime.resolve(`mcp__mercator__${name}`); assert.ok(tool); const result = await tool.handler(args, context); visible.push(result); return JSON.stringify(result); };
  try {
    await open();
    assert.match(await invoke('get_job', { job_id: FREE_JOB, include_plan: true }), /free-result/);
    assert.match(await invoke('list_jobs', {}), new RegExp(UNKNOWN));
    const beforePointer = calls.length;
    assert.match(await invoke('get_job_details', { job_id: UNKNOWN, node_id: 'issue', result_pointer: '/auth/id_token' }), /Private MCP request failed/);
    assert.equal(calls.length, beforePointer);
    assert.match(await invoke('get_job', { job_id: UNKNOWN }), /mercator_private_job_unavailable/);
    assert.equal(captures.length, 0);
    assert.match(await invoke('get_job', { job_id: ORDINARY }), /ordinary-result/);
    assert.match(await invoke('get_job_details', { job_id: ORDINARY, node_id: 'issue' }), /ordinary-result/);
    const rejectedCalls = calls.length;
    assert.match(await invoke('create_job', { idempotency_key: 'unsafe-chain-key', plan: { nodes: [...plan.nodes, { id: 'exfiltrate', serviceId: 'ordinary-search', method: 'POST', path: '/', dependsOn: ['issue'] }] } }), /Private MCP request failed/);
    assert.equal(calls.length, rejectedCalls, 'Laso chained plan never dispatched');
    const issueArgs = { idempotency_key: 'synthetic-issue-key', approved_total: '5', plan };
    const peer = await createMcpRuntime({ mercator: { url, privateResult: mercatorPrivateResult({ owner: 'synthetic-owner', binding, store: mercatorPrivateStore(binding, 'synthetic-owner'), authorize() {} }) } });
    await peer.settled();
    const outcomes = await Promise.all([invoke('create_job', issueArgs), peer.resolve('mcp__mercator__create_job').handler({ ...issueArgs, plan: { nodes: [{ input: { format: 'json', amount: 5 }, path: '/get-card', method: 'GET', serviceId: LASO, id: 'issue' }] } }, { ...context, sessionId: 'independent-session' }).then(result => { visible.push(result); return JSON.stringify(result); })]);
    await peer.close();
    assert.equal(outcomes.filter(x => x.includes('saved')).length, 1);
    assert.equal(calls.filter(c => c.name === 'create_job').length, 1);
    assert.equal(captures.length, 2);
    assert.match(await invoke('create_job', issueArgs), /resume_existing_job/);
    assert.equal(calls.filter(c => c.name === 'create_job').length, 1);
    assert.match(await invoke('get_job', { job_id: JOB }), /saved/);
    const capturesBeforeMismatch = captures.length;
    returnedCard = { ...issuance, card: { card_id: 'wrong-card' } };
    assert.match(await invoke('get_job', { job_id: JOB }), /Private MCP request failed/);
    assert.equal(captures.length, capturesBeforeMismatch, 'exact card binding prevents changed provider card import');
    returnedCard = issuance;
    assert.match(await invoke('create_job', { ...issueArgs, idempotency_key: 'ambiguous-dispatch' }), /Private MCP request failed/);
    const chargedAttempts = calls.filter(c => c.name === 'create_job').length;
    await runtime.close(); await open();
    assert.match(await invoke('create_job', { ...issueArgs, idempotency_key: 'ambiguous-dispatch' }), /outcome_unknown/);
    assert.equal(calls.filter(c => c.name === 'create_job').length, chargedAttempts, 'restart does not repeat ambiguous dispatch');
    assert.match(await invoke('create_job', { ...issueArgs, idempotency_key: 'prepayment-key' }), /payment_required/);
    assert.match(await invoke('create_job', { ...issueArgs, idempotency_key: 'prepayment-key' }), /awaiting_card/);
    assert.equal(calls.filter(c => c.args.idempotency_key === 'prepayment-key').length, 2, 'explicit no-job prepayment outcome permits user retry');
    assert.match(await invoke('create_job', { idempotency_key: 'management-key', plan: { nodes: [{ id: 'search', serviceId: 'ordinary-search', method: 'GET', path: '/search' }] } }), /Top up the Mercator wallet/);
    await runtime.close(); await open('other-synthetic-owner');
    const otherOwnerCaptures = captures.length;
    assert.match(await invoke('get_job', { job_id: JOB }), /mercator_private_job_unavailable/);
    assert.equal(captures.length, otherOwnerCaptures, 'owner isolation prevents importing another owner job');
    for (const sentinel of [SECRET, PAN]) assert.equal(JSON.stringify(visible).includes(sentinel), false);
    console.log(JSON.stringify({ journey: 'managed Mercator HTTP guard', remoteCalls: calls.map(c => c.name), captureRequests: captures.length, visible, leakedSentinels: false }));
  } finally { await runtime?.close(); await new Promise(resolve => { server.close(resolve); server.closeAllConnections(); }); await rm(temp, { recursive: true, force: true }); }
});
