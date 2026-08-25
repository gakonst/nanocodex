import { env, SELF } from "cloudflare:test";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import type { Env } from "../src/index";

const testEnv = env as unknown as Env;
const USER_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const API_KEY = `ncx_live_${"d".repeat(12)}_${"h".repeat(43)}`;
const createdAgents = new Set<string>();

beforeAll(async () => seedApiKey());

afterAll(async () => {
  await Promise.all([...createdAgents].map(async (agentId) => {
    await authenticatedFetch(`https://example.test/v1/agents/${agentId}`, { method: "DELETE" });
  }));
});

describe("managed Android device host", () => {
  it("authenticates the endpoint and monotonically fences a replaced host", async () => {
    const agentId = await createAgent();
    const endpoint = `https://example.test/v1/agents/${agentId}/device-host`;

    const unauthenticated = await SELF.fetch(endpoint, {
      headers: { upgrade: "websocket" },
    });
    expect(unauthenticated.status).toBe(401);

    const first = await upgrade(endpoint);
    const firstLeaseMessage = nextMessage(first);
    first.send(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: "11111111-1111-4111-8111-111111111111",
      catalog_version: 1,
    }));
    const firstLease = await firstLeaseMessage as {
      type: string;
      lease_id: string;
      epoch: number;
      expires_at: number;
    };
    expect(firstLease).toMatchObject({ type: "lease", epoch: 1 });
    expect(firstLease.expires_at).toBeGreaterThan(Date.now());

    const pongMessage = nextMessage(first);
    first.send(JSON.stringify({
      type: "ping",
      lease_id: firstLease.lease_id,
      epoch: firstLease.epoch,
      nonce: "device-heartbeat",
    }));
    expect(await pongMessage).toMatchObject({
      type: "pong",
      lease_id: firstLease.lease_id,
      epoch: 1,
      nonce: "device-heartbeat",
    });

    const fencedMessage = nextMessage(first);
    const second = await upgrade(endpoint);
    const secondLeaseMessage = nextMessage(second);
    second.send(JSON.stringify({
      type: "attach",
      protocol_version: 1,
      host_id: "22222222-2222-4222-8222-222222222222",
      catalog_version: 3,
    }));
    expect(await fencedMessage).toMatchObject({
      type: "fenced",
      epoch: 2,
    });
    expect(await secondLeaseMessage).toMatchObject({
      type: "lease",
      epoch: 2,
      catalog_version: 3,
    });
    second.close(1000, "test complete");
  });

});

async function createAgent(): Promise<string> {
  const created = await authenticatedFetch("https://example.test/v1/agents", { method: "POST" });
  expect(created.status).toBe(201);
  const { agent_id: agentId } = await created.json<{ agent_id: string }>();
  createdAgents.add(agentId);
  return agentId;
}

async function upgrade(endpoint: string): Promise<WebSocket> {
  const response = await authenticatedFetch(endpoint, {
    headers: { upgrade: "websocket" },
  });
  expect(response.status).toBe(101);
  expect(response.webSocket).toBeTruthy();
  response.webSocket!.accept();
  return response.webSocket!;
}

function nextMessage(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timed out waiting for device-host message")), 2_000);
    socket.addEventListener("message", (event) => {
      clearTimeout(timeout);
      try {
        resolve(JSON.parse(String(event.data)) as Record<string, unknown>);
      } catch (error) {
        reject(error);
      }
    }, { once: true });
  });
}

async function authenticatedFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const request = new Request(input, init);
  const headers = new Headers(request.headers);
  headers.set("authorization", `Bearer ${API_KEY}`);
  return SELF.fetch(new Request(request, { headers }));
}

async function seedApiKey(): Promise<void> {
  const digestBytes = new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(API_KEY)),
  );
  let binary = "";
  for (const byte of digestBytes) binary += String.fromCharCode(byte);
  const digest = btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const account = testEnv.NANOCODEX_USERS.getByName(USER_ID);
  const provisioned = await account.fetch("https://user.internal/account", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ id: USER_ID, persistent: true }),
  });
  expect(provisioned.ok).toBe(true);
  const key = testEnv.NANOCODEX_API_KEYS.getByName(digest);
  await key.fetch("https://api-key.internal/record", { method: "DELETE" });
  const record = await key.fetch("https://api-key.internal/record", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      id: "d".repeat(12),
      label: "device-host-test",
      prefix: API_KEY.slice(0, "ncx_live_".length + 12),
      createdAt: Date.now(),
      digest,
      userId: USER_ID,
    }),
  });
  expect(record.status).toBe(201);
}
