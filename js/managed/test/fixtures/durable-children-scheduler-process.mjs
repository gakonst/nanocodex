import { Miniflare } from 'miniflare';
import { readFile } from 'node:fs/promises';

// Each fork owns a fresh workerd process. Persisted SQLite/R2 survives SIGKILL;
// no JavaScript runtime, live Session, model socket or WASM instance is reused.
const directory = process.argv[2];
const names = JSON.parse(await readFile(directory + '/assets.json', 'utf8'));
const modules = [
  { type: 'ESModule', path: 'worker.mjs', contents: await readFile(directory + '/worker.mjs', 'utf8') },
  ...await Promise.all(names.map(async name => ({ type: 'CompiledWasm', path: name, contents: await readFile(directory + '/' + name) }))),
];
const mf = new Miniflare({
  durableObjectsPersist: directory + '/sqlite',
  r2Persist: directory + '/r2',
  workers: [
    {
      name: 'managed', modules, compatibilityDate: '2026-07-29',
      compatibilityFlags: ['nodejs_compat', 'enable_request_signal', 'durable_object_io_tasks_prevent_eviction'],
      bindings: { AGENT_IDLE_TIMEOUT_MS: '1000' },
      durableObjects: {
        NANOCODEX_SESSIONS: { className: 'FixtureSession', useSQLite: true },
        MODEL: { className: 'FixtureModel', useSQLite: true },
        NANOCODEX_ACCOUNT_TOOLS: { className: 'FixtureModel', useSQLite: true },
        NANOCODEX_MEMORY: { className: 'FixtureModel', useSQLite: true },
      },
      serviceBindings: { NANOCODEX: 'provider' },
      r2Buckets: ['NANOCODEX_HISTORY', 'NANOCODEX_WORKSPACES'],
    },
    {
      name: 'provider',
      modules: [{ type: 'ESModule', path: 'provider.mjs', contents: `export default { fetch(request, env) { return env.MODEL.getByName('provider').fetch(request); } };` }],
      compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat'],
      durableObjects: { MODEL: { className: 'FixtureModel', scriptName: 'managed', useSQLite: true } },
    },
  ],
});
await mf.ready;
process.send({ ready: true });
process.on('message', async message => {
  try {
    const headers = {
      'x-nanocodex-owner-id': '00000000-0000-4000-8000-000000000005',
      'x-nanocodex-session-organization-id': '00000000-0000-4000-8000-000000000003',
      'x-nanocodex-session-team-id': '00000000-0000-4000-8000-000000000004',
      'x-nanocodex-authorization-epoch': '1',
      'x-nanocodex-capabilities': JSON.stringify(['agents:read', 'agents:write', 'tools:use']),
    };
    const response = await mf.dispatchFetch('https://fixture.internal' + message.path, {
      method: message.method, headers,
      ...(message.body === undefined ? {} : { body: JSON.stringify(message.body) }),
    });
    if (!response.ok) throw Error(message.path + ' HTTP ' + response.status + ': ' + (await response.text()).slice(0, 2048));
    const value = response.status === 204 ? null : await response.json();
    process.send({ request: message.request, value });
  } catch (error) {
    process.send({ request: message.request, error: String(error.message) });
  }
});
