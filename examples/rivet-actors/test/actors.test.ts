import { createServer, type IncomingMessage, type Server } from "node:http";

import { setupTest } from "rivetkit/test";
import { afterAll, beforeAll, describe, expect, test, vi } from "vitest";
import { WebSocketServer } from "ws";

import { registry } from "../src/registry.js";
import { createCapabilityProof } from "../src/auth-capability.js";

const ACCOUNT_ID = "rivet-test-account";
const AUTH_CAPABILITY = "test-only-credential-capability-with-32-bytes";
const VALID_ACCESS_TOKEN = jwt({ exp: Math.floor(Date.now() / 1_000) + 3_600 });
const ID_TOKEN = jwt({
  exp: Math.floor(Date.now() / 1_000) + 3_600,
  "https://api.openai.com/auth": {
    chatgpt_account_id: ACCOUNT_ID,
    chatgpt_account_is_fedramp: false,
  },
});
const ORIGINAL_ENV = { ...process.env };

let server: Server;
let sockets: WebSocketServer;
let baseUrl: string;
let currentRefreshToken = "local-refresh-token";
let refreshCount = 0;
let modelRequests = 0;
let nextResponse = 1;
let rejectNextSubscriptionUpgrade = false;
let responseDelayMs = 0;
let responseGate: Promise<void> | undefined;
let authActorKey = "subscription";

beforeAll(async () => {
  server = createServer(handleHttp);
  sockets = new WebSocketServer({ noServer: true, maxPayload: 32 * 1024 * 1024 });
  server.on("upgrade", (request, socket, head) => {
    const subscription = request.url?.includes("/backend-api/codex/responses") === true;
    if (subscription && rejectNextSubscriptionUpgrade) {
      rejectNextSubscriptionUpgrade = false;
      socket.end("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
      return;
    }
    if (subscription && (
      request.headers.authorization !== `Bearer ${VALID_ACCESS_TOKEN}`
      || request.headers["chatgpt-account-id"] !== ACCOUNT_ID
    )) {
      socket.end("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
      return;
    }
    sockets.handleUpgrade(request, socket, head, (websocket) => sockets.emit("connection", websocket, request));
  });
  sockets.on("connection", (socket) => {
    socket.on("message", async (data) => {
      modelRequests += 1;
      const request = JSON.parse(data.toString("utf8")) as Record<string, unknown>;
      const encoded = JSON.stringify(request.input ?? []);
      const exactToken = encoded.match(/Reply with exactly ([A-Z0-9_-]{1,128})/)?.[1];
      const asksForHistory = encoded.includes("What exact token did I ask you to return previously?");
      const hasHistory = encoded.includes("Reply with exactly EDGE_OK");
      if (responseDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, responseDelayMs));
      if (responseGate) await responseGate;
      const text = asksForHistory
        ? (hasHistory ? "EDGE_OK" : "HISTORY_MISSING")
        : (exactToken ?? "RIVET_OK");
      socket.send(JSON.stringify({
        type: "response.completed",
        response: {
          id: `rivet-mock-${nextResponse++}`,
          status: "completed",
          output: [{
            type: "message",
            role: "assistant",
            content: [{ type: "output_text", text }],
          }],
          usage: null,
        },
      }));
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.listen(0, "127.0.0.1", resolve);
    server.once("error", reject);
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("mock server did not bind a TCP port");
  baseUrl = `http://127.0.0.1:${address.port}`;
});

afterAll(async () => {
  process.env = ORIGINAL_ENV;
  for (const socket of sockets.clients) socket.terminate();
  await new Promise<void>((resolve) => sockets.close(() => resolve()));
  await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  await registry.shutdown();
});

describe.sequential("Nanocodex Rivet Actors", () => {
  test("runs commands against the actor-owned persistent AgentOS filesystem", async (context) => {
    resetMock();
    configureApiKey();
    const { client } = await setupTest(context, registry);
    const session = client.nanocodex.getOrCreate([`agentos-${crypto.randomUUID()}`]);

    const executed = await session.exec("printf AGENTOS_OK > /workspace/probe.txt", {
      cwd: "/workspace",
      captureStdio: true,
    });
    expect(executed.exitCode).toBe(0);
    expect(Buffer.from(await session.readFile("/workspace/probe.txt")).toString("utf8"))
      .toBe("AGENTOS_OK");
  });

  test("uses the Rust journal to deduplicate, unload, and restore a WASM session", async (context) => {
    resetMock();
    configureApiKey();
    const { client } = await setupTest(context, registry);
    const session = client.nanocodex.getOrCreate([`api-${crypto.randomUUID()}`]);
    const connection = session.connect();
    const completedEvents: string[] = [];
    connection.on("turnCompleted", (event) => completedEvents.push(event.id));
    await connection.ready;

    const request = { id: "turn-1", input: "Reply with exactly EDGE_OK and nothing else." };
    const [first, duplicate] = await Promise.all([session.turn(request), session.turn(request)]);
    expect(first.final_message).toBe("EDGE_OK");
    expect(duplicate).toEqual(first);
    const requestsAfterFirstTurn = modelRequests;
    expect(requestsAfterFirstTurn).toBeGreaterThanOrEqual(1);
    await expect(session.turn({ ...request, input: "different" })).rejects.toThrow(/different input/);

    const replay = await session.turn(request);
    expect(replay).toEqual(first);
    expect(modelRequests).toBe(requestsAfterFirstTurn);

    await session.unload();
    expect((await session.status()).agent_loaded).toBe(false);
    const restored = await session.turn({
      id: "turn-2",
      input: "What exact token did I ask you to return previously? Reply with only that token.",
    });
    expect(restored.final_message).toBe("EDGE_OK");
    expect(await session.status()).toMatchObject({
      has_snapshot: true,
      completed_turns: 2,
      agent_loaded: true,
      auth_mode: "api_key",
    });
    expect(completedEvents).toEqual(["turn-1", "turn-2"]);
    await connection.dispose();
  });

  test("keeps a started turn alive without a client waiter", async (context) => {
    resetMock();
    configureApiKey();
    responseDelayMs = 100;
    const { client } = await setupTest(context, registry);
    const session = client.nanocodex.getOrCreate([`detached-${crypto.randomUUID()}`]);
    const request = {
      id: "detached-turn",
      input: "Reply with exactly DETACHED_OK",
    };

    await expect(session.start(request)).resolves.toEqual({
      type: "turn_accepted",
      id: request.id,
      input: request.input,
      replayed: false,
    });
    expect((await session.status()).active_turns).toContain(request.id);

    await expect(session.start({ ...request, input: "different" }))
      .rejects.toThrow(/different input/);
    const replayed = await session.start(request);
    expect(replayed.replayed).toBe(true);
    await vi.waitFor(async () => {
      expect(await session.status()).toMatchObject({
        active_turns: [],
        completed_turns: 1,
      });
    });
    const requestsAfterDetachedCompletion = modelRequests;
    expect(requestsAfterDetachedCompletion).toBeGreaterThan(0);

    await session.start(request);
    const completed = await session.turn(request);
    expect(completed.final_message).toBe("DETACHED_OK");
    expect((await session.status()).active_turns).not.toContain(request.id);
    expect(modelRequests).toBe(requestsAfterDetachedCompletion);
  });

  test("synchronizes accepted turns and durable results across connected clients", async (context) => {
    resetMock();
    configureApiKey();
    responseDelayMs = 100;
    const { client } = await setupTest(context, registry);
    const session = client.nanocodex.getOrCreate([`multiclient-${crypto.randomUUID()}`]);
    const first = session.connect();
    const second = session.connect();
    const firstAccepted: string[] = [];
    const secondAccepted: string[] = [];
    const firstCompleted: string[] = [];
    const secondCompleted: string[] = [];
    first.on("turnAccepted", (turn) => firstAccepted.push(`${turn.id}:${turn.input}`));
    second.on("turnAccepted", (turn) => secondAccepted.push(`${turn.id}:${turn.input}`));
    first.on("turnCompleted", (turn) => firstCompleted.push(`${turn.id}:${turn.final_message}`));
    second.on("turnCompleted", (turn) => secondCompleted.push(`${turn.id}:${turn.final_message}`));
    await Promise.all([first.ready, second.ready]);

    const request = {
      id: "shared-turn",
      input: "Reply with exactly SYNC_OK",
    };
    await session.start(request);
    await vi.waitFor(() => {
      expect(firstAccepted).toEqual(["shared-turn:Reply with exactly SYNC_OK"]);
      expect(secondAccepted).toEqual(firstAccepted);
    });
    expect(await session.status()).toMatchObject({
      active_turns: [request.id],
      active_turn_details: [request],
    });

    const completed = await session.turn(request);
    expect(completed.final_message).toBe("SYNC_OK");
    await vi.waitFor(() => {
      expect(firstCompleted).toEqual(["shared-turn:SYNC_OK"]);
      expect(secondCompleted).toEqual(firstCompleted);
    });
    const requestsAfterCompletion = modelRequests;
    expect((await session.turn(request)).final_message).toBe("SYNC_OK");
    expect(modelRequests).toBe(requestsAfterCompletion);
    await Promise.all([first.dispose(), second.dispose()]);
  });

  test("reconstructs an active prompt for a client that reconnects mid-turn", async (context) => {
    resetMock();
    configureApiKey();
    responseDelayMs = 1_000;
    const { client } = await setupTest(context, registry);
    const session = client.nanocodex.getOrCreate([`reconnect-${crypto.randomUUID()}`]);
    const first = session.connect();
    await first.ready;
    const request = {
      id: "reconnected-turn",
      input: "Reply with exactly RECONNECTED_OK",
    };
    await session.start(request);
    await first.dispose();

    const reconnected = session.connect();
    const completed: string[] = [];
    reconnected.on("turnCompleted", (turn) => completed.push(`${turn.id}:${turn.final_message}`));
    await reconnected.ready;
    expect(await reconnected.status()).toMatchObject({
      active_turns: [request.id],
      active_turn_details: [request],
    });
    expect((await reconnected.turn(request)).final_message).toBe("RECONNECTED_OK");
    await vi.waitFor(() => {
      expect(completed).toEqual(["reconnected-turn:RECONNECTED_OK"]);
    });
    await reconnected.dispose();
  });

  test("single-flights rotating subscription credentials and retries a rejected upgrade", async (context) => {
    resetMock();
    configureSubscription();
    const { client } = await setupTest(context, registry);
    const auth = client.nanocodexAuth.getOrCreate([authActorKey]);

    await expect(auth.snapshot({ at_ms: Date.now(), nonce: "0".repeat(32), mac: "0".repeat(64) }))
      .rejects.toThrow(/internal error/i);
    const oneTimeProof = createCapabilityProof("snapshot");
    expect((await auth.snapshot(oneTimeProof)).revision).toBe(1);
    await expect(auth.snapshot(oneTimeProof)).rejects.toThrow(/internal error/i);
    const snapshots = await Promise.all(
      Array.from({ length: 64 }, () => auth.snapshot(createCapabilityProof("snapshot"))),
    );
    expect(new Set(snapshots.map((snapshot) => snapshot.revision))).toEqual(new Set([1]));
    expect(refreshCount).toBe(1);

    rejectNextSubscriptionUpgrade = true;
    const session = client.nanocodex.getOrCreate([`subscription-${crypto.randomUUID()}`]);
    const result = await session.turn({ id: "subscription-turn", input: "Reply with exactly SUB_OK" });
    expect(result.final_message).toBe("SUB_OK");
    expect(refreshCount).toBe(2);
    expect(await auth.status(createCapabilityProof("status"))).toMatchObject({
      configured: true,
      account_id: ACCOUNT_ID,
      revision: 2,
    });
    expect((await session.status()).auth_mode).toBe("chatgpt");
  });

  test("runs a disposable subscription deployment without copying the refresh token", async (context) => {
    resetMock();
    configureAccessTokenOnlySubscription();
    const { client } = await setupTest(context, registry);
    const auth = client.nanocodexAuth.getOrCreate([authActorKey]);

    expect(await auth.snapshot(createCapabilityProof("snapshot"))).toMatchObject({
      accountId: ACCOUNT_ID,
      revision: 0,
    });
    const session = client.nanocodex.getOrCreate([`access-only-${crypto.randomUUID()}`]);
    const result = await session.turn({
      id: "access-only-turn",
      input: "Reply with exactly ACCESS_ONLY_OK",
    });
    expect(result.final_message).toBe("ACCESS_ONLY_OK");
    await expect(auth.recover(createCapabilityProof("recover:0"), 0))
      .rejects.toThrow(/internal error/i);
    expect(refreshCount).toBe(0);
  });

  test("bounds prompt size and active turn fan-in", async (context) => {
    resetMock();
    configureApiKey();
    const { client } = await setupTest(context, registry);
    const session = client.nanocodex.getOrCreate([`bounds-${crypto.randomUUID()}`]);
    await expect(session.turn({ id: "oversized", input: "x".repeat(1024 * 1024 + 1) }))
      .rejects.toThrow(/exceeds 1 MiB/);

    let releaseResponses = () => {};
    responseGate = new Promise<void>((resolve) => {
      releaseResponses = resolve;
    });
    const turns = Array.from({ length: 16 }, (_, index) => session.turn({
      id: `bounded-${index}`,
      input: `Reply with exactly BOUNDED_${index}`,
    }));
    try {
      await vi.waitFor(async () => {
        expect((await session.status()).active_turns).toHaveLength(16);
      }, { timeout: 10_000 });
      await expect(session.turn({
        id: "bounded-overflow",
        input: "Reply with exactly BOUNDED_OVERFLOW",
      })).rejects.toThrow(/at most 16 turns/);
    } finally {
      releaseResponses();
      responseGate = undefined;
    }
    await expect(Promise.all(turns)).resolves.toHaveLength(16);
  });
});

async function handleHttp(request: IncomingMessage, response: import("node:http").ServerResponse): Promise<void> {
  if (request.method !== "POST" || request.url !== "/oauth/token") {
    response.writeHead(404).end();
    return;
  }
  const body = JSON.parse(await readBody(request, 16 * 1024)) as Record<string, unknown>;
  if (body.grant_type !== "refresh_token" || body.refresh_token !== currentRefreshToken) {
    response.writeHead(401, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: { code: "refresh_token_invalidated" } }));
    return;
  }
  currentRefreshToken = `rotated-${++refreshCount}`;
  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify({
    access_token: VALID_ACCESS_TOKEN,
    refresh_token: currentRefreshToken,
    id_token: ID_TOKEN,
  }));
}

function configureApiKey(): void {
  process.env.NANOCODEX_AUTH_MODE = "api_key";
  process.env.OPENAI_API_KEY = "test-api-key";
  process.env.OPENAI_WEBSOCKET_URL = `${baseUrl.replace("http:", "ws:")}/v1/responses`;
}

function configureSubscription(): void {
  process.env.NANOCODEX_AUTH_MODE = "chatgpt";
  process.env.OPENAI_WEBSOCKET_URL = `${baseUrl.replace("http:", "ws:")}/backend-api/codex/responses`;
  process.env.CHATGPT_ACCESS_TOKEN = jwt({ exp: Math.floor(Date.now() / 1_000) - 60 });
  process.env.CHATGPT_REFRESH_TOKEN = "local-refresh-token";
  process.env.CHATGPT_ACCOUNT_ID = ACCOUNT_ID;
  process.env.CHATGPT_TOKEN_ENDPOINT = `${baseUrl}/oauth/token`;
  process.env.NANOCODEX_AUTH_ACTOR_KEY = authActorKey;
  process.env.NANOCODEX_AUTH_CAPABILITY = AUTH_CAPABILITY;
  delete process.env.OPENAI_API_KEY;
}

function configureAccessTokenOnlySubscription(): void {
  configureSubscription();
  process.env.CHATGPT_ACCESS_TOKEN = VALID_ACCESS_TOKEN;
  delete process.env.CHATGPT_REFRESH_TOKEN;
}

function resetMock(): void {
  currentRefreshToken = "local-refresh-token";
  refreshCount = 0;
  modelRequests = 0;
  nextResponse = 1;
  rejectNextSubscriptionUpgrade = false;
  responseDelayMs = 0;
  authActorKey = `subscription-${crypto.randomUUID()}`;
}

function jwt(payload: Record<string, unknown>): string {
  const encode = (value: Record<string, unknown>) => Buffer.from(JSON.stringify(value)).toString("base64url");
  return `${encode({ alg: "none" })}.${encode(payload)}.local`;
}

async function readBody(request: IncomingMessage, limit: number): Promise<string> {
  const chunks: Buffer[] = [];
  let bytes = 0;
  for await (const value of request) {
    const chunk = Buffer.isBuffer(value) ? value : Buffer.from(value);
    bytes += chunk.byteLength;
    if (bytes > limit) throw new Error(`request exceeded ${limit} bytes`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}
