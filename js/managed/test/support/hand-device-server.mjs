// Shared Miniflare fixture for Hand device-key journeys. Builds the shipped
// managed Worker from source and serves its public routes over real HTTP and
// WebSocket. Only external identity is substituted: synthetic ncx_live_ API
// keys resolve to synthetic account principals, and the live account
// authorization lookup returns a synthetic organization grant.
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

const root = fileURLToPath(new URL("../..", import.meta.url));
const repo = fileURLToPath(new URL("../../../../", import.meta.url));
export const owner = "11111111-1111-4111-8111-111111111111";
export const otherOwner = "44444444-4444-4444-8444-444444444444";
export const apiKey = "ncx_live_" + "a".repeat(12) + "_" + "b".repeat(43);
export const otherApiKey = "ncx_live_" + "c".repeat(12) + "_" + "d".repeat(43);
/** Synthetic signed-in browser session for the owner (the account service's session store is substituted). */
export const sessionCookie = "nanocodex_account=s_" + "e".repeat(43);
const grant = { organizationId: "22222222-2222-4222-8222-222222222222", teamId: "33333333-3333-4333-8333-333333333333",
  role: "owner", authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"] };

const source = (keys, publicOrigin) => [
  "import { DurableObject } from 'cloudflare:workers';",
  "import { AccountHostedToolsProvider } from './src/account-hosted-tools.ts';",
  "import worker, { AccountHostedTools, DurableAgentSession, VmHostPool } from './src/index.ts';",
  "export { AccountHostedTools, DurableAgentSession, VmHostPool };",
  "// Private service-binding entrypoint for egress SSH host-key attestation lookups.",
  "export { HandDeviceSshHostKeys } from './src/index.ts';",
  "const PUBLIC_ORIGIN = " + JSON.stringify(publicOrigin ?? null) + ";",
  "// Structured observations as JSON lines so the journey can assert on them.",
  "const info = console.info.bind(console);",
  "console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);",
  "// Test-only driver: the model-facing tool path that invokes a published Hand tool.",
  "export class ToolDriver extends DurableObject {",
  "  async fetch(request) {",
  "    const { owner, machine, call, cmd, workdir } = await request.json();",
  "    const provider = new AccountHostedToolsProvider(this.env.NANOCODEX_ACCOUNT_TOOLS, owner, () => true, undefined);",
  "    await provider.refresh();",
  "    const tool = provider.machineTool(machine, 'exec_command');",
  "    if (!tool) return Response.json({ error: 'tool_unavailable' }, { status: 404 });",
  "    try {",
  "      const result = await tool.handler({ cmd, workdir: workdir || '/synthetic/workspace' },",
  "        { sessionId: 'device-journey', turnId: 'device-turn', callId: call, model: 'synthetic', signal: request.signal });",
  "      return Response.json(result);",
  "    } catch (error) { return Response.json({ error: String(error && error.message || error) }, { status: 502 }); }",
  "  }",
  "}",
  "const KEYS = " + JSON.stringify(keys) + ";",
  "const DIGESTS = " + JSON.stringify(Object.fromEntries(Object.entries(keys).map(([key, userId]) => [
    createHash("sha256").update(key).digest("base64url"), { userId, id: key.slice(9, 21) }]))) + ";",
  "const GRANT = " + JSON.stringify(grant) + ";",
  "const SESSIONS = " + JSON.stringify({ [sessionCookie.split("=")[1]]: owner }) + ";",
  "export default { async fetch(request, env, ctx) {",
  "  // Optional stand-in for a TLS-terminating front door: the Worker sees the configured public origin.",
  "  if (PUBLIC_ORIGIN) { const incoming = new URL(request.url); request = new Request(PUBLIC_ORIGIN + incoming.pathname + incoming.search, request); }",
  "  const url = new URL(request.url);",
  "  if (url.pathname === '/__fixture/tool') return env.DRIVER.getByName('driver').fetch(request);",
  "  // server_hand connect mints this grant during SSH setup; the fixture calls the same owner RPC.",
  "  if (url.pathname === '/__fixture/grant') {",
  "    const { owner, host, name } = await request.json();",
  "    return Response.json(await env.NANOCODEX_ACCOUNT_TOOLS.getByName(owner).mintServerHandDeviceGrant(owner, host, name));",
  "  }",
  "  const authorization = request.headers.get('authorization');",
  "  const entry = Object.entries(KEYS).find(([key]) => authorization === 'Bearer ' + key);",
  "  const actor = entry ? Object.assign({ kind: 'api_key', userId: entry[1], subjectId: 'user:' + entry[1], credentialId: 'synthetic-' + entry[1] }, GRANT) : undefined;",
  "  // Identity lookup used by the CLI to locate its state; substitutes the account service.",
  "  if (url.pathname === '/v1/me' && request.method === 'GET') return actor ? Response.json({ user: { id: actor.userId } }) : Response.json({ error: 'unauthorized' }, { status: 401 });",
  "  const users = { getByName: id => ({ resolveAuthorization: async () => ({ userId: id, grant: GRANT }) }) };",
  "  // API-key records resolved by the shipped authenticate() (account hand-device routes never trust an injected principal).",
  "  const apiKeys = { getByName: digest => ({ id: { toString: () => '' }, resolveAuthorizedKey: async () => DIGESTS[digest] ? Object.assign({",
  "    id: DIGESTS[digest].id, label: 'synthetic', prefix: 'ncx_live_' + DIGESTS[digest].id, createdAt: 1, digest, userId: DIGESTS[digest].userId }, GRANT) : undefined }) };",
  "  // Browser sessions resolved by the shipped authenticate() cookie path; only the session store is substituted.",
  "  const auth = { idFromName: name => name, get: () => ({ readAccountSession: async token => SESSIONS[token] ? { userId: SESSIONS[token], expiresAt: Date.now() / 1000 + 3600 } : undefined }) };",
  "  return worker.fetch(request, Object.assign({}, env, { NANOCODEX_USERS: users, NANOCODEX_API_KEYS: apiKeys, NANOCODEX_AUTH: auth }), ctx, actor);",
  "} };",
].join("\n");

/**
 * Start the managed Worker. Returns its base URL and captured structured observations.
 * Optional workers run beside it in the same workerd (e.g. the egress Worker,
 * which binds the managed "managed" Worker's HandDeviceSshHostKeys entrypoint).
 */
export async function startHandDeviceServer({ output, ttlSeconds = 10, publicOrigin, workers = [] } = {}) {
  await mkdir(output, { recursive: true });
  const assets = [];
  let wasmSequence = 0;
  const bundle = await build({ stdin: { contents: source({ [apiKey]: owner, [otherApiKey]: otherOwner }, publicOrigin), resolveDir: root },
    bundle: true, write: false, metafile: true, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    external: ["cloudflare:*", "node:*"],
    alias: { "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"),
      "node-rsa": join(repo, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const path = join(args.resolveDir, args.path), name = "fixture-" + wasmSequence++ + ".wasm";
      assets.push({ type: "CompiledWasm", path: name, contents: await readFile(path) });
      return { path: "./" + name, external: true };
    }); } }], logLevel: "silent" });
  await writeFile(join(output, "worker-inputs.json"),
    JSON.stringify(Object.keys(bundle.metafile.inputs).filter(path => path.includes("src/hand")), null, 2));
  const observations = [], logs = [];
  const capture = line => {
    logs.push(line);
    const start = line.indexOf("{");
    if (start >= 0 && line.includes('"type"')) { try { observations.push(JSON.parse(line.slice(start))); } catch { /* unstructured */ } }
  };
  const managed = { name: "managed",
    compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
    modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
    bindings: { NANOCODEX_HAND_DEVICE_CREDENTIAL_TTL_SECONDS: String(ttlSeconds) },
    durableObjects: { DRIVER: { className: "ToolDriver", useSQLite: true },
      NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
      NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true },
      NANOCODEX_VM_HOST_POOLS: { className: "VmHostPool", useSQLite: true } },
    r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"],
    serviceBindings: { NANOCODEX: async request => {
      const path = new URL(request.url).pathname;
      if (path.startsWith("/subjects/")) return new Response(null, { status: 204 });
      if (path.endsWith("/catalog")) return Response.json({ connectors: {}, mcp_connections: [] });
      if (path.endsWith("/credentials/vault")) return Response.json({ vault: [] });
      return Response.json({ tools: [], machines: [], connections: [] });
    } } };
  const mf = new Miniflare({ port: 0, host: "127.0.0.1", durableObjectsPersist: join(output, "sqlite"),
    ...(workers.length ? { workers: [managed, ...workers] } : managed),
    handleRuntimeStdio(stdout, stderr) {
      createInterface({ input: stdout }).on("line", capture);
      createInterface({ input: stderr }).on("line", capture);
    } });
  const base = (await mf.ready).href.replace(/\/$/, "");
  /** Invoke exec_command on an attached account machine through the model-facing provider path. */
  const callHandTool = async ({ ownerId = owner, machineId, cmd, workdir, callId = crypto.randomUUID() }) => {
    const response = await fetch(base + "/__fixture/tool", { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ owner: ownerId, machine: machineId, call: callId, cmd, workdir }), signal: AbortSignal.timeout(20_000) });
    return { status: response.status, body: await response.json() };
  };
  return { base, origin: publicOrigin ?? new URL(base).origin, owner, apiKey, otherOwner, otherApiKey, sessionCookie, observations, logs, callHandTool,
    worker: name => mf.getWorker(name), stop: () => mf.dispose() };
}
