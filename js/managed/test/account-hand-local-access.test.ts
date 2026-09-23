import { expect, it } from "vitest";
import { REMOTE_VM_ASSERTION } from "../src/hand-remote";
import { routeManaged } from "../../account/worker/managedProxy";
import { createManagedAccessClaims, signManagedAccessClaims } from "nanocodex/cloudflare/managed-access";
it("account-local viewer admission preserves a real upgraded socket and first ready message", async () => {
  const secret = "local-account-snapshot-fixture-thirty-two-bytes";
  const source = new Request("https://account.test/v1/account/hands/screens", { headers: { authorization: "Bearer fixture" } });
  const principal = { kind: "api_key" as const, userId: "owner", organizationId: "org", teamId: "team", authorizationEpoch: 1, capabilities: ["agents:read", "tools:use"] };
  const token = await signManagedAccessClaims(await createManagedAccessClaims(source, principal), { NANOCODEX_ACCESS_SECRET: secret });
  const request = new Request("https://account.test/v1/account/hands/view", { headers: { authorization: "Bearer fixture", upgrade: "websocket", "x-nanocodex-access": token, [REMOTE_VM_ASSERTION]: "forged" } });
  const pair = new WebSocketPair(); pair[1].accept(); pair[1].send('ready');
  const response = await routeManaged(request, { NANOCODEX_ACCESS_SECRET: secret,
    NANOCODEX_BACKEND: { fetch() { throw new Error("unexpected managed hop"); } } as unknown as Fetcher,
    NANOCODEX_HAND_BROKER: { getByName(owner: string) { expect(owner).toBe("owner"); return { fetch: async (forwarded: Request) => { expect(forwarded.headers.has(REMOTE_VM_ASSERTION)).toBe(false); return new Response(null, { status: 101, webSocket: pair[0] }); } }; } } as unknown as DurableObjectNamespace,
  }, new URL(request.url));
  expect(response?.status).toBe(101); expect(response?.webSocket).toBe(pair[0]);
  const socket = response!.webSocket!; socket.accept();
  const received = await new Promise(resolve => socket.addEventListener("message", event => resolve(event.data), { once: true }));
  expect(received).toBe("ready"); socket.close(); pair[1].close();
});

it("browser discovery cookie admits fresh real viewer sockets with the original session and no header", async () => {
  const secret = "browser-account-snapshot-fixture-thirty-two-bytes";
  const source = new Request("https://account.test/v1/account/hands/screens", { headers: { cookie: "nanocodex_account=browser-fixture" } });
  const principal = { kind: "account_session" as const, userId: "owner", organizationId: "org", teamId: "team", authorizationEpoch: 1, capabilities: ["agents:read", "tools:use"] };
  const token = await signManagedAccessClaims(await createManagedAccessClaims(source, principal), { NANOCODEX_ACCESS_SECRET: secret });
  let managedCalls = 0;
  const peers: WebSocket[] = [];
  const sockets: WebSocket[] = [];
  const env = { NANOCODEX_ACCESS_SECRET: secret,
    NANOCODEX_BACKEND: { async fetch(request: Request) {
      managedCalls++; expect(request).toBe(source);
      return Response.json({ surfaces: [] }, { headers: { "x-nanocodex-access": token, "x-nanocodex-access-ttl-ms": "119000" } });
    } } as unknown as Fetcher,
    NANOCODEX_HAND_BROKER: { getByName(owner: string) {
      expect(owner).toBe("owner");
      return { fetch: async (forwarded: Request) => {
        expect(forwarded.headers.has("x-nanocodex-access")).toBe(false);
        expect(forwarded.headers.has("authorization")).toBe(false);
        expect(forwarded.headers.get("cookie")).toContain("nanocodex_account=browser-fixture");
        const pair = new WebSocketPair(); pair[1].accept(); peers.push(pair[1]);
        pair[1].send(JSON.stringify({ type: "ready", connection_id: crypto.randomUUID() }));
        return new Response(null, { status: 101, webSocket: pair[0] });
      } };
    } } as unknown as DurableObjectNamespace,
  };
  try {
    const discovery = await routeManaged(source, env, new URL(source.url));
    const cookie = discovery!.headers.get("set-cookie")!.split(";")[0];
    expect(cookie).toBe(`__Secure-nanocodex_hand_access=${token}`);
    const connections = new Set<string>();
    for (let i = 0; i < 2; i++) {
      const request = new Request("https://account.test/v1/account/hands/view", { headers: {
        cookie: `${source.headers.get("cookie")}; ${cookie}`, origin: "https://account.test", upgrade: "websocket",
      } });
      const response = await routeManaged(request, env, new URL(request.url));
      expect(response!.status).toBe(101); expect(response!.headers.has("set-cookie")).toBe(false);
      expect(response!.headers.get("server-timing")).toContain('desc="access"');
      const socket = response!.webSocket!; sockets.push(socket); socket.accept();
      const ready = await new Promise<string>(resolve => socket.addEventListener("message", event => resolve(String(event.data)), { once: true }));
      connections.add(JSON.parse(ready).connection_id);
    }
    expect(managedCalls).toBe(1); expect(connections.size).toBe(2); expect(sockets[0]).not.toBe(sockets[1]);
  } finally { for (const socket of [...sockets, ...peers]) socket.close(); }
});
