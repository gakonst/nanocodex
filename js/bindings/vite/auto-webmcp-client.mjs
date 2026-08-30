import { Client, Dialog, Transport } from "../cloud/index.mjs";
import { publish } from "../webmcp/WebMcp.mjs";

const DEFAULT_ENDPOINT = "/__nanocodex/webmcp";
const DEFAULT_RESOURCES = Object.freeze([
  "urn:nanocodex:agent:run",
  "urn:nanocodex:agent:output:final",
]);
const listeners = new Set();
let generation;
let generationId = 0;
let active;
let snapshot = Object.freeze({ data: undefined, error: undefined, status: "pending" });

/** Config consumed directly by nanocodex-react/vite; no application harness is required. */
export const automaticWebMcpConfig = Object.freeze({
  subscribeAgent(resource, listener) {
    if (resource?.enabled === false) return () => {};
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  getAgent(resource) {
    return resource?.enabled === false
      ? Object.freeze({ data: undefined, error: undefined, status: "idle" })
      : snapshot;
  },
  refetchAgent() { void prepareAutomaticWebMcp({ force: true }).catch(() => {}); },
  async destroy() {
    generationId += 1;
    await closeActive();
    generation = undefined;
    publishSnapshot(Object.freeze({ data: undefined, error: undefined, status: "idle" }));
  },
});

/** Returns the signed connection that owns the current automatic Agent. */
export function automaticWebMcpConnection() {
  return active?.connection;
}

/** Starts at most one automatic generation/attachment lifecycle for this page. */
export async function prepareAutomaticWebMcp(options = {}) {
  if (generation && (options.force !== true || snapshot.status === "pending")) return generation;
  if (options.force === true) await closeActive();
  const id = ++generationId;
  publishSnapshot(Object.freeze({ data: undefined, error: undefined, status: "pending" }));
  const { force: _force, ...startOptions } = options;
  generation = startAutomaticWebMcp(startOptions).then(async (ready) => {
    if (id !== generationId) {
      ready.publication.close?.();
      await ready.agent.session.shutdown?.();
      throw new Error("automatic WebMCP generation was superseded");
    }
    active = ready;
    publishSnapshot(Object.freeze({ data: ready.agent, error: undefined, status: "success" }));
    return ready;
  }, (error) => {
    if (id === generationId) {
      publishSnapshot(Object.freeze({ data: undefined, error, status: "error" }));
    }
    throw error;
  });
  return generation;
}

async function closeActive() {
  const current = active;
  active = undefined;
  current?.publication.close?.();
  await current?.agent.session.shutdown?.();
}

/**
 * Runs the Vite-owned development generator in the authenticated application
 * page. Accounts remains the only identity and provider UI; neither tokens nor
 * the generated Agent cross into the Vite process.
 */
export async function startAutomaticWebMcp(options = {}) {
  const endpoint = options.endpoint ?? DEFAULT_ENDPOINT;
  const fetchImpl = options.fetch ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") throw new TypeError("automatic WebMCP requires fetch");
  const draft = await requestJson(fetchImpl, endpoint);
  if (!draft?.manifest || typeof draft.sourceRevision !== "string") {
    throw new Error("Nanocodex Vite returned no WebMCP generation draft");
  }

  const client = options.client ?? createConnectClient({
    appId: draft.appId,
    apiUrl: draft.apiUrl,
    dialogHost: draft.dialogHost,
  });
  const connectionRequest = Object.freeze({
    capabilities: Object.freeze({
      agent: Object.freeze({ finalMessages: true }),
      cloudAccounts: Object.freeze({ chatgpt: true }),
    }),
  });
  let connection = await client.connection.reconnect(connectionRequest);
  if (!connection) connection = await client.connection.connect(connectionRequest);

  const agent = await client.agent.create({
    connection,
    webMcp: {
      fallback: "when-empty",
      dialog: client.dialog,
    },
  });
  let manifest = draft.generated;
  if (!manifest) {
    const turn = agent.turn.prompt({ input: generatorPrompt(draft.manifest) });
    const result = await turn.result();
    const proposed = parseManifest(result.finalMessage, draft.sourceRevision);
    manifest = await requestJson(fetchImpl, endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        sourceRevision: draft.sourceRevision,
        manifest: proposed,
      }),
    });
  }
  const publication = await (options.publish ?? publish)(manifest);
  const ready = Object.freeze({ agent, connection, manifest, publication });
  if (typeof globalThis.CustomEvent === "function") {
    globalThis.dispatchEvent?.(new CustomEvent("nanocodex:webmcp-ready", { detail: ready }));
  }
  return ready;
}

function createConnectClient({ appId, apiUrl, dialogHost }) {
  const baseUrl = apiUrl ?? Transport.DEFAULT_API_URL;
  return Client.create({
    appId,
    auth: {
      challenge: `${baseUrl.replace(/\/$/, "")}/v1/connect/auth/challenge`,
      verify: `${baseUrl.replace(/\/$/, "")}/v1/connect/auth`,
      logout: `${baseUrl.replace(/\/$/, "")}/v1/connect/auth/logout`,
      resources: DEFAULT_RESOURCES,
      returnToken: true,
    },
    dialog: Dialog.iframe(dialogHost ? { host: dialogHost } : {}),
    transport: Transport.http(baseUrl),
  });
}

function generatorPrompt(draft) {
  return [
    "You are the WebMCP generator embedded in this running website.",
    "Use web_page_observe to understand the visible product and the supplied source-derived candidates.",
    "Return only one JSON WebMCP manifest. Do not use Markdown fences.",
    "Keep version 1, sourceRevision, every implementation and every evidence entry exactly unchanged.",
    "You may remove false-positive candidates and improve names, titles, descriptions, schemas, and annotations.",
    "Set approved=true only for tools that correspond to a real user-facing operation you can establish from the page or source evidence.",
    "Mutating tools remain protected by Nanocodex's per-call approval dialog even when published.",
    JSON.stringify(draft),
  ].join("\n\n");
}

function parseManifest(message, sourceRevision) {
  if (typeof message !== "string" || !message.trim()) {
    throw new Error("Nanocodex returned an empty WebMCP manifest");
  }
  let value;
  try { value = JSON.parse(message.trim()); }
  catch (error) {
    throw new Error("Nanocodex returned invalid WebMCP JSON", { cause: error });
  }
  if (!value || typeof value !== "object" || Array.isArray(value)
      || value.version !== 1 || value.sourceRevision !== sourceRevision
      || !Array.isArray(value.tools)) {
    throw new Error("Nanocodex returned a WebMCP manifest for the wrong source revision");
  }
  return value;
}

async function requestJson(fetchImpl, input, init) {
  const response = await fetchImpl(input, init);
  const body = await response.json().catch(() => undefined);
  if (!response.ok) throw new Error(body?.error ?? `Nanocodex Vite request failed with ${response.status}`);
  return body;
}

function publishSnapshot(value) {
  snapshot = value;
  for (const listener of [...listeners]) listener();
}
