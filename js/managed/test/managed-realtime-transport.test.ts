import { describe, expect, it, vi } from "vitest";
import { validRealtimeSession, voiceRelayRegion, routeManagedRealtimeTransport } from "../src/managed-realtime-transport";

const session = () => ({
  model: "gpt-live-1-codex", instructions: "Use the user's ChatGPT subscription.",
  audio: { output: { voice: "maple" } }, delegation: { type: "client" },
});

describe("ChatGPT subscription voice call boundary", () => {
  it("accepts the Codex acknowledgement option and preserves provider defaults", () => {
    expect(validRealtimeSession(session())).toBe(true);
    for (const ack_filler of [true, false]) {
      expect(validRealtimeSession({ ...session(), delegation: { type: "client", ack_filler } })).toBe(true);
    }
  });
  it("accepts full instructions beyond the former 32 KiB cutoff", () => {
    expect(validRealtimeSession({ ...session(), instructions: "x".repeat(96 * 1024) })).toBe(true);
  });
  it("rejects malformed acknowledgements and arbitrary provider fields", () => {
    for (const ack_filler of [null, "false", 0, {}, []]) {
      expect(validRealtimeSession({ ...session(), delegation: { type: "client", ack_filler } })).toBe(false);
    }
    expect(validRealtimeSession({ ...session(), delegation: { type: "server", ack_filler: true } })).toBe(false);
    expect(validRealtimeSession({ ...session(), delegation: { type: "client", ack_filler: true, instructions: "override" } })).toBe(false);
  });
  it("rejects Platform audio options, custom voices, and other models", () => {
    expect(validRealtimeSession({ ...session(), audio: { input: { turn_detection: { type: "semantic_vad" } }, output: { voice: "maple" } } })).toBe(false);
    expect(validRealtimeSession({ ...session(), audio: { output: { voice: "maple", speed: 1.5 } } })).toBe(false);
    for (const voice of ["alloy", "voice_custom", { id: "voice_custom" }]) {
      expect(validRealtimeSession({ ...session(), audio: { output: { voice } } })).toBe(false);
    }
    expect(validRealtimeSession({ ...session(), model: "gpt-realtime" })).toBe(false);
  });
});


describe("voice relay geography", () => {
  it.each([
    ["NA", "-122.4", "wnam"], ["NA", "-74", "enam"],
    ["EU", "2.3", "weur"], ["EU", "23.7", "eeur"],
    ["AS", "139", "apac"], ["SA", "-46", "sam"], ["OC", "151", "oc"],
    ["AF", "30", undefined], ["NA", "", undefined], ["EU", "bad", undefined],
  ])("uses trusted %s/%s metadata", (continent, longitude, expected) => {
    expect(voiceRelayRegion({ cf: { continent, longitude } } as Request)).toBe(expected);
  });
  it("does not accept a caller's placement header without Cloudflare metadata", () => {
    expect(voiceRelayRegion(new Request("https://test.example", {
      headers: { "x-nanocodex-voice-region": "wnam", "cf-ipcontinent": "NA" },
    }))).toBeUndefined();
  });
});

describe("private voice egress admission", () => {
  async function fixture(owned: boolean, privateBinding = true, direct = true, sideband = false, rpc = false, callRpc = false, accountId?: string) {
    const owner = "11111111-1111-4111-8111-111111111111";
    const id = "a".repeat(64);
    const token = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
    const digest = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest(
      "SHA-256", new TextEncoder().encode(token),
    )))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
    const relay = vi.fn(async (_request: Request) => new Response("v=0", { status: 201 }));
    const createCall = vi.fn(async (_body: string, _headers: Record<string, string>) => ({
      status: 201, body: "v=0", headers: { location: "/v1/realtime/calls/rtc_fixture", "set-cookie": "private" },
    }));
    const generic = vi.fn(async (_request: RequestInfo | URL, _init?: RequestInit) => new Response("v=0", { status: 201 }));
    const ownership = vi.fn(async (_url: string, _init: RequestInit) => owned
      ? Response.json({ subject: direct ? `managed-session-v1_${id}` : id, strategy: direct ? "session_v1" : "directory_v1", ...(accountId ? { chatgpt_account_id: accountId } : {}) })
      : new Response(null, { status: 404 }));
    const resolveCredentialSubject = vi.fn(async (assertions: Record<string, string>) => {
      expect(assertions["x-nanocodex-owner-id"]).toBe(owner);
      expect(assertions["x-nanocodex-session-organization-id"]).toBe("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
      return owned ? { subject: direct ? `managed-session-v1_${id}` : id, strategy: direct ? "session_v1" : "directory_v1", ...(accountId ? { chatgpt_account_id: accountId } : {}) } : undefined;
    });
    const env = {
      NANOCODEX_API_KEYS: { getByName: () => ({ fetch: async () => Response.json({
        id: "k".repeat(12), prefix: `ncx_live_${"k".repeat(12)}`, label: "voice", digest,
        createdAt: 1, userId: owner, organizationId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        teamId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc", role: "writer",
        authorizationEpoch: 1, capabilities: ["agents:write"],
      }, { headers: { "x-nanocodex-api-key-authorized": "1" } }) }) },
      NANOCODEX_SESSIONS: { idFromName: () => ({ toString: () => id }), get: () => ({ fetch: ownership, ...(rpc ? { resolveCredentialSubject } : {}) }) },
      NANOCODEX: { fetch: generic },
      ...(privateBinding ? { NANOCODEX_REALTIME: { fetch: relay, ...(callRpc ? { createCall } : {}) } } : {}),
    } as unknown as Parameters<typeof routeManagedRealtimeTransport>[1];
    const url = new URL("https://test.example/v1/agents/11111111-1111-7111-8111-111111111111/realtime/calls");
    if (sideband) {
      url.pathname = url.pathname.replace(/calls$/, "sideband");
      url.search = "call_id=rtc_fixture&voice_session_id=22222222-2222-7222-8222-222222222222";
    }
    const request = new Request(url, {
      method: sideband ? "GET" : "POST", headers: {
        ...(sideband ? { upgrade: "websocket" } : {}),
        authorization: `Bearer ${token}`, "content-type": "application/json",
        "x-nanocodex-voice-session-id": "22222222-2222-7222-8222-222222222222",
        "x-nanocodex-realtime-owner": "attacker", "x-nanocodex-voice-region": "wnam",
      }, ...(sideband ? {} : { body: JSON.stringify({ sdp: "v=0", session: session() }) }),
    });
    const response = await routeManagedRealtimeTransport(request, env, url, 1000);
    return { response, relay, generic, ownership, owner, resolveCredentialSubject, createCall };
  }
  it("transports SDP with headers through private RPC and still sanitizes the reply", async () => {
    const f = await fixture(true, true, true, false, true, true);
    expect(f.response?.status).toBe(201);
    expect(await f.response?.text()).toBe("v=0");
    expect(f.response?.headers.get("x-nanocodex-realtime-location")).toBe("/v1/realtime/calls/rtc_fixture");
    expect(f.response?.headers.has("set-cookie")).toBe(false);
    expect(f.response?.headers.has("location")).toBe(false);
    expect(f.createCall).toHaveBeenCalledTimes(1);
    expect(f.createCall.mock.calls[0]?.[1]["x-nanocodex-realtime-owner"]).toBe(f.owner);
    expect(f.relay).not.toHaveBeenCalled();
  });
  it.each([true, false])("uses live ownership RPC and never retries a denial through HTTP (owned=%s)", async (owned) => {
    const f = await fixture(owned, true, true, false, true);
    expect(f.response?.status).toBe(owned ? 201 : 404);
    expect(f.resolveCredentialSubject).toHaveBeenCalledTimes(1);
    expect(f.ownership).not.toHaveBeenCalled();
    expect(f.relay).toHaveBeenCalledTimes(owned ? 1 : 0);
    if (owned) expect(f.response?.headers.get("server-timing")).toContain("voice_auth;dur=");
  });
  it.each([true, false])("checks Session ownership without rebinding or resolving a directory (direct=%s)", async (direct) => {
    const f = await fixture(true, true, direct);
    expect(f.response?.status).toBe(201);
    expect(f.ownership).toHaveBeenCalledTimes(1);
    expect(f.generic).not.toHaveBeenCalled();
    const request = f.relay.mock.calls[0]![0];
    expect(request.headers.get("x-nanocodex-subject")).toBe(direct ? `managed-session-v1_${"a".repeat(64)}` : "a".repeat(64));
    expect(request.headers.get("x-nanocodex-realtime-owner")).toBe(f.owner);
    expect(request.headers.has("x-nanocodex-voice-region")).toBe(false);
  });
  it.each([false, true])("carries a retained account pin through voice admission (rpc=%s)", async (rpc) => {
    const f = await fixture(true, true, true, false, rpc, false, "account-a");
    expect(f.response?.status).toBe(201);
    expect(f.relay.mock.calls[0]![0].headers.get("x-nanocodex-chatgpt-account-id")).toBe("account-a");
  });
  it.each([true, false])("does not reach either egress capability when ownership is denied (direct=%s)", async (direct) => {
    const f = await fixture(false, true, direct);
    expect(f.response?.status).toBe(404);
    expect(f.relay).not.toHaveBeenCalled();
    expect(f.generic).not.toHaveBeenCalled();
  });
  it("retains the generic broker's ownership check while the binding is absent", async () => {
    const f = await fixture(true, false);
    expect(f.response?.status).toBe(201);
    expect((f.generic.mock.calls[0]![0] as Request).headers.has("x-nanocodex-realtime-owner")).toBe(false);
  });
  it.each([true, false])("repairs legacy directory state when private call egress cannot be used (sideband=%s)", async (sideband) => {
    const f = await fixture(true, sideband, false, sideband);
    expect(f.response?.status).toBe(201);
    expect(f.relay).not.toHaveBeenCalled();
    expect(f.generic).toHaveBeenCalledTimes(2);
    expect(f.generic.mock.calls[0]![0]).toBe(`https://broker.internal/subjects/${"a".repeat(64)}`);
    expect(f.generic.mock.calls[0]![1]).toMatchObject({ method: "PUT", body: JSON.stringify({ user_id: f.owner }) });
    expect((f.generic.mock.calls[1]![0] as Request).headers.has("x-nanocodex-realtime-owner")).toBe(false);
  });
});
