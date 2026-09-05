import {
  Client,
  Dialog,
  Principal,
  Transport,
  type ConnectAgent,
  type HostConnection,
} from "nanocodex/connect";

const CONVERSATION_KEY = "nanocodex:better-auth-example:conversation";
const PROOF_KEY = "nanocodex:better-auth-example:proof";

export type PublicConfiguration = Readonly<{
  configured: boolean;
  appId?: string;
  appOrigin?: string;
}>;

export type DurableProof = Readonly<{
  agentId: string;
  marker: string;
  first?: string;
  second?: string;
}>;

export function createConnectClient(configuration: PublicConfiguration) {
  if (!configuration.configured || !configuration.appId || !configuration.appOrigin) {
    return undefined;
  }
  if (configuration.appOrigin !== window.location.origin) {
    throw new Error(
      `This host project is registered for ${configuration.appOrigin}, not ${window.location.origin}.`,
    );
  }
  return Client.create({
    appId: configuration.appId,
    appOrigin: configuration.appOrigin,
    principal: Principal.host({ url: "/api/nanocodex/host-principal" }),
    dialog: Dialog.popup(),
    transport: Transport.http(),
  });
}

export function connectionRequest(signal?: AbortSignal) {
  return {
    authorization: "hosted" as const,
    capabilities: {
      agent: {
        finalMessages: true,
        actionSummaries: true,
        conversationHistory: true,
        rawTraces: false,
      },
      cloudAccounts: { chatgpt: true as const },
    },
    conversationId: conversationId(),
    permission: "agent.run",
    ...(signal ? { signal } : {}),
  };
}

export async function openAgent(
  client: ReturnType<typeof createConnectClient> & {},
  connection: HostConnection,
  signal?: AbortSignal,
): Promise<ConnectAgent> {
  return client.agent.create({ connection, ...(signal ? { signal } : {}) });
}

export function loadProof(agentId?: string): DurableProof | undefined {
  try {
    const value = JSON.parse(localStorage.getItem(PROOF_KEY) ?? "null") as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
    const proof = value as Record<string, unknown>;
    if (typeof proof.agentId !== "string" || typeof proof.marker !== "string"
      || (proof.first !== undefined && typeof proof.first !== "string")
      || (proof.second !== undefined && typeof proof.second !== "string")) {
      return undefined;
    }
    if (agentId && proof.agentId !== agentId) return undefined;
    return proof as DurableProof;
  } catch {
    return undefined;
  }
}

export function storeProof(proof: DurableProof): void {
  localStorage.setItem(PROOF_KEY, JSON.stringify(proof));
}

export function newProof(agentId: string): DurableProof {
  const suffix = crypto.getRandomValues(new Uint32Array(2));
  return Object.freeze({
    agentId,
    marker: `durable-${suffix[0]!.toString(16)}-${suffix[1]!.toString(16)}`,
  });
}

export function clearLocalConnectState(): void {
  localStorage.removeItem(CONVERSATION_KEY);
  localStorage.removeItem(PROOF_KEY);
}

function conversationId(): string {
  const retained = localStorage.getItem(CONVERSATION_KEY);
  if (retained && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(retained)) {
    return retained;
  }
  const created = crypto.randomUUID().toLowerCase();
  localStorage.setItem(CONVERSATION_KEY, created);
  return created;
}
