import {
  Client,
  Dialog,
  Transport,
  type ConnectAgent,
  type Connection,
} from "nanocodex/connect";
import type { NamedTool } from "nanocodex/host";
import { createCleanupTool } from "./extension.ts";
import type { CookieSyncTransport } from "./cookie-sync.ts";
import { createAuthenticatedCookieSyncTransport } from "./cookie-sync-client.ts";

const CONNECT_API = "https://nanocodex-connect-api.gakonst.workers.dev";
const CONNECT_DIALOG = "https://nanocodex.gakonst.workers.dev/connect-dialog/";
const MANAGED_AGENT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const CONVERSATION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export const LEGACY_CONVERSATION_ID = "legacy";
export const CHROME_CONNECT_RESOURCES = [
  "urn:nanocodex:agent:run",
  "urn:nanocodex:browser-cookies:sync",
] as const;

export const CHROME_CONNECT_TOOLS = [createCleanupTool(() => {
  throw new Error("The browser tool is available only after the approved agent attaches.");
})] as const;

export const CHROME_CONNECT_REQUEST = {
  capabilities: {
    agent: {
      finalMessages: true,
      actionSummaries: true,
      conversationHistory: true,
      rawTraces: true,
    },
    cloudAccounts: { chatgpt: true },
  },
  authorization: "hosted",
  permission: "agent.run",
  tools: CHROME_CONNECT_TOOLS,
} as const;

type ConnectClient = Client.Client;
export type ExpectedConversation = Readonly<{ agentId: string; accountAddress?: string }>;
const conversationClients = new Map<string, ConnectClient>();
const agentClients = new Map<string, ConnectClient>();

export async function connectNanocodex(
  conversationId = LEGACY_CONVERSATION_ID,
  expected?: ExpectedConversation,
): Promise<Connection> {
  const client = clientForConversation(conversationId);
  const snapshot = expected ? snapshotConversationStorage(conversationId) : undefined;
  try {
    const connection = await client.connection.connect(connectRequest(conversationId));
    if (!isManagedAgentId(connection.agentId) || (expected && !connectionMatchesIdentity(connection, expected))) {
      await client.connection.disconnect().catch(() => {});
      restoreConversationStorage(conversationId, snapshot ?? new Map());
      conversationClients.delete(conversationId);
      throw new Error("This conversation belongs to a different Nanocodex account. Start a new conversation instead.");
    }
    agentClients.set(connection.agentId, client);
    return connection;
  } catch (cause) {
    if (snapshot) {
      restoreConversationStorage(conversationId, snapshot);
      conversationClients.delete(conversationId);
    }
    throw cause;
  }
}

export async function reconnectNanocodex(
  conversationId = LEGACY_CONVERSATION_ID,
  expected?: ExpectedConversation,
): Promise<Connection | undefined> {
  const client = clientForConversation(conversationId);
  const connection = await client.connection.reconnect(connectRequest(conversationId));
  if (!connection) return undefined;
  if (!isManagedAgentId(connection.agentId) || (expected && !connectionMatchesIdentity(connection, expected))) {
    await client.connection.disconnect();
    conversationClients.delete(conversationId);
    if (expected) {
      throw new Error("The retained authorization no longer matches this conversation.");
    }
    return undefined;
  }
  agentClients.set(connection.agentId, client);
  return connection;
}

export function disconnectNanocodex(conversationId = LEGACY_CONVERSATION_ID): Promise<void> {
  return clientForConversation(conversationId).connection.disconnect();
}

export function createConnectedAgent(
  connection: Connection,
  tools: readonly NamedTool[],
  signal?: AbortSignal,
): Promise<ConnectAgent> {
  const client = agentClients.get(connection.agentId);
  if (!client) throw new Error("The durable conversation session is unavailable.");
  return client.agent.create({ connection, tools, signal });
}

export function cookieSyncTransport(connection: Connection): CookieSyncTransport {
  const client = agentClients.get(connection.agentId);
  if (!client) throw new Error("The authenticated cookie-sync session is unavailable.");
  return createAuthenticatedCookieSyncTransport((input, init) => client.fetch(input, init));
}

export function createConversationId(): string {
  return crypto.randomUUID();
}

export function isConversationId(value: string): boolean {
  return value === LEGACY_CONVERSATION_ID || CONVERSATION_ID.test(value);
}

function clientForConversation(conversationId: string): ConnectClient {
  if (!isConversationId(conversationId)) throw new TypeError("Invalid durable conversation identifier.");
  const retained = conversationClients.get(conversationId);
  if (retained) return retained;
  if (conversationId === LEGACY_CONVERSATION_ID) migrateLegacyConversationSession();
  const parameters: Client.Parameters = {
    appId: "nanocodex-chrome",
    auth: {
      challenge: `${CONNECT_API}/v1/connect/auth/challenge`,
      verify: `${CONNECT_API}/v1/connect/auth`,
      logout: `${CONNECT_API}/v1/connect/auth/logout`,
      resources: CHROME_CONNECT_RESOURCES,
      returnToken: true,
    },
    dialog: Dialog.popup({
      host: CONNECT_DIALOG,
      key: `nanocodex-chrome-${conversationId}`,
      name: "Nanocodex Connect",
    }),
    session: conversationStorage(conversationId),
    transport: Transport.http(CONNECT_API, {
      credentials: "omit",
      key: `nanocodex-chrome-${conversationId}`,
      name: "Nanocodex Connect API",
    }),
  };
  const client = Client.create(parameters);
  conversationClients.set(conversationId, client);
  return client;
}

function connectRequest(conversationId: string) {
  return {
    ...CHROME_CONNECT_REQUEST,
    ...(conversationId === LEGACY_CONVERSATION_ID ? {} : { conversationId }),
  } as const;
}

function conversationStorage(conversationId: string): Pick<Storage, "getItem" | "setItem" | "removeItem"> {
  const prefix = conversationStoragePrefix(conversationId);
  return {
    getItem: (key) => localStorage.getItem(`${prefix}${key}`),
    setItem: (key, value) => localStorage.setItem(`${prefix}${key}`, value),
    removeItem: (key) => localStorage.removeItem(`${prefix}${key}`),
  };
}

function snapshotConversationStorage(conversationId: string): Map<string, string> {
  const prefix = conversationStoragePrefix(conversationId);
  const snapshot = new Map<string, string>();
  for (let index = 0; index < localStorage.length; index += 1) {
    const key = localStorage.key(index);
    if (key?.startsWith(prefix)) snapshot.set(key, localStorage.getItem(key) ?? "");
  }
  return snapshot;
}

function restoreConversationStorage(conversationId: string, snapshot: ReadonlyMap<string, string>): void {
  const prefix = conversationStoragePrefix(conversationId);
  const currentKeys: string[] = [];
  for (let index = 0; index < localStorage.length; index += 1) {
    const key = localStorage.key(index);
    if (key?.startsWith(prefix)) currentKeys.push(key);
  }
  for (const key of currentKeys) localStorage.removeItem(key);
  for (const [key, value] of snapshot) localStorage.setItem(key, value);
}

function conversationStoragePrefix(conversationId: string): string {
  return `nanocodex:chrome:conversation:${conversationId}:`;
}

export function migrateLegacyConversationSession(
  storage: Pick<Storage, "getItem" | "setItem" | "removeItem"> = localStorage,
): void {
  const key = "nanocodex:connect:nanocodex-chrome:session";
  const migratedKey = `${conversationStoragePrefix(LEGACY_CONVERSATION_ID)}${key}`;
  const retained = storage.getItem(key);
  if (storage.getItem(migratedKey) === null && retained !== null) storage.setItem(migratedKey, retained);
  if (retained !== null) storage.removeItem(key);
}

function connectionMatchesIdentity(connection: Connection, expected: ExpectedConversation): boolean {
  return connection.agentId === expected.agentId
    && (expected.accountAddress === undefined
      || connection.accountAddress.toLowerCase() === expected.accountAddress.toLowerCase());
}

export function isManagedAgentId(value: string): boolean {
  return MANAGED_AGENT_ID.test(value);
}

export type { Connection as NanocodexConnection };
