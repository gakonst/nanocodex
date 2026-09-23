import { createExecutionContext } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import { handleEgress, handleManagedRealtimeSideband, ManagedRealtimeEgress, type EgressEnv } from "../src/egress";

const owner = "11111111-1111-4111-8111-111111111111";
const directSubject = `managed-session-v1_${"a".repeat(64)}`;
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

function fixture(subject = directSubject) {
  const directory = vi.fn(async () => new Response(null, { status: 404 }));
  const resolveModelCredential = vi.fn(async () => ({ status: 200, credential: {
    kind: "chatgpt", revision: 1, secret: "synthetic-token", accountId: "synthetic-account",
  } }));
  const credentials = vi.fn(() => ({ resolveModelCredential }));
  const relay = vi.fn(() => { throw new Error("sideband must not enter a container"); });
  const env = {
    AGENT_SUBJECTS: { getByName: () => ({ fetch: directory }) },
    MANAGED_AGENT_OWNERSHIP: { fetch: directory },
    USER_CREDENTIALS: { getByName: credentials },
    CHATGPT_EGRESS: { idFromName: relay, get: relay },
    CHATGPT_EGRESS_WNAM: { idFromName: relay, get: relay },
  } as unknown as EgressEnv;
  const request = new Request("https://nanocodex.internal/v1/realtime/sideband", {
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      upgrade: "websocket", "x-nanocodex-subject": subject,
      "x-nanocodex-realtime-owner": owner,
      "x-nanocodex-realtime-call-id": "rtc_fixture",
      "x-nanocodex-chatgpt-account-id": "synthetic-account",
      "x-nanocodex-voice-region": "wnam",
      "openai-alpha": "quicksilver=v2", "session-id": "voice-fixture",
      "thread-id": "voice-fixture", "x-session-id": "voice-fixture",
    },
  });
  return { env, request, directory, credentials, resolveModelCredential, relay };
}

describe("private managed realtime sideband capability", () => {
  it.each([directSubject, "a".repeat(64)])("uses verified %s ownership while preserving direct provider upgrade and account selection", async subject => {
    const f = fixture(subject);
    const [client, server] = Object.values(new WebSocketPair());
    server.accept();
    const upgrade = new Response(null, { status: 101, webSocket: client });
    const upstream = vi.fn(async (_request: Request) => upgrade);
    vi.stubGlobal("fetch", upstream);
    const entrypoint = new ManagedRealtimeEgress(createExecutionContext(), f.env);
    expect(await entrypoint.fetch(f.request)).toBe(upgrade);
    expect(f.directory).not.toHaveBeenCalled();
    expect(f.credentials).toHaveBeenCalledWith(owner, undefined);
    expect(f.resolveModelCredential).toHaveBeenCalledWith(false, undefined, "synthetic-account");
    expect(f.relay).not.toHaveBeenCalled();
    const sent = upstream.mock.calls[0]![0];
    expect(sent.url).toBe("https://api.openai.com/v1/live/rtc_fixture");
    expect(sent.headers.get("authorization")).toBe("Bearer synthetic-token");
    expect(sent.headers.get("chatgpt-account-id")).toBe("synthetic-account");
    for (const header of ["session-id", "thread-id", "x-session-id"]) expect(sent.headers.get(header)).toBe("voice-fixture");
    for (const header of ["x-nanocodex-realtime-owner", "x-nanocodex-subject", "x-nanocodex-chatgpt-account-id", "x-nanocodex-voice-region", "x-nanocodex-realtime-call-id"]) {
      expect(sent.headers.has(header)).toBe(false);
    }
    client.accept(); client.close(); server.close();
  });

  it("keeps the call and account pin through 401 refresh without repeating Session ownership", async () => {
    const f = fixture();
    const [client, server] = Object.values(new WebSocketPair());
    server.accept();
    const upgrade = new Response(null, { status: 101, webSocket: client });
    const upstream = vi.fn(async (_request: Request) => upgrade).mockResolvedValueOnce(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", upstream);
    expect(await handleManagedRealtimeSideband(f.request, f.env)).toBe(upgrade);
    expect(upstream).toHaveBeenCalledTimes(2);
    expect(f.resolveModelCredential.mock.calls).toEqual([[false, undefined, "synthetic-account"], [true, 1, "synthetic-account"]]);
    for (const [sent] of upstream.mock.calls) {
      expect(sent.url).toBe("https://api.openai.com/v1/live/rtc_fixture");
      expect(sent.headers.get("x-session-id")).toBe("voice-fixture");
    }
    expect(f.directory).not.toHaveBeenCalled();
    expect(f.relay).not.toHaveBeenCalled();
    client.accept(); client.close(); server.close();
  });

  it.each([directSubject, "a".repeat(64)])("generic sideband never accepts the owner assertion for %s", async subject => {
    const f = fixture(subject);
    const upstream = vi.fn(async () => new Response(null, { status: 503 }));
    vi.stubGlobal("fetch", upstream);
    expect((await handleEgress(f.request, f.env)).status).toBe(403);
    expect(f.directory).toHaveBeenCalledTimes(1);
    expect(f.credentials).not.toHaveBeenCalled();
    expect(upstream).not.toHaveBeenCalled();
  });

  it.each(["owner", "subject", "method", "origin", "path", "query", "upgrade", "call", "placeholder", "provider-header"])("rejects invalid %s before credential or provider access", async invalid => {
    const f = fixture();
    const headers = new Headers(f.request.headers);
    let url = f.request.url;
    let method = "GET";
    if (invalid === "owner") headers.set("x-nanocodex-realtime-owner", "attacker");
    if (invalid === "subject") headers.set("x-nanocodex-subject", "attacker");
    if (invalid === "method") method = "POST";
    if (invalid === "origin") url = "https://attacker.example/v1/realtime/sideband";
    if (invalid === "path") url = "https://nanocodex.internal/v1/responses";
    if (invalid === "query") url += "?call_id=rtc_other";
    if (invalid === "upgrade") headers.delete("upgrade");
    if (invalid === "call") headers.set("x-nanocodex-realtime-call-id", "../other");
    if (invalid === "placeholder") headers.set("authorization", "Bearer attacker");
    if (invalid === "provider-header") headers.set("chatgpt-account-id", "attacker");
    const upstream = vi.fn(async () => new Response(null, { status: 503 }));
    vi.stubGlobal("fetch", upstream);
    expect((await handleManagedRealtimeSideband(new Request(url, { method, headers }), f.env)).status).toBe(403);
    expect(f.credentials).not.toHaveBeenCalled();
    expect(f.directory).not.toHaveBeenCalled();
    expect(upstream).not.toHaveBeenCalled();
  });

  it("does not retry an uncertain provider upgrade or fall back to a relay", async () => {
    const f = fixture();
    const upstream = vi.fn(async () => { throw new Error("provider upgrade interrupted"); });
    vi.stubGlobal("fetch", upstream);
    expect((await handleManagedRealtimeSideband(f.request, f.env)).status).toBe(502);
    expect(upstream).toHaveBeenCalledTimes(1);
    expect(f.directory).not.toHaveBeenCalled();
    expect(f.relay).not.toHaveBeenCalled();
  });
});
