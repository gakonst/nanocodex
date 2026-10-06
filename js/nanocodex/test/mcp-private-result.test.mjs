import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { createMcpRuntime } from '../runtime/mcp-runtime.mjs';
import { createCodeRuntime } from '../runtime/code-runtime.mjs';
import { mcpPayment } from '../tempo/index.mjs';
import { Method } from 'mppx';
import { Methods } from 'mppx/tempo';

const secrets = ['synthetic-private-token-8c4a', '4242424242424242'];
const privateBody = secrets.join(' ');

// Actual Streamable HTTP transport: no client/transport mocks. The synthetic
// remote sends synthetic private results and unsolicited notifications. No
// notification consumer is configured; notification silence is not a leak fix.
async function fixture({ failDiscovery = false } = {}) {
  const calls = [];
  const server = createServer(async (req, res) => {
    if (req.method !== 'POST') { res.writeHead(405).end(); return; }
    let body = '';
    for await (const chunk of req) body += chunk;
    const rpc = JSON.parse(body);
    if (rpc.id === undefined) { res.writeHead(202).end(); return; }
    let result;
    if (rpc.method === 'initialize') {
      result = { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'synthetic', version: '1' } };
    } else if (rpc.method === 'tools/list') {
      if (failDiscovery) {
        res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ jsonrpc: '2.0', id: rpc.id, error: { code: -32603, message: privateBody } }));
        return;
      }
      result = { tools: [{ name: 'capture', description: 'Capture private synthetic data', inputSchema: { type: 'object' } }] };
    } else if (rpc.method === 'tools/call') {
      calls.push(rpc.params.arguments);
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      for (const method of ['notifications/message', 'notifications/progress', 'notifications/private']) {
        res.write(`event: message\ndata: ${JSON.stringify({ jsonrpc: '2.0', method, params: { level: 'error', data: privateBody, progressToken: 99999, progress: 1, message: privateBody } })}\n\n`);
      }
      if (rpc.params.arguments.mode === 'drop') {
        res.flushHeaders();
        setImmediate(() => res.destroy());
        return;
      }
      const response = rpc.params.arguments.mode === 'throw'
        ? { error: { code: -32603, message: privateBody, data: { secret: privateBody } } }
        : { result: { content: [{ type: 'text', text: privateBody }], structuredContent: { token: secrets[0], pan: secrets[1] }, _meta: { private: privateBody }, ...(rpc.params.arguments.mode === 'error-result' ? { isError: true } : {}) } };
      res.end(`event: message\ndata: ${JSON.stringify({ jsonrpc: '2.0', id: rpc.id, ...response })}\n\n`);
      return;
    }
    res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ jsonrpc: '2.0', id: rpc.id, result }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return { calls, url: `http://127.0.0.1:${server.address().port}/mcp`, close: () => new Promise(resolve => { server.close(resolve); server.closeAllConnections(); }) };
}

test('private MCP HTTP interception precedes model projections and tracing, including transport loss', async () => {
  const remote = await fixture();
  const transcript = [];
  const captures = [];
  const traces = [];
  let concurrentPreflights = 0;
  let releaseConcurrent;
  const bothPreflights = new Promise(resolve => { releaseConcurrent = resolve; });
  const failedReceipt = { isError: true, content: [{ type: 'text', text: 'Private MCP request failed' }] };
  const consoleMethods = ['log', 'warn', 'error', 'info', 'debug'];
  const originalConsole = Object.fromEntries(consoleMethods.map(name => [name, console[name]]));
  for (const name of consoleMethods) console[name] = (...args) => transcript.push({ console: name, args });
  let mcp;
  try {
    mcp = await createMcpRuntime({ private: { url: remote.url, timeoutMs: 1000, privateResult: {
      async beforeCall({ name, arguments: input }, context) {
        assert.equal(name, 'capture');
        if (['paid', 'unknown-job'].includes(input.mode)) throw new Error(privateBody);
        const state = { callId: context.callId, mode: input.mode, secret: secrets[0] };
        if (input.mode === 'concurrent') {
          if (++concurrentPreflights === 2) releaseConcurrent();
          await bothPreflights;
        }
        if (input.mode === 'replay') {
          return { result: { content: [{ type: 'text', text: 'Saved securely' }], structuredContent: { capture_id: 'synthetic-capture' } } };
        }
        return { privateContext: state };
      },
      transformResult({ name, arguments: input, result, privateContext }, context) {
        assert.equal(name, 'capture');
        assert.notEqual(input.mode, 'replay', 'safe replay bypasses raw-result transformation');
        assert.ok(context);
        assert.deepEqual(privateContext, { callId: context.callId, mode: input.mode, secret: secrets[0] });
        assert.ok(JSON.stringify(result).includes(secrets[0]));
        captures.push(result);
        if (input.mode === 'transform-fail') throw new Error(privateBody);
        return { content: [{ type: 'text', text: 'Saved securely' }], structuredContent: { capture_id: 'synthetic-capture' } };
      },
    } } });
    await mcp.settled();
    const runtime = createCodeRuntime({}, {
      async traceTool(name, context, run) {
        try {
          const result = await run();
          traces.push({ name, callId: context.callId, result });
          return result;
        } catch (error) {
          traces.push({ name, error: { message: error.message, stack: error.stack, cause: error.cause } });
          throw error;
        }
      },
    });
    runtime.addProvider(mcp);
    transcript.push(JSON.parse(await runtime.executeTool('tool_search', JSON.stringify({ query: 'capture' }))));
    for (const mode of ['ok', 'error-result', 'throw', 'transform-fail', 'paid', 'unknown-job', 'drop', 'ok-after-drop', 'replay']) {
      const direct = JSON.parse(await runtime.executeTool('mcp__private__capture', JSON.stringify({ mode }), 'private-session', `direct-${mode}`));
      transcript.push(direct);
      assert.equal(direct.success, ['ok', 'error-result', 'ok-after-drop', 'replay'].includes(mode));
      if (!direct.success) {
        assert.deepEqual(JSON.parse(direct.output), failedReceipt);
        assert.deepEqual(direct.structured_result, failedReceipt);
      }
      const nested = JSON.parse(await runtime.executeCode(`
        const found = await tools.tool_search({query: "capture"});
        try { text(await tools[found.tools[0].name]({mode: ${JSON.stringify(mode)}})); }
        catch (error) { text(error); }
      `, 'private-session', `nested-${mode}`));
      transcript.push(nested);
      assert.equal(nested.success, true);
      assert.equal(nested.nested_calls.find(call => call.name === 'mcp__private__capture' || call.tool === 'mcp__private__capture')?.success ?? nested.nested_calls.at(-1).success, ['ok', 'error-result', 'ok-after-drop', 'replay'].includes(mode));
    }
    const concurrent = await Promise.all(['a', 'b'].map(id => runtime.executeTool(
      'mcp__private__capture', JSON.stringify({ mode: 'concurrent' }), 'private-session', `concurrent-${id}`,
    ).then(JSON.parse)));
    assert.ok(concurrent.every(result => result.success), 'overlapping preflights retain their own trusted state');
    transcript.push(...concurrent);
    const privateTraces = traces.filter(trace => trace.name === 'mcp__private__capture');
    assert.equal(privateTraces.length, 20, 'direct and nested calls are observed by the real trace hook');
    for (const trace of privateTraces) {
      assert.ok(trace.result, 'private failures become safe tool results, not thrown errors with causes');
      const expected = trace.result.success
        ? { content: [{ type: 'text', text: 'Saved securely' }], structuredContent: { capture_id: 'synthetic-capture' } }
        : failedReceipt;
      for (const field of ['output', 'structuredResult', 'value']) assert.deepEqual(trace.result[field], expected);
      assert.equal(Object.hasOwn(trace.result, 'cause'), false);
    }
    assert.equal(remote.calls.length, 14, 'preflight rejects unsafe calls and replays safe receipts without HTTP execution');
    assert.equal(captures.length, 10, 'normal and error results reach trusted transform on both paths');
    const visible = JSON.stringify({ transcript, traces });
    for (const secret of secrets) assert.equal(visible.includes(secret), false);
    assert.ok(visible.includes('Saved securely'));
    assert.ok(visible.includes('Private MCP request failed'));
  } finally {
    for (const name of consoleMethods) console[name] = originalConsole[name];
    await mcp?.close();
    await remote.close();
  }
  console.log(JSON.stringify({ journey: 'private MCP HTTP', remoteCalls: remote.calls.length, trustedCaptures: captures.length, visibleReceipts: transcript.length, leakedSentinels: false, transcript, traces }));
});


test('private MCP discovery exceptions are safe through tool_search', async () => {
  const remote = await fixture({ failDiscovery: true });
  let mcp;
  try {
    mcp = await createMcpRuntime({ private: {
      url: remote.url,
      privateResult: { transformResult() { throw new Error('unreachable'); } },
    } });
    await mcp.settled();
    const runtime = createCodeRuntime();
    runtime.addProvider(mcp);
    const visible = await runtime.executeTool('tool_search', JSON.stringify({ query: 'capture' }));
    assert.match(visible, /Private MCP server initialization failed/);
    for (const secret of secrets) assert.equal(visible.includes(secret), false);
    console.log(JSON.stringify({ journey: 'private MCP discovery failure', receipt: JSON.parse(visible) }));
  } finally {
    await mcp?.close();
    await remote.close();
  }
});


test('failed free quote leaves the same private operation available for a later authorized call', async () => {
  const remote = await fixture();
  let quoteAttempts = 0;
  const dispatched = new Set();
  let mcp;
  try {
    mcp = await createMcpRuntime({ private: {
      url: remote.url,
      payment: mcpPayment({ methods: [Method.toClient(Methods.charge, { async createCredential() { throw new Error('No payment in this test'); } })], context: async () => {
        if (++quoteAttempts === 1) throw new Error('Synthetic quote unavailable');
        return {};
      } }),
      privateResult: {
        beforeCall({ arguments: input }) {
          if (dispatched.has(input.operation)) return { result: {
            content: [{ type: 'text', text: 'Resume saved operation' }],
          } };
          dispatched.add(input.operation);
          return { privateContext: input.operation };
        },
        transformResult({ privateContext }) {
          assert.equal(privateContext, 'same-operation');
          return { content: [{ type: 'text', text: 'Saved securely' }] };
        },
      },
    } });
    await mcp.settled();
    const runtime = createCodeRuntime();
    runtime.addProvider(mcp);
    const call = () => runtime.executeTool('mcp__private__capture', JSON.stringify({ operation: 'same-operation' })).then(JSON.parse);
    const failed = await call();
    assert.equal(failed.success, false);
    assert.equal(dispatched.size, 0, 'failed quote must not poison a paid operation fence');
    assert.equal(remote.calls.length, 0);
    const saved = await call();
    assert.equal(saved.success, true);
    assert.match(saved.output, /Saved securely/);
    assert.equal(dispatched.size, 1);
    const replay = await call();
    assert.equal(replay.success, true);
    assert.match(replay.output, /Resume saved operation/);
    assert.equal(remote.calls.length, 1, 'recovery dispatches once, then replays locally');
    for (const secret of secrets) assert.equal(JSON.stringify([failed, saved, replay]).includes(secret), false);
  } finally {
    await mcp?.close();
    await remote.close();
  }
});
