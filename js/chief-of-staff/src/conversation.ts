import { digest, sameChannelIdentity, type ChannelIdentity } from "./protocol.ts";

const MAX_INPUT_CHARS = 120_000;
const MAX_SLACK_REPLY_CHARS = 35_000;
const MAX_VIBER_REPLY_CHARS = 7_000;

export interface ConversationStore {
  bindIdentity(identity: ChannelIdentity): Promise<boolean>;
  get<T>(key: string): Promise<T | undefined>;
  put<T>(key: string, value: T): Promise<void>;
}

export interface ManagedAgentGateway {
  createAgent(idempotencyKey: string): Promise<string>;
  runTurn(
    agentId: string,
    request: Readonly<{ id: string; idempotencyKey: string; input: string }>,
  ): Promise<string>;
}

export type ConversationTurnRequest = Readonly<{
  actorId: string;
  channel: ChannelIdentity;
  messageId: string;
  text: string;
}>;

export type ConversationTurnResult = Readonly<{
  agentId: string;
  finalMessage: string;
  replayed: boolean;
  turnId: string;
}>;

type TurnRecord = Readonly<{
  agentId: string;
  finalMessage?: string;
  inputDigest: string;
  turnId: string;
}>;

export class ConversationEngine {
  private readonly store: ConversationStore;
  private readonly managed: ManagedAgentGateway;

  constructor(
    store: ConversationStore,
    managed: ManagedAgentGateway,
  ) {
    this.store = store;
    this.managed = managed;
  }

  async turn(request: ConversationTurnRequest): Promise<ConversationTurnResult> {
    validateTurn(request);
    if (!(await this.store.bindIdentity(request.channel))) {
      throw new ConversationError(403, "channel_identity_conflict");
    }
    const channelDigest = await digest(request.channel);
    const messageDigest = await digest({
      messageId: request.messageId,
      platform: request.channel.platform,
    });
    const input = chiefOfStaffPrompt(request);
    const inputDigest = await digest(input);
    const turnKey = `turn:${messageDigest}`;
    const retained = await this.store.get<TurnRecord>(turnKey);
    if (retained && retained.inputDigest !== inputDigest) {
      throw new ConversationError(409, "idempotency_conflict");
    }
    if (retained?.finalMessage !== undefined) {
      return {
        agentId: retained.agentId,
        finalMessage: retained.finalMessage,
        replayed: true,
        turnId: retained.turnId,
      };
    }

    const agentId = retained?.agentId ?? await this.agent(channelDigest);
    const turnId = retained?.turnId ?? `${request.channel.platform}-${messageDigest.slice(0, 48)}`;
    const record = { agentId, inputDigest, turnId } satisfies TurnRecord;
    if (!retained) await this.store.put(turnKey, record);
    const result = normalizeReply(await this.managed.runTurn(agentId, {
      id: turnId,
      idempotencyKey: `chief-turn:${channelDigest}:${messageDigest}`,
      input,
    }), request.channel.platform);
    await this.store.put(turnKey, { ...record, finalMessage: result });
    return { agentId, finalMessage: result, replayed: false, turnId };
  }

  private async agent(channelDigest: string): Promise<string> {
    const retained = await this.store.get<string>("agent-id");
    if (retained) return retained;
    const created = await this.managed.createAgent(`chief-session:${channelDigest}`);
    await this.store.put("agent-id", created);
    return created;
  }
}

export class ConversationError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string) {
    super(code);
    this.status = status;
    this.code = code;
  }
}

export class MemoryConversationStore implements ConversationStore {
  readonly values = new Map<string, unknown>();

  async bindIdentity(identity: ChannelIdentity): Promise<boolean> {
    const retained = this.values.get("channel-identity") as ChannelIdentity | undefined;
    if (retained && !sameChannelIdentity(retained, identity)) return false;
    if (!retained) this.values.set("channel-identity", identity);
    return true;
  }

  async get<T>(key: string): Promise<T | undefined> {
    return this.values.get(key) as T | undefined;
  }

  async put<T>(key: string, value: T): Promise<void> {
    this.values.set(key, value);
  }
}

function validateTurn(request: ConversationTurnRequest): void {
  if (!request.text.trim()) throw new ConversationError(400, "empty_message");
  if (request.text.length > MAX_INPUT_CHARS) {
    throw new ConversationError(413, "message_too_large");
  }
  if (request.channel.platform === "slack") {
    if (!/^[UW][A-Z0-9]+$/.test(request.actorId)) {
      throw new ConversationError(400, "invalid_actor");
    }
    if (!/^[0-9]+\.[0-9]+$/.test(request.messageId)) {
      throw new ConversationError(400, "invalid_message");
    }
  } else {
    if (request.actorId !== request.channel.userId) {
      throw new ConversationError(400, "invalid_actor");
    }
    if (!/^[0-9]+$/.test(request.messageId)) {
      throw new ConversationError(400, "invalid_message");
    }
  }
}

function chiefOfStaffPrompt(request: ConversationTurnRequest): string {
  const platform = request.channel.platform === "slack" ? "Slack" : "Viber";
  return [
    `You are the user's Chief of Staff in ${platform}.`,
    "Be concise, action-oriented, and explicit about uncertainty.",
    "Never claim to have contacted people or changed external state unless tool evidence proves it.",
    `${platform} actor: ${request.actorId}`,
    "User message:",
    request.text.trim(),
  ].join("\n");
}

function normalizeReply(value: string, platform: ChannelIdentity["platform"]): string {
  const reply = value.trim() || "I completed the turn, but it did not produce a text response.";
  const maxChars = platform === "slack" ? MAX_SLACK_REPLY_CHARS : MAX_VIBER_REPLY_CHARS;
  if (reply.length <= maxChars) return reply;
  const suffix = `\n\n[Response truncated for ${platform === "slack" ? "Slack" : "Viber"}]`;
  return `${reply.slice(0, maxChars - suffix.length)}${suffix}`;
}
