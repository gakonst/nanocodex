import { env } from "cloudflare:workers";
import { SELF } from "cloudflare:test";
import { beforeAll, describe, expect, it, vi } from "vitest";

import {
  handleEgress,
  isLegacyLocalBootstrapCredential,
  sponsoredResponsesFrame,
  type EgressEnv,
} from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const DEMO_USER_ID = "88888888-8888-4888-8888-888888888888";
const QUOTA_USER_ID = "77777777-7777-4777-8777-777777777777";
const SOCKET_USER_ID = "66666666-6666-4666-8666-666666666666";
const REPLAY_USER_ID = "55555555-5555-4555-8555-555555555555";
const TOOL_USER_ID = "44444444-4444-4444-8444-444444444444";
const RETRY_USER_ID = "33333333-3333-4333-8333-333333333333";
const LEASE_USER_ID = "22222222-2222-4222-8222-222222222222";
const LEASE_FALLBACK_USER_ID = "11111111-1111-4111-8111-111111111111";
const CANCEL_USER_ID = "00000000-0000-4000-8000-000000000000";
const TAKEOVER_USER_ID = "abababab-abab-4bab-8bab-abababababab";
const CONNECTION_USER_ID = "cdcdcdcd-cdcd-4dcd-8dcd-cdcdcdcdcdcd";
const FENCE_USER_ID = "efefefef-efef-4fef-8fef-efefefefefef";
const EPHEMERAL_SUBJECT = "e".repeat(43);
const QUOTA_SUBJECT = "q".repeat(43);
const SOCKET_SUBJECT = "s".repeat(43);
const REPLAY_SUBJECT = "r".repeat(43);
const TOOL_SUBJECT = "t".repeat(43);
const RETRY_SUBJECT = "y".repeat(43);
const LEASE_SUBJECT = "l".repeat(43);
const CANCEL_SUBJECT = "c".repeat(43);
const TAKEOVER_SUBJECT = "o".repeat(43);
const CONNECTION_SUBJECT = "n".repeat(43);
const FENCE_SUBJECT = "f".repeat(43);
const DURABLE_SUBJECT = "d".repeat(64);

beforeAll(async () => {
  await Promise.all([
    bindSubject(EPHEMERAL_SUBJECT, DEMO_USER_ID),
    bindSubject(QUOTA_SUBJECT, QUOTA_USER_ID),
    bindSubject(SOCKET_SUBJECT, SOCKET_USER_ID),
    bindSubject(REPLAY_SUBJECT, REPLAY_USER_ID),
    bindSubject(TOOL_SUBJECT, TOOL_USER_ID),
    bindSubject(RETRY_SUBJECT, RETRY_USER_ID),
    bindSubject(LEASE_SUBJECT, LEASE_USER_ID),
    bindSubject(CANCEL_SUBJECT, CANCEL_USER_ID),
    bindSubject(TAKEOVER_SUBJECT, TAKEOVER_USER_ID),
    bindSubject(CONNECTION_SUBJECT, CONNECTION_USER_ID),
    bindSubject(FENCE_SUBJECT, FENCE_USER_ID),
    bindSubject(DURABLE_SUBJECT, DEMO_USER_ID),
  ]);
});

describe("sponsored homepage model access", () => {
  it("treats only an unmarked local bootstrap account as a legacy auto-claim", () => {
    const legacy = {
      kind: "chatgpt" as const,
      secret: "legacy-local-access",
      accountId: "legacy-local-account",
      revision: 0,
    };
    const localEnv = {
      ENVIRONMENT: "development",
      ALLOW_LOCAL_CREDENTIAL_CLAIM: "true",
      LOCAL_CHATGPT_BOOTSTRAP: JSON.stringify({
        access_token: legacy.secret,
        account_id: legacy.accountId,
      }),
      NANOCODEX_SPONSORED_CHATGPT_USER_ID: "local-sponsor",
    };

    expect(isLegacyLocalBootstrapCredential(localEnv, "sms-user", legacy)).toBe(true);
    expect(isLegacyLocalBootstrapCredential(localEnv, "sms-user", {
      ...legacy,
      provenance: "user",
    })).toBe(false);
    expect(isLegacyLocalBootstrapCredential(localEnv, "local-sponsor", legacy)).toBe(false);
    expect(isLegacyLocalBootstrapCredential({ ...localEnv, ENVIRONMENT: "production" },
      "sms-user", legacy)).toBe(false);
    expect(isLegacyLocalBootstrapCredential(localEnv, "sms-user", {
      ...legacy,
      secret: "rotated-legacy-access",
    })).toBe(true);
    expect(isLegacyLocalBootstrapCredential(localEnv, "sms-user", {
      ...legacy,
      accountId: "independently-connected-account",
    })).toBe(false);
  });

  it("advertises the deployment owner's ChatGPT only for an ephemeral browser subject", async () => {
    const status = await modelStatus(EPHEMERAL_SUBJECT);
    expect(status.status).toBe(200);
    expect(await status.json()).toEqual({
      ready: true,
      active: "chatgpt",
      source: "sponsored",
      free_prompts_remaining: 3,
    });

    const search = await handleEgress(new Request("https://nanocodex.internal/v1/search", {
      method: "POST",
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        "content-type": "application/json",
        "x-nanocodex-subject": EPHEMERAL_SUBJECT,
      },
      body: "{}",
    }), workerEnv);
    expect(search.status).toBe(409);
    expect(await search.json()).toEqual({ error: "user_credential_unavailable" });
  });

  it("allows three live Responses prompts, skips warmup, and blocks prompt four", async () => {
    const response = await handleEgress(new Request("https://nanocodex.internal/v1/responses", {
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        "openai-beta": "responses_websockets=2026-02-06",
        upgrade: "websocket",
        "x-nanocodex-subject": SOCKET_SUBJECT,
      },
    }), workerEnv);
    expect(response.status).toBe(101);
    const socket = response.webSocket!;
    socket.accept();

    socket.send(JSON.stringify({ type: "response.create", generate: false, input: [] }));
    expect(await nextSocketMessage(socket)).toMatchObject({ type: "provider.received" });
    expect(await nextSocketMessage(socket)).toMatchObject({ type: "response.completed" });

    for (let index = 1; index <= 3; index += 1) {
      socket.send(JSON.stringify({
        type: "response.create",
        model: "gpt-5.6-sol",
        reasoning: { effort: "max", mode: "pro" },
        service_tier: "priority",
        input: [{
          type: "message",
          role: "user",
          id: `msg_socket_${index}`,
          content: [{ type: "input_text", text: `prompt ${index}` }],
        }],
      }));
      const received = await nextSocketMessage(socket);
      expect(received).toMatchObject({ type: "provider.received" });
      const forwarded = JSON.parse(String(received.frame));
      expect(forwarded).toMatchObject({
        model: "gpt-5.6-luna",
        reasoning: { effort: "none", mode: "standard" },
      });
      expect(forwarded).not.toHaveProperty("service_tier");
      expect(await nextSocketMessage(socket)).toMatchObject({ type: "response.completed" });
    }

    socket.send(JSON.stringify({
      type: "response.create",
      input: [{
        type: "message",
        role: "user",
        id: "msg_socket_four",
        content: [{ type: "input_text", text: "prompt four" }],
      }],
    }));
    expect(await nextSocketMessage(socket)).toMatchObject({
      type: "error",
      error: { code: "sponsored_prompt_limit_reached" },
    });

    const status = await modelStatus(SOCKET_SUBJECT);
    expect(await status.json()).toMatchObject({ free_prompts_remaining: 0 });
  });

  it("caps pre-prompt sponsored sockets and rejects exhausted quota before upstream", async () => {
    const providerPeers: WebSocket[] = [];
    const upstream = vi.fn(async () => {
      const pair = new WebSocketPair();
      const [client, server] = Object.values(pair);
      server.accept();
      providerPeers.push(server);
      return new Response(null, { status: 101, webSocket: client });
    });
    const { CHATGPT_EGRESS: _chatGptEgress, ...isolatedEnv } = workerEnv;
    const request = () => new Request("https://nanocodex.internal/v1/responses", {
      headers: {
        authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
        "openai-beta": "responses_websockets=2026-02-06",
        upgrade: "websocket",
        "x-nanocodex-subject": CONNECTION_SUBJECT,
      },
    });
    const opened = await Promise.all(Array.from({ length: 4 }, () => (
      handleEgress(request(), isolatedEnv, undefined, upstream as typeof fetch)
    )));
    expect(opened.map(({ status }) => status).sort()).toEqual([101, 101, 101, 429]);
    expect(upstream).toHaveBeenCalledTimes(3);

    const broker = workerEnv.USER_CREDENTIALS.getByName(CONNECTION_USER_ID);
    await Promise.all(["msg_cap_one", "msg_cap_two", "msg_cap_three"].map((promptId) => (
      broker.fetch("https://credentials.internal/v1/sponsored-prompts", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ prompt_id: promptId, prompt_hash: "c".repeat(43) }),
      })
    )));
    const exhausted = await handleEgress(
      request(), isolatedEnv, undefined, upstream as typeof fetch,
    );
    expect(exhausted.status).toBe(402);
    expect(upstream).toHaveBeenCalledTimes(3);

    for (const response of opened.filter(({ status }) => status === 101)) {
      response.webSocket!.accept();
      response.webSocket!.close(1000, "test complete");
    }
    for (const peer of providerPeers) peer.close(1000, "test complete");
  });

  it("settles heartbeat races and fences late provider results", async () => {
    const broker = workerEnv.USER_CREDENTIALS.getByName(FENCE_USER_ID);
    const reserve = (promptId: string) => broker.fetch(
      "https://credentials.internal/v1/sponsored-prompts",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ prompt_id: promptId, prompt_hash: "f".repeat(43) }),
      },
    );
    const lifecycle = (promptId: string, action: string, attempt = 1) => broker.fetch(
      "https://credentials.internal/v1/sponsored-prompts/lifecycle",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ prompt_id: promptId, attempt, action }),
      },
    );
    const grant = (promptId: string, attempt = 1) => broker.fetch(
      "https://credentials.internal/v1/sponsored-prompts/continuation",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          action: "grant",
          prompt_id: promptId,
          attempt,
          response_id: `resp_${promptId}`,
          call_ids: [`call_${promptId}`],
        }),
      },
    );

    await reserve("msg_grant_race");
    expect((await grant("msg_grant_race")).status).toBe(200);
    expect(await (await lifecycle("msg_grant_race", "heartbeat")).json())
      .toEqual({ updated: false, settled: true });

    await reserve("msg_terminal_race");
    expect(await (await lifecycle("msg_terminal_race", "terminal")).json())
      .toEqual({ updated: true });
    expect(await (await lifecycle("msg_terminal_race", "heartbeat")).json())
      .toEqual({ updated: false, settled: true });

    await reserve("msg_late_result");
    expect(await (await lifecycle("msg_late_result", "interrupted")).json())
      .toEqual({ updated: true });
    expect((await grant("msg_late_result")).status).toBe(409);
    expect(await (await lifecycle("msg_late_result", "terminal")).json())
      .toEqual({ updated: false });
    expect(await (await reserve("msg_late_result")).json())
      .toMatchObject({ dispatch: true, attempt: 2 });
    await new Promise((resolve) => setTimeout(resolve, 275));
    expect((await grant("msg_late_result", 2)).status).toBe(409);
    expect(await (await lifecycle("msg_late_result", "terminal", 2)).json())
      .toEqual({ updated: false });
  });

  it("never redispatches an admitted prompt ID", async () => {
    const socket = await openResponsesSocket(REPLAY_SUBJECT);
    const frame = JSON.stringify({
      type: "response.create",
      input: [{
        type: "message",
        role: "user",
        id: "msg_replay_once",
        content: [{ type: "input_text", text: "only once" }],
      }],
    });
    socket.send(frame);
    expect(await nextSocketMessage(socket)).toMatchObject({ type: "provider.received" });
    expect(await nextSocketMessage(socket)).toMatchObject({ type: "response.completed" });

    socket.send(frame);
    expect(await nextSocketMessage(socket)).toMatchObject({
      type: "error",
      error: { code: "sponsored_prompt_replay" },
    });
    expect(await (await modelStatus(REPLAY_SUBJECT)).json()).toMatchObject({
      free_prompts_remaining: 2,
    });
  });

  it("restores a full-history tool-search continuation after reconnect without another prompt", async () => {
    const first = await openResponsesSocket(TOOL_SUBJECT);
    first.send(JSON.stringify({
      type: "response.create",
      input: [{
        type: "message",
        role: "user",
        id: "msg_tool_search_root",
        content: [{ type: "input_text", text: "find a tool" }],
      }],
    }));
    expect(await nextSocketMessage(first)).toMatchObject({ type: "provider.received" });
    expect(await nextSocketMessage(first)).toMatchObject({
      type: "response.completed",
      response: { id: "resp_tool_search_root" },
    });
    first.close(1000, "test reconnect");

    const second = await openResponsesSocket(TOOL_SUBJECT);
    second.send(JSON.stringify({
      type: "response.create",
      input: [
        {
          type: "message",
          role: "user",
          id: "msg_tool_search_root",
          content: [{ type: "input_text", text: "find a tool" }],
        },
        {
          type: "tool_search_call",
          call_id: "call_tool_search_root",
          execution: "client",
          arguments: { query: "tools" },
        },
        {
          type: "tool_search_output",
          call_id: "call_tool_search_root",
          execution: "client",
          tools: [],
        },
      ],
    }));
    expect(await nextSocketMessage(second)).toMatchObject({ type: "provider.received" });
    expect(await nextSocketMessage(second)).toMatchObject({ type: "response.completed" });
    expect(await (await modelStatus(TOOL_SUBJECT)).json()).toMatchObject({
      free_prompts_remaining: 2,
    });
  });

  it("allows one bounded transport retry but not a completed or repeated replay", async () => {
    const frame = JSON.stringify({
      type: "response.create",
      input: [{
        type: "message",
        role: "user",
        id: "msg_retry_root",
        content: [{ type: "input_text", text: "retry me" }],
      }],
    });
    const first = await openResponsesSocket(RETRY_SUBJECT);
    first.send(frame);
    expect(await nextSocketMessage(first)).toMatchObject({ type: "provider.received" });
    first.close(1000, "transport lost");

    const second = await openResponsesSocket(RETRY_SUBJECT);
    second.send(frame);
    expect(await nextSocketMessage(second)).toMatchObject({ type: "provider.received" });
    second.close(1000, "transport lost again");

    let lifecycleState: unknown;
    for (let check = 0; check < 5; check += 1) {
      await new Promise((resolve) => setTimeout(resolve, 10));
      const lifecycle = await workerEnv.USER_CREDENTIALS.getByName(RETRY_USER_ID).fetch(
        "https://credentials.internal/v1/sponsored-prompts/lifecycle",
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            action: "heartbeat",
            prompt_id: "msg_retry_root",
            attempt: 2,
          }),
        },
      );
      lifecycleState = await lifecycle.json();
      if ((lifecycleState as { updated?: boolean }).updated === false) break;
    }
    expect(lifecycleState).toEqual({ updated: false });

    const third = await openResponsesSocket(RETRY_SUBJECT);
    third.send(frame);
    expect(await nextSocketMessage(third)).toMatchObject({
      type: "error",
      error: { code: "sponsored_prompt_replay" },
    });
    expect(await (await modelStatus(RETRY_SUBJECT)).json()).toMatchObject({
      free_prompts_remaining: 2,
    });
  });

  it("renews the attempt lease while the original sponsored generation is live", async () => {
    const frame = JSON.stringify({
      type: "response.create",
      input: [{
        type: "message",
        role: "user",
        id: "msg_retry_root",
        content: [{ type: "input_text", text: "lease recovery" }],
      }],
    });
    const original = await openResponsesSocket(LEASE_SUBJECT);
    original.send(frame);
    expect(await nextSocketMessage(original)).toMatchObject({ type: "provider.received" });

    const replacement = await openResponsesSocket(LEASE_SUBJECT);
    replacement.send(frame);
    expect(await nextSocketMessage(replacement)).toMatchObject({
      type: "error",
      error: { code: "sponsored_prompt_replay" },
    });
    expect(await (await modelStatus(LEASE_SUBJECT)).json()).toMatchObject({
      free_prompts_remaining: 2,
    });
    original.close(1000, "test complete");
    replacement.close(1000, "test complete");
  });

  it("closes a stale socket after another attempt takes over its prompt", async () => {
    const frame = JSON.stringify({
      type: "response.create",
      input: [{
        type: "message",
        role: "user",
        id: "msg_retry_root",
        content: [{ type: "input_text", text: "take over" }],
      }],
    });
    const original = await openResponsesSocket(TAKEOVER_SUBJECT);
    const originalClosed = nextSocketClose(original);
    original.send(frame);
    expect(await nextSocketMessage(original)).toMatchObject({ type: "provider.received" });

    const broker = workerEnv.USER_CREDENTIALS.getByName(TAKEOVER_USER_ID);
    const interrupted = await broker.fetch(
      "https://credentials.internal/v1/sponsored-prompts/lifecycle",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          action: "interrupted",
          prompt_id: "msg_retry_root",
          attempt: 1,
        }),
      },
    );
    expect(await interrupted.json()).toEqual({ updated: true });

    const replacement = await openResponsesSocket(TAKEOVER_SUBJECT);
    replacement.send(frame);
    expect(await nextSocketMessage(replacement)).toMatchObject({ type: "provider.received" });
    expect(await originalClosed).toMatchObject({
      code: 1011,
      reason: "sponsored prompt heartbeat failed",
    });
    replacement.close(1000, "test complete");
  });

  it("recovers one retry from the durable lease if an owner disappears without interruption", async () => {
    const broker = workerEnv.USER_CREDENTIALS.getByName(LEASE_FALLBACK_USER_ID);
    const request = () => broker.fetch("https://credentials.internal/v1/sponsored-prompts", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        prompt_id: "msg_lease_fallback",
        prompt_hash: "f".repeat(43),
      }),
    });
    expect(await (await request()).json()).toMatchObject({ dispatch: true, attempt: 1 });
    await new Promise((resolve) => setTimeout(resolve, 275));
    expect(await (await request()).json()).toMatchObject({
      dispatch: true,
      reserved: false,
      attempt: 2,
      remaining: 2,
    });
  });

  it("does not consume the retry when a waiting replacement socket closes", async () => {
    const frame = JSON.stringify({
      type: "response.create",
      input: [{
        type: "message",
        role: "user",
        id: "msg_retry_root",
        content: [{ type: "input_text", text: "cancel replacement" }],
      }],
    });
    const original = await openResponsesSocket(CANCEL_SUBJECT);
    original.send(frame);
    expect(await nextSocketMessage(original)).toMatchObject({ type: "provider.received" });

    const abandoned = await openResponsesSocket(CANCEL_SUBJECT);
    abandoned.send(frame);
    await new Promise((resolve) => setTimeout(resolve, 50));
    abandoned.close(1000, "abandoned reconnect");
    await new Promise((resolve) => setTimeout(resolve, 300));
    original.close(1000, "actual interruption");

    const retry = await openResponsesSocket(CANCEL_SUBJECT);
    retry.send(frame);
    expect(await nextSocketMessage(retry)).toMatchObject({ type: "provider.received" });
    retry.close(1000, "test complete");
  });

  it("refuses the shared credential to a durable agent subject", async () => {
    const status = await modelStatus(DURABLE_SUBJECT);
    expect(status.status).toBe(503);
    expect(await status.json()).toEqual({ error: "broker_not_ready" });
  });

  it("reserves only three distinct prompt IDs atomically and preserves retries", async () => {
    const broker = workerEnv.USER_CREDENTIALS.getByName(QUOTA_USER_ID);
    const attempts = await Promise.all([
      "msg_prompt_one",
      "msg_prompt_two",
      "msg_prompt_three",
    ].map((promptId) => broker.fetch("https://credentials.internal/v1/sponsored-prompts", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ prompt_id: promptId, prompt_hash: "h".repeat(43) }),
    })));
    expect(attempts.map(({ status }) => status)).toEqual([200, 200, 200]);

    const [retry, fourth] = await Promise.all([
      broker.fetch("https://credentials.internal/v1/sponsored-prompts", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ prompt_id: "msg_prompt_one", prompt_hash: "h".repeat(43) }),
      }),
      broker.fetch("https://credentials.internal/v1/sponsored-prompts", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ prompt_id: "msg_prompt_four", prompt_hash: "h".repeat(43) }),
      }),
    ]);
    expect(retry.status).toBe(200);
    expect(fourth.status).toBe(402);
    expect(await retry.json()).toMatchObject({
      limit: 3,
      used: 3,
      remaining: 0,
      reserved: false,
      dispatch: false,
      pending: true,
    });

    const conflict = await broker.fetch("https://credentials.internal/v1/sponsored-prompts", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ prompt_id: "msg_prompt_one", prompt_hash: "x".repeat(43) }),
    });
    expect(conflict.status).toBe(409);

    const status = await broker.fetch("https://credentials.internal/v1/sponsored-prompts");
    expect(await status.json()).toEqual({ limit: 3, used: 3, remaining: 0 });

    const productionReset = await handleEgress(new Request(
      "https://broker.internal/.well-known/nanocodex/sponsored-trial-reset",
      { method: "POST", headers: { "x-nanocodex-subject": QUOTA_SUBJECT } },
    ), {
      ...workerEnv,
      ALLOW_LOCAL_CREDENTIAL_CLAIM: "false",
      NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET: "false",
      ENVIRONMENT: "production",
    });
    expect(productionReset.status).toBe(404);

    const reset = await handleEgress(new Request(
      "https://broker.internal/.well-known/nanocodex/sponsored-trial-reset",
      { method: "POST", headers: { "x-nanocodex-subject": QUOTA_SUBJECT } },
    ), workerEnv);
    expect(reset.status).toBe(200);
    expect(await reset.json()).toEqual({ free_prompts_remaining: 3 });
    expect(await (await broker.fetch("https://credentials.internal/v1/sponsored-prompts")).json())
      .toEqual({ limit: 3, used: 0, remaining: 3 });
  });

  it("identifies prompt generations without charging warmups or tool continuations", () => {
    expect(sponsoredResponsesFrame(JSON.stringify({
      type: "response.create",
      generate: false,
      input: [],
    }))).toMatchObject({ valid: true, generation: false });
    expect(sponsoredResponsesFrame(JSON.stringify({
      type: "response.create",
      input: [{ type: "function_call_output", call_id: "call_1", output: "done" }],
    }))).toMatchObject({ valid: true, generation: true });
    expect(sponsoredResponsesFrame(JSON.stringify({
      type: "response.create",
      input: [
        { type: "message", role: "user", id: "msg_first", content: [] },
        { type: "message", role: "user", id: "msg_second", content: [] },
      ],
    }))).toMatchObject({ valid: true, generation: true, promptId: "msg_second" });
  });

  it("prefers the user's connected credential and permits it for durable agents", async () => {
    const connected = await SELF.fetch(
      `https://broker.internal/users/${DEMO_USER_ID}/credentials/openai`,
      {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ api_key: "sk-user-owned-test-secret" }),
      },
    );
    expect(connected.status).toBe(204);

    for (const subject of [EPHEMERAL_SUBJECT, DURABLE_SUBJECT]) {
      const status = await modelStatus(subject);
      expect(status.status).toBe(200);
      expect(await status.json()).toEqual({
        ready: true,
        active: "openai",
        source: "user",
      });
    }
  });
});

function modelStatus(subject: string): Promise<Response> {
  return handleEgress(new Request(
    "https://broker.internal/.well-known/nanocodex/model-status",
    { headers: { "x-nanocodex-subject": subject } },
  ), workerEnv);
}

async function openResponsesSocket(subject: string): Promise<WebSocket> {
  const response = await handleEgress(new Request("https://nanocodex.internal/v1/responses", {
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      "openai-beta": "responses_websockets=2026-02-06",
      upgrade: "websocket",
      "x-nanocodex-subject": subject,
    },
  }), workerEnv);
  expect(response.status).toBe(101);
  const socket = response.webSocket!;
  socket.accept();
  return socket;
}

async function bindSubject(subject: string, userId: string): Promise<void> {
  const response = await SELF.fetch(`https://broker.internal/subjects/${subject}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ user_id: userId }),
  });
  expect(response.status).toBe(200);
  await response.body?.cancel();
}

function nextSocketMessage(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    socket.addEventListener("message", (event) => {
      try { resolve(JSON.parse(String(event.data)) as Record<string, unknown>); }
      catch (error) { reject(error); }
    }, { once: true });
    socket.addEventListener("error", () => reject(new Error("WebSocket failed")), { once: true });
  });
}

function nextSocketClose(socket: WebSocket): Promise<Readonly<{ code: number; reason: string }>> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("WebSocket did not close")), 1_000);
    socket.addEventListener("close", (event) => {
      clearTimeout(timeout);
      resolve({ code: event.code, reason: event.reason });
    }, { once: true });
  });
}
