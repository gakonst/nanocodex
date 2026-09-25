import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";
import { createEgressHandler, type ActiveCredential } from "./handler";
import { validChatGptImport, type ChatGptCredentialImport } from "./chatgpt";
import { OwnerSubscription } from "./subscription";
import { openChatGptSubscription } from "./subscriptionRuntime";
import { CredentialCipher } from "./encryption";
import { routeChatGpt } from "./relay";
import { createSearchHandler } from "./search";

interface Env {
  USER_CREDENTIALS: DurableObjectNamespace<UserCredentials>;
  CREDENTIAL_ENCRYPTION_KEY: string;
  CHATGPT_EGRESS?: DurableObjectNamespace;
  GATEWAY?: Fetcher;
}

/** One SQLite-backed Durable Object per owner. No HTTP access to this class. */
export class UserCredentials extends DurableObject<Env> {
  private readonly subscription: OwnerSubscription;
  private readonly cipher: CredentialCipher;
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.cipher = new CredentialCipher(env.CREDENTIAL_ENCRYPTION_KEY, ctx.id.toString());
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS credentials (provider TEXT PRIMARY KEY, value TEXT NOT NULL)");
    this.subscription = new OwnerSubscription(ctx.storage, `chatgpt:${ctx.id.toString()}`, openChatGptSubscription, this.cipher);
  }

  private read(provider: string): string | null {
    return this.ctx.storage.sql.exec<{ value: string }>(
      "SELECT value FROM credentials WHERE provider = ? LIMIT 1", provider,
    ).toArray()[0]?.value ?? null;
  }

  private write(provider: string, value: string): void {
    this.ctx.storage.sql.exec(
      "INSERT INTO credentials (provider, value) VALUES (?, ?) ON CONFLICT(provider) DO UPDATE SET value = excluded.value",
      provider, value,
    );
  }

  async getActiveCredential(): Promise<ActiveCredential | null> {
    const active = this.read("active") ?? "openai";
    if (active === "openai") {
      const value = this.read("openai");
      return value ? { kind: "openai", secret: await this.cipher.open(value, "openai") } : null;
    }
    if (active !== "chatgpt") return null;
    try { return { kind: "chatgpt", ...await this.subscription.credential() }; }
    catch { return null; } // no Rust/provider error or secret crosses the RPC boundary
  }

  async recoverChatGptCredential(revision: string): Promise<ActiveCredential | null> {
    if (this.read("active") !== "chatgpt" || !/^(0|[1-9][0-9]*)$/.test(revision)) return null;
    try { return { kind: "chatgpt", ...await this.subscription.recover(revision) }; }
    catch { return null; }
  }

  async putCredential(provider: string, value: string): Promise<void> {
    if (provider !== "openai" || typeof value !== "string" || !value) throw new Error("Invalid credential");
    this.write(provider, await this.cipher.seal(value, "openai"));
    this.write("active", "openai");
  }

  async putChatGptCredential(imported: ChatGptCredentialImport): Promise<void> {
    if (!validChatGptImport(imported)) throw new Error("Invalid ChatGPT credential");
    try {
      await this.subscription.replace(imported);
      this.write("active", "chatgpt");
    } catch { throw new Error("ChatGPT credential unavailable"); }
  }
}

const handler = createEgressHandler<Env>({
  readCredential: (ownerId, env) => env.USER_CREDENTIALS.get(env.USER_CREDENTIALS.idFromName(ownerId)).getActiveCredential(),
  recoverCredential: (ownerId, revision, env) => env.USER_CREDENTIALS.get(env.USER_CREDENTIALS.idFromName(ownerId)).recoverChatGptCredential(revision),
  upstreamFetch: (request, ownerId, env) => {
    return new URL(request.url).hostname === "chatgpt.com"
      ? routeChatGpt(request, ownerId, env) : fetch(request);
  },
});

const search = createSearchHandler<Env>({
  readCredential: (owner, env) => env.USER_CREDENTIALS.get(env.USER_CREDENTIALS.idFromName(owner)).getActiveCredential(),
  upstreamFetch: (request, owner, env) => new URL(request.url).hostname === "chatgpt.com"
    ? routeChatGpt(request, owner, env) : fetch(request),
});

/** Private service binding only. Caller must authenticate the user before asserting the owner header. */
export default class Egress2 extends WorkerEntrypoint<Env> {
  fetch(request: Request): Promise<Response> {
    return request.url === "https://nanocodex.internal/v1/search"
      ? search(request, this.env) : handler.fetch(request, this.env);
  }

  async putCredential(ownerId: string, provider: string = "openai", value: string): Promise<void> {
    if (typeof ownerId !== "string" || !ownerId.trim() || provider !== "openai" || typeof value !== "string" || !value) {
      throw new Error("Invalid credential input");
    }
    await this.env.USER_CREDENTIALS.get(this.env.USER_CREDENTIALS.idFromName(ownerId)).putCredential(provider, value);
    handler.invalidate(ownerId);
  }

  async putChatGptCredential(ownerId: string, imported: ChatGptCredentialImport): Promise<void> {
    if (typeof ownerId !== "string" || !ownerId.trim() || !validChatGptImport(imported)) {
      throw new Error("Invalid ChatGPT credential input");
    }
    await this.env.USER_CREDENTIALS.get(this.env.USER_CREDENTIALS.idFromName(ownerId)).putChatGptCredential(imported);
    handler.invalidate(ownerId);
  }
}
