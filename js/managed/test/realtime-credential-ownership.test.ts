import { describe, expect, it, vi } from "vitest";

vi.mock("../src/account-auth", async (load) => {
  const actual = await load<typeof import("../src/account-auth")>();
  const principal = {
    kind: "account_session", userId: "11111111-1111-4111-8111-111111111111",
    organizationId: "22222222-2222-4222-8222-222222222222",
    teamId: "33333333-3333-4333-8333-333333333333", authorizationEpoch: 1, capabilities: [],
  };
  return { ...actual,
    authenticate: async () => principal,
    authenticatePersistentAccount: async () => principal,
    requireSameOriginMutation: () => undefined,
  };
});

import { routeManagedRealtimeTransport } from "../src/managed-realtime-transport";
import { routeBrowserModel } from "../src/browser-model";

const agentId = "018f25e8-7b51-7a32-8c4d-0123456789ab";
const voiceId = "018f25e8-7b51-7a32-8c4d-0123456789ac";
const storageId = "a".repeat(64);

function fixture(direct: boolean) {
  const subject = direct ? `managed-session-v1_${storageId}` : storageId;
  const requests: Request[] = [];
  const ownership = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init);
    expect(request.url).toBe("https://session.internal/credential-subject");
    expect(request.headers.get("x-nanocodex-owner-id")).toBe("11111111-1111-4111-8111-111111111111");
    return Response.json({ subject, strategy: direct ? "session_v1" : "directory_v1" });
  });
  const env = {
    NANOCODEX_SESSIONS: {
      idFromName: () => ({ toString: () => storageId }),
      get: () => ({ fetch: ownership }),
    },
    NANOCODEX: { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input, init);
      requests.push(request);
      return request.url.startsWith("https://broker.internal/subjects/")
        ? new Response(null, { status: 204 })
        : Response.json({ error: "agent_subject_unavailable" }, { status: 403 });
    } },
  };
  return { env, subject, ownership, requests };
}

describe("Realtime retained credential strategy", () => {
  for (const direct of [false, true]) {
    it(`uses the retained subject on managed voice transport (direct=${direct})`, async () => {
      const { env, subject, requests } = fixture(direct);
      const url = new URL(`https://nanocodex.example/v1/agents/${agentId}/realtime/sideband?call_id=rtc_test&voice_session_id=${voiceId}`);
      const response = await routeManagedRealtimeTransport(new Request(url, {
        headers: { upgrade: "websocket", origin: url.origin },
      }), env as unknown as Parameters<typeof routeManagedRealtimeTransport>[1], url, 1_000);
      expect(response?.status).toBe(403);
      const binds = requests.filter(({ url }) => url.startsWith("https://broker.internal/subjects/"));
      expect(binds).toHaveLength(direct ? 0 : 1);
      expect(requests.at(-1)?.headers.get("x-nanocodex-subject")).toBe(subject);
    });

    it(`does not repair a Session denial through browser voice's legacy directory (direct=${direct})`, async () => {
      const { env, subject, requests } = fixture(direct);
      const url = new URL("https://nanocodex.internal/v1/realtime/sideband");
      const response = await routeBrowserModel(new Request(url, { headers: {
        "x-nanocodex-agent-id": agentId, "x-session-id": voiceId,
        "session-id": voiceId, "thread-id": voiceId, upgrade: "websocket",
      } }), env as unknown as Parameters<typeof routeBrowserModel>[1], url);
      expect(response?.status).toBe(403);
      const binds = requests.filter(({ url }) => url.startsWith("https://broker.internal/subjects/"));
      expect(binds).toHaveLength(direct ? 0 : 1);
      const models = requests.filter(({ url }) => url.startsWith("https://nanocodex.internal/"));
      expect(models).toHaveLength(direct ? 1 : 2);
      expect(models.every((request) => request.headers.get("x-nanocodex-subject") === subject)).toBe(true);
    });
  }
});
