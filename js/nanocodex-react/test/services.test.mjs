import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { createElement } from 'react';
import { act, create } from 'react-test-renderer';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createServicesClient, createHostedRequest } from 'nanocodex/services';
import { ServicesProvider, useVault, useProvisionPhone, usePhoneRequest, HostedServiceButton } from 'nanocodex-react/services';

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const operationId = '11111111-1111-4111-8111-111111111111';
const approvalId = '22222222-2222-4222-8222-222222222222';

// Account transport is a real HTTP server. Its upstream purchase side effect is synthetic.
test('React hooks recover a lost intent reply using the original UUID without retrying writes', async t => {
  const writes = [], paths = [];
  let receipt;
  const server = createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    paths.push(`${req.method} ${req.url}`);
    res.setHeader('content-type', 'application/json');
    if (req.method === 'POST') {
      writes.push(JSON.parse(body));
      receipt = { operation_id: operationId, approval_request_id: approvalId, status: 'pending_approval', kind: 'purchase', phone_number: '+12025550101', created_at: '2026-10-05T00:00:00Z' };
      return res.destroy();
    }
    if (req.url.endsWith(`/requests/${operationId}`)) return res.end(JSON.stringify({ request: receipt }));
    return res.end(JSON.stringify({ vault: [{ id: 'vault_synthetic_item_123456789', kind: 'api_key', name: 'Example key', created_at: 1 }] }));
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  const client = createServicesClient({ apiKey: 'synthetic-test-key', baseUrl: `http://127.0.0.1:${server.address().port}` });
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: 3, retryDelay: 0 } } });
  let provision, recovered;
  function App({ recoveryId }) {
    const vault = useVault(); provision = useProvisionPhone(); recovered = usePhoneRequest(recoveryId);
    return createElement('p', null, recovered.data?.request.status ?? vault.data?.vault[0]?.name ?? 'Loading');
  }
  const render = recoveryId => createElement(QueryClientProvider, { client: queryClient }, createElement(ServicesProvider, { client }, createElement(App, { recoveryId })));
  let tree;
  await act(async () => { tree = create(render(undefined)); });
  t.after(async () => { await act(async () => tree.unmount()); queryClient.clear(); });
  for (let attempt = 0; attempt < 50 && tree.toJSON().children[0] === 'Loading'; attempt++) await act(async () => { await new Promise(resolve => setTimeout(resolve, 10)); });
  assert.equal(tree.toJSON().children[0], 'Example key');
  assert.equal(paths.some(path => path.includes('/requests/')), false);
  await act(async () => { await assert.rejects(provision.mutateAsync({ operation_id: operationId, country: 'US', phone_number: '+12025550101' }), e => e.outcomeUnknown); });
  assert.deepEqual(writes, [{ operation_id: operationId, country: 'US', phone_number: '+12025550101' }]);
  await act(async () => { tree.update(render(operationId)); });
  for (let attempt = 0; attempt < 50 && !recovered.data; attempt++) await act(async () => { await new Promise(resolve => setTimeout(resolve, 10)); });
  assert.equal(tree.toJSON().children[0], 'pending_approval');
  assert.equal(recovered.data.request.operation_id, operationId);
  assert.equal(recovered.data.request.approval_request_id, approvalId);
  assert.equal(writes.length, 1);
  t.diagnostic(JSON.stringify({ rendered: tree.toJSON(), paths, mutationAttempts: writes.length, transport: 'native fetch + node:http', agentProvider: false }));
});

// Browser popup/window primitives are unavailable in Node; the actual account UI journey is separate.
function browser(t) {
  const previous = globalThis.window;
  const bus = new EventTarget();
  const popups = [];
  bus.open = (url, name, features) => {
    const popup = { url, name, features, closed: false, close() { this.closed = true; } };
    popups.push(popup); return popup;
  };
  bus.send = (origin, source, data) => bus.dispatchEvent(Object.assign(new Event('message'), { origin, source, data }));
  globalThis.window = bus;
  t.after(() => { globalThis.window = previous; });
  return { bus, popups };
}

test('hosted button opens on click and binds completion to popup source, origin, state and phone approval ID', async t => {
  const { bus, popups } = browser(t);
  const request = createHostedRequest({ service: 'phone', operationId: approvalId, appOrigin: 'https://app.example', state: operationId });
  const results = [];
  let tree, opening;
  await act(async () => { tree = create(createElement(HostedServiceButton, { request, onComplete: result => results.push(result) })); });
  assert.equal(popups.length, 0);
  assert.equal(tree.toJSON().children[0].type, 'button');
  await act(async () => { opening = tree.root.findByType('button').props.onClick(); });
  assert.equal(tree.root.findByType('button').props.disabled, true);
  const source = popups[0];
  assert.equal(source.url, request.url);
  const data = { type: 'nanocodex:service-enrollment', service: 'phone', state: request.state, operation_id: approvalId, status: 'complete', body: 'private' };
  bus.send('https://attacker.example', source, data); bus.send(request.origin, {}, data);
  bus.send(request.origin, source, { ...data, state: 'wrong' });
  bus.send(request.origin, source, { ...data, operation_id: operationId });
  bus.send(request.origin, source, { ...data, status: 'outcome_unknown' });
  assert.equal(results.length, 0);
  await act(async () => { bus.send(request.origin, source, data); bus.send(request.origin, source, data); await opening; });
  assert.equal(results.length, 1); assert.equal(results[0].body, undefined);
  assert.equal(source.closed, true);
  assert.equal(tree.root.findByType('button').props.disabled, false);
  await act(async () => tree.unmount());
  bus.send(request.origin, source, data); assert.equal(results.length, 1);
  t.diagnostic(JSON.stringify({ popupUrl: source.url, completions: results, spoofedMessagesAccepted: 0, popupClosed: source.closed }));
});

test('hosted button handles blocked and closed popups and cancels on request replacement or unmount', async t => {
  const { bus, popups } = browser(t);
  const request = createHostedRequest({ appOrigin: 'https://app.example', state: operationId });
  const results = [], errors = [];
  const render = value => createElement(HostedServiceButton, { request: value, onComplete: result => results.push(result), onError: error => errors.push(error.message) });
  let tree, opening;
  await act(async () => { tree = create(render(request)); });
  const open = bus.open; bus.open = () => null;
  await act(async () => { await tree.root.findByType('button').props.onClick(); });
  assert.match(tree.root.findByProps({ role: 'alert' }).children[0], /blocked/);
  bus.open = open;
  await act(async () => { opening = tree.root.findByType('button').props.onClick(); });
  popups[0].closed = true;
  await act(async () => { await opening; });
  assert.match(errors.at(-1), /closed/);
  await act(async () => { opening = tree.root.findByType('button').props.onClick(); });
  await act(async () => { tree.update(render(createHostedRequest({ appOrigin: 'https://app.example', state: approvalId }))); });
  await opening;
  assert.equal(popups[1].closed, true);
  assert.equal(tree.root.findByType('button').props.disabled, false);
  await act(async () => { opening = tree.root.findByType('button').props.onClick(); });
  await act(async () => tree.unmount()); await opening;
  assert.equal(popups[2].closed, true);
  assert.equal(results.length, 0); assert.equal(errors.length, 2);
  t.diagnostic(JSON.stringify({ cancelledPopupsClosed: popups.every(p => p.closed), errors, completions: results.length }));
});

test('Vault picker button works before any service provider and shares only the chosen item identity', async t => {
  const { bus, popups } = browser(t);
  const request = createHostedRequest({ service: 'vault', action: 'select', appOrigin: 'https://app.example', state: operationId });
  let tree, opening;
  const results = [];
  await act(async () => { tree = create(createElement(HostedServiceButton, { request, onComplete: result => results.push(result) })); });
  await act(async () => { opening = tree.root.findByType('button').props.onClick(); });
  assert.equal(new URL(popups[0].url).searchParams.get('service'), 'select');
  const data = { type: 'nanocodex:service-enrollment', service: 'vault', action: 'select', state: request.state, vault_id: 'vault_synthetic_item_123456789', kind: 'login', name: 'Existing login', username: 'private', password: 'private' };
  await act(async () => { bus.send(request.origin, popups[0], data); await opening; });
  assert.deepEqual(results, [{ type: data.type, service: 'vault', action: 'select', state: request.state, vault_id: data.vault_id, kind: 'login', name: 'Existing login' }]);
  assert.equal(popups[0].closed, true);
  await act(async () => tree.unmount());
  t.diagnostic(JSON.stringify({ selected: results[0], serviceProvider: false, grantCreated: false }));
});
