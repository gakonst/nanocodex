import type { ManagedAgentGateway } from "./conversation.ts";

const ACCOUNT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const AGENT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export type ChiefOfStaffIdentity = Readonly<{
  provider: "slack" | "viber" | "whatsapp";
  subject: string;
  tenant: string;
}>;

export type ManagedBackend = Readonly<{
  createAgent(identity: unknown, idempotencyKey: unknown): Promise<string>;
  requestingAccountId(request: Request): Promise<string | null>;
  runTurn(identity: unknown, agentId: unknown, request: unknown): Promise<string>;
}>;

export class NanocodexManagedGateway implements ManagedAgentGateway {
  private readonly binding: ManagedBackend;
  private readonly identity: ChiefOfStaffIdentity;

  constructor(binding: ManagedBackend, identity: ChiefOfStaffIdentity) {
    this.binding = binding;
    this.identity = identity;
  }

  async createAgent(idempotencyKey: string): Promise<string> {
    const agentId = await this.binding.createAgent(this.identity, idempotencyKey);
    if (!AGENT_ID.test(agentId)) throw new Error("Managed agent creation returned an invalid identity");
    return agentId;
  }

  async runTurn(
    agentId: string,
    request: Readonly<{ id: string; idempotencyKey: string; input: string }>,
  ): Promise<string> {
    const finalMessage = await this.binding.runTurn(this.identity, agentId, request);
    if (typeof finalMessage !== "string") {
      throw new Error("Managed agent turn returned an invalid result");
    }
    return finalMessage;
  }
}

export async function requestingAccountId(
  binding: ManagedBackend,
  request: Request,
): Promise<string | undefined> {
  const headers = new Headers();
  for (const name of ["authorization", "cookie", "origin"]) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  const accountId = await binding.requestingAccountId(new Request(request.url, { headers }));
  return typeof accountId === "string" && ACCOUNT_ID.test(accountId) ? accountId : undefined;
}
