import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { createMcpRuntime } from '../runtime/mcp-runtime.mjs';
import { createCodeRuntime } from '../runtime/code-runtime.mjs';

const secrets = ['synthetic-private-token-8c4a', '4242424242424242'];
const privateBody = secrets.join(' ');

// Actual Streamable HTTP transport: no client/transport mocks. The synthetic
// remote emits private data on every response channel that a provider can use.
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

test('private MCP HTTP results, errors and notifications stay out of both runtime projections', async () => {
  const remote = await fixture();
  const transcript = [];
  const captures = [];
  const consoleMethods = ['log', 'warn', 'error', 'info', 'debug'];
  const originalConsole = Object.fromEntries(consoleMethods.map(name => [name, console[name]]));
  for (const name of consoleMethods) console[name] = (...args) => transcript.push({ console: name, args });
  let mcp;
  try {
    mcp = await createMcpRuntime({ private: { url: remote.url, privateResult: {
      beforeCall(name, input) {
        assert.equal(name, 'capture');
        if (['paid', 'unknown-job'].includes(input.mode)) throw new Error(privateBody);
      },
      transformResult(name, input, result, context) {
        assert.equal(name, 'capture');
        assert.ok(context);
        assert.ok(JSON.stringify(result).includes(secrets[0]));
        captures.push(result);
        if (input.mode === 'transform-fail') throw new Error(privateBody);
        return { content: [{ type: 'text', text: 'Saved securely' }], structuredContent: { capture_id: 'synthetic-capture' } };
      },
    } } });
    await mcp.settled();
    const runtime = createCodeRuntime();
    runtime.addProvider(mcp);
    transcript.push(JSON.parse(await runtime.executeTool('tool_search', JSON.stringify({ query: 'capture' }))));
    for (const mode of ['ok', 'error-result', 'throw', 'transform-fail', 'paid', 'unknown-job']) {
      const direct = JSON.parse(await runtime.executeTool('mcp__private__capture', JSON.stringify({ mode }), 'private-session', `direct-${mode}`));
      transcript.push(direct);
      assert.equal(direct.success, ['ok', 'error-result'].includes(mode));
      const nested = JSON.parse(await runtime.executeCode(`
        const found = await tools.tool_search({query: "capture"});
        try { text(await tools[found.tools[0].name]({mode: ${JSON.stringify(mode)}})); }
        catch (error) { text(String(error)); }
      `, 'private-session', `nested-${mode}`));
      transcript.push(nested);
      assert.equal(nested.success, true);
      assert.equal(nested.nested_calls.find(call => call.name === 'mcp__private__capture' || call.tool === 'mcp__private__capture')?.success ?? nested.nested_calls.at(-1).success, ['ok', 'error-result'].includes(mode));
    }
    assert.equal(remote.calls.length, 8, 'preflight rejects both paths before HTTP execution');
    assert.equal(captures.length, 6, 'normal and error results reach trusted transform on both paths');
    const visible = JSON.stringify(transcript);
    for (const secret of secrets) assert.equal(visible.includes(secret), false);
    assert.ok(visible.includes('Saved securely'));
    assert.ok(visible.includes('Private MCP request failed'));
  } finally {
    for (const name of consoleMethods) console[name] = originalConsole[name];
    await mcp?.close();
    await remote.close();
  }
  console.log(JSON.stringify({ journey: 'private MCP HTTP', remoteCalls: remote.calls.length, trustedCaptures: captures.length, visibleReceipts: transcript.length, leakedSentinels: false, transcript }));
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
