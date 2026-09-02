import assert from "node:assert/strict";
import test from "node:test";
import {
  ConversationEngine,
  ConversationError,
  MemoryConversationStore,
  type ManagedAgentGateway,
} from "../src/conversation.ts";
import { digest, type ChannelIdentity } from "../src/protocol.ts";

class RememberingGateway implements ManagedAgentGateway {
  readonly created: string[] = [];
  readonly turns: Readonly<{ agentId: string; idempotencyKey: string; input: string }>[] = [];
  private memory = "";

  async createAgent(idempotencyKey: string): Promise<string> {
    this.created.push(idempotencyKey);
    return "agent-chief";
  }

  async runTurn(
    agentId: string,
    request: Readonly<{ id: string; idempotencyKey: string; input: string }>,
  ): Promise<string> {
    (this.turns as { agentId: string; idempotencyKey: string; input: string }[]).push({
      agentId,
      idempotencyKey: request.idempotencyKey,
      input: request.input,
    });
    if (request.input.includes("Remember kiwi")) {
      this.memory = "kiwi";
      return "I’ll remember kiwi.";
    }
    return `You asked me to remember ${this.memory}.`;
  }
}

const channel: ChannelIdentity = {
  accountId: "account-a",
  channelId: "D123ABC",
  conversationId: "dm:U123ABC",
  platform: "slack",
  teamId: "T123ABC",
};

test("a durable conversation reuses one managed agent across two turns", async () => {
  const store = new MemoryConversationStore();
  const gateway = new RememberingGateway();
  await new ConversationEngine(store, gateway).turn({
    actorId: "U123ABC",
    channel,
    messageId: "1700000000.000001",
    text: "Remember kiwi",
  });
  const second = await new ConversationEngine(store, gateway).turn({
    actorId: "U123ABC",
    channel,
    messageId: "1700000001.000002",
    text: "What should you remember?",
  });

  assert.equal(second.agentId, "agent-chief");
  assert.equal(second.finalMessage, "You asked me to remember kiwi.");
  assert.equal(gateway.created.length, 1);
  assert.equal(gateway.turns.length, 2);
  assert.equal(gateway.turns[0]?.agentId, gateway.turns[1]?.agentId);
});

test("a replay returns the retained result without another managed turn", async () => {
  const store = new MemoryConversationStore();
  const gateway = new RememberingGateway();
  const engine = new ConversationEngine(store, gateway);
  const request = {
    actorId: "U123ABC",
    channel,
    messageId: "1700000000.000001",
    text: "Remember kiwi",
  } as const;

  const first = await engine.turn(request);
  const replay = await engine.turn(request);

  assert.equal(first.replayed, false);
  assert.equal(replay.replayed, true);
  assert.equal(replay.finalMessage, first.finalMessage);
  assert.equal(gateway.turns.length, 1);
});

test("a replayed message id with altered input is fenced", async () => {
  const engine = new ConversationEngine(new MemoryConversationStore(), new RememberingGateway());
  await engine.turn({
    actorId: "U123ABC",
    channel,
    messageId: "1700000000.000001",
    text: "Remember kiwi",
  });

  await assert.rejects(
    engine.turn({
      actorId: "U123ABC",
      channel,
      messageId: "1700000000.000001",
      text: "Forget everything",
    }),
    (error: unknown) => error instanceof ConversationError
      && error.status === 409
      && error.code === "idempotency_conflict",
  );
});

test("durable state cannot be rebound across accounts or Slack channels", async () => {
  const store = new MemoryConversationStore();
  const gateway = new RememberingGateway();
  const engine = new ConversationEngine(store, gateway);
  await engine.turn({
    actorId: "U123ABC",
    channel,
    messageId: "1700000000.000001",
    text: "Remember kiwi",
  });

  for (const conflicting of [
    { ...channel, accountId: "account-b" },
    { ...channel, channelId: "D999XYZ" },
  ]) {
    await assert.rejects(
      engine.turn({
        actorId: "U123ABC",
        channel: conflicting,
        messageId: "1700000001.000002",
        text: "What should you remember?",
      }),
      (error: unknown) => error instanceof ConversationError
        && error.status === 403
        && error.code === "channel_identity_conflict",
    );
  }
  assert.notEqual(await digest(channel), await digest({ ...channel, accountId: "account-b" }));
  assert.notEqual(await digest(channel), await digest({ ...channel, channelId: "D999XYZ" }));
});
