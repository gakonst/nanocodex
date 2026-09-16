import { env } from "cloudflare:workers";
import { SELF } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { chatGptLimitReset } from "../src/chatgpt-failover";
import { handleEgress, type EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const { CHATGPT_EGRESS: _relay, ...directEnv } = workerEnv;

describe("ChatGPT subscription failover", () => {
  it("recognizes structured subscription limits and reset hints only", () => {
    const now = 1_800_000_000_000;
    expect(chatGptLimitReset({ error: { type: "usage_limit_reached", resets_at: now / 1_000 + 3600 } }, null, now))
      .toBe(now + 3_600_000);
    expect(chatGptLimitReset({ response: { error: { code: "usage_limit_exceeded", resets_in_seconds: 15 } } }, null, now))
      .toBe(now + 15_000);
    expect(chatGptLimitReset({ error: { code: "insufficient_quota" } }, "120", now)).toBe(now + 120_000);
    expect(chatGptLimitReset({ error: { code: "usage_limit_reached" } }, null, now)).toBe(now + 60_000);
    expect(chatGptLimitReset({ error: { code: "rate_limit_exceeded" } }, "120", now)).toBeUndefined();
    expect(chatGptLimitReset({ error: { message: "usage_limit_reached" } }, null, now)).toBeUndefined();
    expect(chatGptLimitReset(null)).toBeUndefined();
  });

  it("replays a rejected HTTP request on the next account and stays there", async () => {
    const subject = await setup();
    const accounts: (string | null)[] = [];
    const bodies: string[] = [];
    const upstream = vi.fn(async (request: Request) => {
      const account = request.headers.get("chatgpt-account-id");
      accounts.push(account);
      bodies.push(await request.text());
      return account === "account-b" ? exhausted() : Response.json({ results: [] });
    });
    for (let index = 0; index < 2; index++) {
      const response = await handleEgress(searchRequest(subject), directEnv, undefined, upstream as typeof fetch);
      expect(response.status).toBe(200);
      await response.body?.cancel();
    }
    expect(accounts).toEqual(["account-b", "account-a", "account-a"]);
    expect(new Set(bodies).size).toBe(1);
  });

  it("pins a backup account without changing other sessions and strips its private selector", async () => {
    const subject = await setup();
    const accounts: (string | null)[] = [];
    const upstream = vi.fn(async (request: Request) => {
      accounts.push(request.headers.get("chatgpt-account-id"));
      expect(request.headers.has("x-nanocodex-chatgpt-account-id")).toBe(false);
      return Response.json({ results: [] });
    });
    const pinned = searchRequest(subject);
    pinned.headers.set("x-nanocodex-chatgpt-account-id", "account-a");
    for (const request of [pinned, searchRequest(subject)]) {
      const response = await handleEgress(request, directEnv, undefined, upstream as typeof fetch);
      expect(response.status).toBe(200);
      await response.body?.cancel();
    }
    expect(accounts).toEqual(["account-a", "account-b"]);
  });

  it("fails closed for an unavailable pin and never switches an exhausted pinned session", async () => {
    const subject = await setup();
    const accounts: (string | null)[] = [];
    const upstream = vi.fn(async (request: Request) => {
      const account = request.headers.get("chatgpt-account-id");
      accounts.push(account);
      return account === "account-a" ? exhausted() : Response.json({ results: [] });
    });
    for (const [account, status, error] of [
      ["unknown-account", 409, "chatgpt_account_unavailable"],
      ["account-a", 429, "chatgpt_account_exhausted"],
      ["account-a", 429, "chatgpt_account_exhausted"],
    ] as const) {
      const request = searchRequest(subject);
      request.headers.set("x-nanocodex-chatgpt-account-id", account);
      const response = await handleEgress(request, directEnv, undefined, upstream as typeof fetch);
      expect(response.status).toBe(status);
      expect(await response.json()).toMatchObject({ error });
    }
    const other = await handleEgress(searchRequest(subject), directEnv, undefined, upstream as typeof fetch);
    expect(other.status).toBe(200);
    await other.body?.cancel();
    expect(accounts).toEqual(["account-a", "account-b"]);
  });

  it("preserves a pinned WebSocket quota error instead of requesting account recovery", async () => {
    const subject = await setup();
    const upstream = vi.fn(async (request: Request) => {
      expect(request.headers.get("chatgpt-account-id")).toBe("account-a");
      const [client, server] = Object.values(new WebSocketPair());
      server.accept();
      server.addEventListener("message", () => server.send(JSON.stringify({
        type: "error", error: { code: "usage_limit_reached" },
      })));
      return new Response(null, { status: 101, webSocket: client });
    });
    const request = socketRequest(subject);
    request.headers.set("x-nanocodex-chatgpt-account-id", "account-a");
    const response = await handleEgress(request, directEnv, undefined, upstream as typeof fetch);
    const socket = response.webSocket!;
    socket.accept();
    const rejected = message(socket);
    socket.send(JSON.stringify({ type: "response.create", input: [] }));
    expect(await rejected).toMatchObject({ type: "error", error: { code: "usage_limit_reached" } });
    socket.close();
  });

  it("does not rotate on an ordinary 429 or a denied request", async () => {
    for (const status of [429, 403]) {
      const subject = await setup();
      const accounts: (string | null)[] = [];
      const upstream = vi.fn(async (request: Request) => {
        accounts.push(request.headers.get("chatgpt-account-id"));
        return Response.json({ error: { code: "rate_limit_exceeded" } }, { status });
      });
      for (let index = 0; index < 2; index++) {
        const response = await handleEgress(searchRequest(subject), directEnv, undefined, upstream as typeof fetch);
        await response.body?.cancel();
      }
      expect(accounts).toEqual(["account-b", "account-b"]);
    }
  });

  it("bounds attempts when every account is exhausted", async () => {
    const subject = await setup();
    const upstream = vi.fn(async () => exhausted());
    const response = await handleEgress(searchRequest(subject), directEnv, undefined, upstream as typeof fetch);
    expect(response.status).toBe(429);
    await response.body?.cancel();
    expect(upstream).toHaveBeenCalledTimes(2);
    const again = await handleEgress(searchRequest(subject), directEnv, undefined, upstream as typeof fetch);
    expect(again.status).toBe(429);
    await again.body?.cancel();
    expect(upstream).toHaveBeenCalledTimes(2);
  });

  it("switches a live WebSocket and requests SDK full-history recovery", async () => {
    const subject = await setup();
    const accounts: (string | null)[] = [];
    const peers: WebSocket[] = [];
    const upstream = vi.fn(async (request: Request) => {
      const account = request.headers.get("chatgpt-account-id");
      accounts.push(account);
      const [client, server] = Object.values(new WebSocketPair());
      server.accept();
      peers.push(server);
      server.addEventListener("message", () => {
        server.send(JSON.stringify(account === "account-b"
          ? { type: "response.failed", response: { error: { code: "usage_limit_reached", resets_in_seconds: 3600 } } }
          : { type: "response.completed", response: { id: "response-a", output: [] } }));
      });
      return new Response(null, { status: 101, webSocket: client });
    });
    const open = async () => {
      const response = await handleEgress(socketRequest(subject), directEnv, undefined, upstream as typeof fetch);
      expect(response.status).toBe(101);
      const socket = response.webSocket!;
      socket.accept();
      return socket;
    };
    const first = await open();
    const rejected = message(first);
    first.send(JSON.stringify({ type: "response.create", input: [] }));
    expect(await rejected).toMatchObject({ type: "error", error: { code: "server_error", retry_after: 0 } });
    const retry = await open();
    const completed = message(retry);
    retry.send(JSON.stringify({ type: "response.create", input: [] }));
    expect(await completed).toMatchObject({ type: "response.completed" });
    expect(accounts).toEqual(["account-b", "account-a"]);
    retry.close();
    for (const peer of peers) if (peer.readyState === WebSocket.OPEN) peer.close();
  });

  it("preserves an exhaustion error after output has started", async () => {
    const subject = await setup();
    const upstream = vi.fn(async () => {
      const [client, server] = Object.values(new WebSocketPair());
      server.accept();
      server.addEventListener("message", () => {
        server.send(JSON.stringify({ type: "response.output_text.delta", delta: "partial" }));
        server.send(JSON.stringify({ type: "error", error: { code: "usage_limit_reached" } }));
      });
      return new Response(null, { status: 101, webSocket: client });
    });
    const response = await handleEgress(socketRequest(subject), directEnv, undefined, upstream as typeof fetch);
    const socket = response.webSocket!;
    socket.accept();
    const frames: unknown[] = [];
    const ended = new Promise<void>((resolve) => socket.addEventListener("message", (event) => {
      frames.push(JSON.parse(String(event.data)));
      if (frames.length === 2) resolve();
    }));
    socket.send(JSON.stringify({ type: "response.create", input: [] }));
    await ended;
    expect(frames[1]).toMatchObject({ type: "error", error: { code: "usage_limit_reached" } });
    socket.close();
  });
});

async function setup(): Promise<string> {
  const user = crypto.randomUUID();
  const subject = user.replaceAll("-", "").repeat(2);
  const bound = await SELF.fetch(`https://broker.internal/subjects/${subject}`, {
    method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ user_id: user }),
  });
  expect(bound.status).toBe(200);
  await bound.body?.cancel();
  for (const account of ["account-a", "account-b"]) {
    const expiresAt = Math.ceil(Date.now() / 1_000) * 1_000 + 3_600_000;
    const token = `${btoa('{}').replace(/=+$/, '')}.${btoa(JSON.stringify({ exp: expiresAt / 1_000,
      "https://api.openai.com/auth": { chatgpt_account_id: account, chatgpt_account_is_fedramp: false },
    })).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")}.signature`;
    const imported = await SELF.fetch(`https://broker.internal/users/${user}/credentials/chatgpt`, {
      method: "PUT", headers: { "content-type": "application/json" },
      body: JSON.stringify({ access_token: token, refresh_token: `refresh-${account}`,
        account_id: account, fedramp: false, expires_at: expiresAt }),
    });
    expect(imported.status).toBe(204);
  }
  return subject;
}
function searchRequest(subject: string): Request {
  return new Request("https://nanocodex.internal/v1/search", { method: "POST", headers: {
    authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json",
    "x-nanocodex-subject": subject,
  }, body: JSON.stringify({ query: "test" }) });
}
function socketRequest(subject: string): Request {
  return new Request("https://nanocodex.internal/v1/responses", { headers: {
    authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "openai-beta": "responses_websockets=2026-02-06",
    upgrade: "websocket", "x-nanocodex-subject": subject,
  } });
}
function exhausted(): Response {
  return Response.json({ error: { type: "usage_limit_reached", resets_in_seconds: 3600 } }, { status: 429 });
}
function message(socket: WebSocket): Promise<unknown> {
  return new Promise((resolve) => socket.addEventListener("message", (event) => resolve(JSON.parse(String(event.data))), { once: true }));
}
