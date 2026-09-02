import { Agent } from "nanocodex/managed";
import type { ManagedAgentGateway } from "./conversation.ts";

const MANAGED_ORIGIN = "https://managed.nanocodex.internal";
const API_KEY = /^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/;
const AGENT_ID = /^[A-Za-z0-9._:-]{1,128}$/;

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;

export class NanocodexManagedGateway implements ManagedAgentGateway {
  private readonly fetch: typeof globalThis.fetch;
  private readonly binding: Fetcher;
  private readonly apiKey: string;

  constructor(
    binding: Fetcher,
    apiKey: string,
  ) {
    if (!API_KEY.test(apiKey)) throw new Error("Nanocodex account credential is invalid");
    this.binding = binding;
    this.apiKey = apiKey;
    this.fetch = this.boundFetch.bind(this);
  }

  async createAgent(idempotencyKey: string): Promise<string> {
    const response = await this.fetch(new URL("/v1/agents", MANAGED_ORIGIN), {
      method: "POST",
      headers: { "idempotency-key": idempotencyKey },
    });
    if (!response.ok) throw await managedFailure(response);
    const body: unknown = await response.json();
    const agentId = isRecord(body) && typeof body.agent_id === "string" ? body.agent_id : "";
    if (!AGENT_ID.test(agentId)) throw new Error("Managed agent creation returned an invalid identity");
    return agentId;
  }

  async runTurn(
    agentId: string,
    request: Readonly<{ id: string; idempotencyKey: string; input: string }>,
  ): Promise<string> {
    const result = await Agent.open(agentId, {
      apiKey: this.apiKey,
      baseUrl: MANAGED_ORIGIN,
      fetch: this.fetch,
    }).turn.prompt(request).result();
    return result.finalMessage;
  }

  async ownerId(headers?: Headers): Promise<string | undefined> {
    const response = await this.binding.fetch(new Request(
      new URL("/v1/me", MANAGED_ORIGIN),
      { headers: headers ?? { authorization: `Bearer ${this.apiKey}` } },
    ));
    if (!response.ok) {
      await response.body?.cancel();
      return undefined;
    }
    const body: unknown = await response.json();
    const user = isRecord(body) && isRecord(body.user) ? body.user : undefined;
    return typeof user?.id === "string" ? user.id : undefined;
  }

  private async boundFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    const request = new Request(input, init);
    const target = new URL(request.url);
    if (target.origin !== MANAGED_ORIGIN) throw new Error("Managed request escaped its service binding");
    const headers = new Headers(request.headers);
    headers.set("authorization", `Bearer ${this.apiKey}`);
    return this.binding.fetch(new Request(target, {
      body: request.body,
      headers,
      method: request.method,
      redirect: "manual",
      signal: request.signal,
    }));
  }
}

export async function requestingAccountId(
  binding: Fetcher,
  request: Request,
): Promise<string | undefined> {
  const headers = new Headers();
  for (const name of ["authorization", "cookie", "origin"]) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  const response = await binding.fetch(new Request(new URL("/v1/me", MANAGED_ORIGIN), { headers }));
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  const body: unknown = await response.json();
  const user = isRecord(body) && isRecord(body.user) ? body.user : undefined;
  return typeof user?.id === "string" ? user.id : undefined;
}

async function managedFailure(response: Response): Promise<Error> {
  let code = `http_${response.status}`;
  try {
    const body: unknown = await response.json();
    if (isRecord(body) && typeof body.error === "string") code = body.error;
  } catch {
    await response.body?.cancel();
  }
  return new Error(`Managed agent request failed: ${code}`);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
