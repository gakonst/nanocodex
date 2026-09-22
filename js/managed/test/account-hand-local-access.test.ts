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
