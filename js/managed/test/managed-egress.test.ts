import { afterEach, describe, expect, it, vi } from "vitest";

import {
  exactConnectorAccess,
  handleManagedEgress,
} from "../src/managed-egress";

const SUBJECT = "s".repeat(43);
const VAULT_ID = "v".repeat(22);

afterEach(() => vi.unstubAllGlobals());

describe("Computer egress gateway", () => {
  it.each(["PASSWORD", "API_KEY"])("routes only vault references and placeholders through the private binding (%s)", async (key) => {
    const seen: Request[] = [];
    const binding = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        seen.push(new Request(input, init));
        return Response.json({ ok: true });
      },
    } as Fetcher;
    const publicFetch = vi.fn();
    vi.stubGlobal("fetch", publicFetch);
    const gateway = testGateway(binding);
    const body = JSON.stringify({
      username: "{{NANOCODEX_VAULT_USERNAME}}",
      password: `{{NANOCODEX_VAULT_${key}}}`,
    });

    const response = await gateway.fetch("https://accounts.example/session", {
      method: "POST",
      headers: {
        authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
        "content-type": "application/json",
        "x-api-key": `{{NANOCODEX_VAULT_${key}}}`,
        "x-nanocodex-vault-id": VAULT_ID,
      },
      body,
    });

    expect(response.status).toBe(200);
    expect(publicFetch).not.toHaveBeenCalled();
    expect(seen).toHaveLength(1);
    expect(seen[0]!.url).toBe("https://vault-egress.internal/v1/request");
    expect(seen[0]!.method).toBe("POST");
    expect(seen[0]!.headers.get("content-type")).toBe("application/json");
    expect(seen[0]!.headers.get("x-nanocodex-subject")).toBe(SUBJECT);
    expect(seen[0]!.headers.get("authorization")).toBeNull();
    expect(await seen[0]!.json()).toEqual({
      vault_id: VAULT_ID,
      url: "https://accounts.example/session",
      method: "POST",
      headers: {
        authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
        "content-type": "application/json",
        "x-api-key": `{{NANOCODEX_VAULT_${key}}}`,
      },
      body,
    });
  });

  it("denies raw secrets and provider destinations in vault mode", async () => {
    const binding = { fetch: vi.fn() } as unknown as Fetcher;
    const publicFetch = vi.fn();
    vi.stubGlobal("fetch", publicFetch);
    const gateway = testGateway(binding);
    const cases: Array<[string, RequestInit, string]> = [
      ["https://example.com/", {
        headers: { authorization: "Bearer raw-secret", "x-nanocodex-vault-id": VAULT_ID },
      }, "credential_header_denied"],
      ["https://example.com/", {
        headers: { cookie: "session=raw-secret", "x-nanocodex-vault-id": VAULT_ID },
      }, "credential_header_denied"],
      ["https://example.com/", {
        headers: { "x-api-key": "raw-secret", "x-nanocodex-vault-id": VAULT_ID },
      }, "credential_header_denied"],
      ["https://api.github.com/user", {
        headers: { "x-nanocodex-vault-id": VAULT_ID },
      }, "destination_denied"],
    ];
    for (const [url, init, error] of cases) {
      const response = await gateway.fetch(url, init);
      expect(response.status, url).toBe(403);
      expect(await response.json(), url).toEqual({ error });
    }
    expect(binding.fetch).not.toHaveBeenCalled();
    expect(publicFetch).not.toHaveBeenCalled();
  });

  it("requires valid vault and subject references before private dispatch", async () => {
    const binding = { fetch: vi.fn() } as unknown as Fetcher;
    vi.stubGlobal("fetch", vi.fn());
    const invalidId = await handleManagedEgress(new Request("https://example.com/", {
      headers: { "x-nanocodex-vault-id": "invalid" },
    }), binding, SUBJECT);
    expect(invalidId.status).toBe(403);
    expect(await invalidId.json()).toEqual({ error: "vault_reference_denied" });

    const missingSubject = await handleManagedEgress(new Request("https://example.com/", {
      headers: { "x-nanocodex-vault-id": VAULT_ID },
    }), binding);
    expect(missingSubject.status).toBe(403);
    expect(await missingSubject.json()).toEqual({ error: "requires_login" });
    expect(binding.fetch).not.toHaveBeenCalled();
  });

  it("bounds vault request bodies before binding dispatch", async () => {
    const binding = { fetch: vi.fn() } as unknown as Fetcher;
    vi.stubGlobal("fetch", vi.fn());
    const response = await testGateway(binding).fetch("https://example.com/", {
      method: "POST",
      headers: { "x-nanocodex-vault-id": VAULT_ID },
      body: "x".repeat(64 * 1024 + 1),
    });
    expect(response.status).toBe(413);
    expect(await response.json()).toEqual({ error: "request_too_large" });
    expect(binding.fetch).not.toHaveBeenCalled();
  });

  it("sends provider reads and unbounded writes through the private connector binding", async () => {
    const seen: Request[] = [];
    const binding = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        seen.push(new Request(input, init));
        return Response.json({ ok: true });
      },
    } as Fetcher;
    const publicFetch = vi.fn(async () => new Response("public"));
    vi.stubGlobal("fetch", publicFetch);
    const gateway = testGateway(binding);

    for (const url of [
      "https://api.github.com/repos/nanocodex/nanocodex/pulls?state=open",
      "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10",
      "https://www.googleapis.com/drive/v3/files?pageSize=10",
      "https://api.x.com/2/dm_events?max_results=10",
    ]) {
      expect((await gateway.fetch(url)).status).toBe(200);
    }
    const writeBody = "x".repeat(2 * 1024 * 1024);
    expect((await gateway.fetch("https://www.googleapis.com/upload/drive/v3/files?uploadType=media", {
      method: "POST",
      headers: { "content-type": "application/octet-stream" },
      body: writeBody,
    })).status).toBe(200);
    const xWriteBody = JSON.stringify({ text: "hello from Nanocodex" });
    expect((await gateway.fetch("https://api.x.com/2/tweets", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: xWriteBody,
    })).status).toBe(200);
    expect((await gateway.fetch(
      "https://api.x.com/2/users/2244994945/bookmarks/1890000000000000000",
      { method: "DELETE" },
    )).status).toBe(200);

    expect(seen.map((request) => request.url)).toEqual([
      "https://api.github.com/repos/nanocodex/nanocodex/pulls?state=open",
      "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10",
      "https://www.googleapis.com/drive/v3/files?pageSize=10",
      "https://api.x.com/2/dm_events?max_results=10",
      "https://www.googleapis.com/upload/drive/v3/files?uploadType=media",
      "https://api.x.com/2/tweets",
      "https://api.x.com/2/users/2244994945/bookmarks/1890000000000000000",
    ]);
    expect(seen.every((request) => (
      request.headers.get("authorization") === "Bearer NANOCODEX_PROVIDER_CREDENTIAL"
        && request.headers.get("x-nanocodex-subject") === SUBJECT
    ))).toBe(true);
    expect(publicFetch).not.toHaveBeenCalled();
    expect(await seen[4]!.text()).toBe(writeBody);
    expect(await seen[5]!.text()).toBe(xWriteBody);
    expect(seen[5]!.headers.get("content-type")).toBe("application/json");
  });

  it("routes every service capability with one validated connection selector", async () => {
    const seen: Request[] = [];
    const allowed = vi.fn(() => true);
    const binding = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        seen.push(new Request(input, init));
        return Response.json({ ok: true });
      },
    } as Fetcher;
    const connectionId = "c".repeat(43);
    const routes = [
      ["github", "https://api.github.com/user"],
      ["gmail", "https://gmail.googleapis.com/gmail/v1/users/me/messages"],
      ["gdrive", "https://www.googleapis.com/drive/v3/files"],
      ["gcalendar", "https://calendar.googleapis.com/calendar/v3/calendars/primary/events"],
      ["gtasks", "https://tasks.googleapis.com/tasks/v1/users/@me/lists"],
      ["gdocs", "https://docs.googleapis.com/v1/documents/document-id"],
      ["gsheets", "https://sheets.googleapis.com/v4/spreadsheets/sheet-id"],
      ["gslides", "https://slides.googleapis.com/v1/presentations/deck-id"],
      ["gcontacts", "https://people.googleapis.com/v1/people/me/connections"],
      ["slack", "https://slack.com/api/conversations.list"],
      ["x", "https://api.x.com/2/users/me"],
    ] as const;

    for (const [capability, url] of routes) {
      const response = await handleManagedEgress(new Request(url, {
        headers: { "X-Nanocodex-Connector-Connection": connectionId },
      }), binding, SUBJECT, allowed);
      expect(response.status, capability).toBe(200);
    }
    expect(allowed.mock.calls).toEqual(routes.map(([capability]) => [capability, connectionId]));
    expect(seen.every((request) => (
      request.headers.get("x-nanocodex-connector-connection") === connectionId
    ))).toBe(true);

    expect((await handleManagedEgress(new Request(routes[0][1], {
      headers: { "X-Nanocodex-Connector-Connection": "not-an-id" },
    }), binding, SUBJECT, allowed)).status).toBe(403);
    expect((await handleManagedEgress(new Request("https://public.example/", {
      headers: { "X-Nanocodex-Connector-Connection": connectionId },
    }), binding, SUBJECT, allowed)).status).toBe(403);
  });

  it("injects a sole approved connection and rejects identities outside the grant", async () => {
    const approved = "a".repeat(43);
    const addedLater = "b".repeat(43);
    const seen: Request[] = [];
    const binding = {
      async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        seen.push(new Request(input, init));
        return Response.json({ ok: true });
      },
    } as Fetcher;
    const authorize = (_connector: string, selected?: string): boolean | string => (
      exactConnectorAccess([approved], selected)
    );

    expect((await handleManagedEgress(
      new Request("https://api.github.com/user"),
      binding,
      SUBJECT,
      authorize,
    )).status).toBe(200);
    expect(seen[0]!.headers.get("x-nanocodex-connector-connection")).toBe(approved);

    expect((await handleManagedEgress(new Request("https://api.github.com/user", {
      headers: { "X-Nanocodex-Connector-Connection": addedLater },
    }), binding, SUBJECT, authorize)).status).toBe(403);
    expect(seen).toHaveLength(1);
    expect(exactConnectorAccess([approved, addedLater])).toBe(false);
    expect(exactConnectorAccess([approved, addedLater], approved)).toBe(approved);
  });

  it("rejects credentials, lookalikes, userinfo, and private destinations", async () => {
    const binding = { fetch: vi.fn() } as unknown as Fetcher;
    const publicFetch = vi.fn();
    vi.stubGlobal("fetch", publicFetch);
    const gateway = testGateway(binding);
    const cases: Array<[RequestInfo, RequestInit | undefined, number]> = [
      ["https://example.com/", { headers: { authorization: "Bearer browser-token" } }, 403],
      ["https://example.com/", { headers: { cookie: "session=secret" } }, 403],
      ["https://example.com/", { headers: { cookie2: "session=secret" } }, 403],
      ["https://example.com/", { headers: { "x-authorization2": "browser-token" } }, 403],
      ["http://api.github.com/user", undefined, 403],
      ["https://api.github.com.evil.test/user", undefined, 403],
      ["https://gmail.googleapis.com/gmail/v1/users/me/%2e%2e%2fother/messages", undefined, 403],
      ["https://www.googleapis.com/drive/v3/%252e%252e%252fother", undefined, 403],
      ["https://name:password@example.com/", undefined, 403],
      ["http://127.0.0.1/", undefined, 403],
      ["http://169.254.169.254/latest/meta-data", undefined, 403],
      ["http://[::1]/", undefined, 403],
    ];
    for (const [url, init, status] of cases) {
      expect((await gateway.fetch(url, init)).status, String(url)).toBe(status);
    }
    expect(binding.fetch).not.toHaveBeenCalled();
    expect(publicFetch).not.toHaveBeenCalled();
  });

  it("routes public requests through the account broker without direct network access", async () => {
    const direct = vi.fn();
    vi.stubGlobal("fetch", direct);
    const requests: Request[] = [];
    const binding = { async fetch(request: Request) {
      requests.push(request);
      return new Response("done", { headers: { "set-cookie": "must-not-return" } });
    } } as unknown as Fetcher;
    const response = await testGateway(binding).fetch("https://one.example/start", { method: "POST", body: "data" });
    expect(await response.text()).toBe("done");
    expect(response.headers.get("set-cookie")).toBeNull();
    expect(direct).not.toHaveBeenCalled();
    expect(requests).toHaveLength(1);
    expect(requests[0]!.url).toBe("https://public-egress.internal/v1/request");
    expect(requests[0]!.headers.get("x-nanocodex-target-url")).toBe("https://one.example/start");
    expect(requests[0]!.headers.get("x-nanocodex-subject")).toBe(SUBJECT);
    expect(await requests[0]!.text()).toBe("data");
  });

  it("uses the same exact GitHub connection for API and smart HTTP", async () => {
    const selected = "c".repeat(43);
    const seen: Request[] = [];
    const binding = { async fetch(request: Request) { seen.push(request); return new Response("ok"); } } as unknown as Fetcher;
    for (const url of [
      "https://api.github.com/user",
      "https://github.com/owner/private.git/info/refs?service=git-upload-pack",
      "https://github.com/owner/private.git/git-upload-pack",
      "https://github.com/owner/private.git/git-receive-pack",
    ]) {
      const request = new Request(url, { method: url.endsWith("pack") && !url.includes("?") ? "POST" : "GET" });
      const response = await handleManagedEgress(request, binding, SUBJECT,
        (connector, id) => connector === "github" ? exactConnectorAccess([selected], id) : false);
      expect(response.status).toBe(200);
      expect(seen.at(-1)!.headers.get("x-nanocodex-connector-connection")).toBe(selected);
      expect(seen.at(-1)!.headers.get("x-nanocodex-subject")).toBe(SUBJECT);
      expect(seen.at(-1)!.headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
      const denied = await handleManagedEgress(new Request(url, {
        headers: { "x-nanocodex-connector-connection": "d".repeat(43) },
      }), binding, SUBJECT, (_connector, id) => exactConnectorAccess([selected], id));
      expect(denied.status).toBe(403);
    }
    expect(seen).toHaveLength(4);
  });

  it("streams public responses without an application byte ceiling", async () => {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const upstream = new ReadableStream<Uint8Array>({
      start(value) {
        controller = value;
        value.enqueue(new TextEncoder().encode("first"));
      },
    });
    const direct = vi.fn();
    vi.stubGlobal("fetch", direct);
    const gateway = testGateway({ fetch: vi.fn(async () => new Response(upstream)) } as unknown as Fetcher);

    const response = await gateway.fetch("https://assets.example/compiler");
    const reader = response.body!.getReader();
    expect(new TextDecoder().decode((await reader.read()).value)).toBe("first");
    controller.enqueue(new TextEncoder().encode("second"));
    controller.close();
    expect(new TextDecoder().decode((await reader.read()).value)).toBe("second");
    expect((await reader.read()).done).toBe(true);
    expect(direct).not.toHaveBeenCalled();
  });
});

function testGateway(binding: Fetcher): Fetcher {
  return {
    fetch(input: RequestInfo | URL, init?: RequestInit) {
      return handleManagedEgress(new Request(input, init), binding, SUBJECT);
    },
  } as Fetcher;
}
