import assert from "node:assert/strict";
import { test } from "node:test";

import worker from "./index.ts";

function createByokSessions() {
  const credentials = new Map<string, string>();
  const namespace = {
    idFromName(name: string) {
      return { name };
    },
    get(id: { name: string }) {
      return {
        async fetch(input: string | URL | Request, init?: RequestInit) {
          const request = new Request(input, init);
          if (request.method === "PUT") {
            credentials.set(id.name, await request.text());
            return new Response(null, { status: 204 });
          }
          if (request.method === "DELETE") {
            credentials.delete(id.name);
            return new Response(null, { status: 204 });
          }
          const credential = credentials.get(id.name);
          return credential === undefined
            ? new Response(null, { status: 404 })
            : new Response(credential);
        },
      };
    },
  };
  return { credentials, namespace: namespace as unknown as DurableObjectNamespace };
}

test("tool proxies keep credentials server-side and preserve native request shapes", async () => {
  const originalFetch = globalThis.fetch;
  const upstream: Array<{ url: string; init?: RequestInit }> = [];
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    upstream.push({ url, init });
    if (url.endsWith("/alpha/search")) {
      return Response.json({ output: "Search result with turn0search0", results: [] });
    }
    if (url.endsWith("/images/generations")) {
      return Response.json({ created: 1, data: [{ b64_json: "aGVsbG8=" }] });
    }
    throw new Error(`unexpected upstream URL ${url}`);
  }) as typeof fetch;

  try {
    const env = { ENVIRONMENT: "test", OPENAI_API_KEY: "server-secret" };
    const search = await worker.fetch(new Request("https://demo.test/api/tools/web-search", {
      method: "POST",
      headers: { "content-type": "application/json", origin: "https://demo.test" },
      body: JSON.stringify({
        session_id: "session-1",
        commands: { search_query: [{ q: "nanocodex" }] },
      }),
    }), env);
    assert.equal(search.status, 200);
    assert.deepEqual(await search.json(), { output: "Search result with turn0search0" });

    const image = await worker.fetch(new Request("https://demo.test/api/tools/image-generation", {
      method: "POST",
      headers: { "content-type": "application/json", origin: "https://demo.test" },
      body: JSON.stringify({ prompt: "a tiny robot", images: [] }),
    }), env);
    assert.equal(image.status, 200);
    assert.deepEqual(await image.json(), { image_url: "data:image/png;base64,aGVsbG8=" });

    assert.equal(upstream.length, 2);
    assert.equal(new Headers(upstream[0]?.init?.headers).get("authorization"), "Bearer server-secret");
    assert.deepEqual(JSON.parse(String(upstream[0]?.init?.body)), {
      id: "session-1",
      model: "gpt-5.6-sol",
      commands: { search_query: [{ q: "nanocodex" }] },
      settings: { allowed_callers: ["direct"], external_web_access: true },
      max_output_tokens: 10_000,
    });
    assert.deepEqual(JSON.parse(String(upstream[1]?.init?.body)), {
      prompt: "a tiny robot",
      background: "auto",
      model: "gpt-image-2",
      quality: "auto",
      size: "auto",
    });
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("tool proxies reject cross-origin calls before using the credential", async () => {
  const response = await worker.fetch(new Request("https://demo.test/api/tools/web-search", {
    method: "POST",
    headers: { "content-type": "application/json", origin: "https://evil.test" },
    body: "{}",
  }), { ENVIRONMENT: "test", OPENAI_API_KEY: "server-secret" });
  assert.equal(response.status, 403);
});

test("BYOK sessions keep the key behind an opaque HttpOnly cookie and take precedence", async () => {
  const { credentials, namespace } = createByokSessions();
  const env = {
    ENVIRONMENT: "test",
    OPENAI_API_KEY: "deployment-secret",
    BYOK_SESSIONS: namespace,
  };
  const created = await worker.fetch(new Request("https://demo.test/api/auth/openai", {
    method: "PUT",
    headers: { "content-type": "application/json", origin: "https://demo.test" },
    body: JSON.stringify({ api_key: "  user-secret  " }),
  }), env);
  assert.equal(created.status, 200);
  const createdBody = await created.text();
  assert.doesNotMatch(createdBody, /user-secret/);
  assert.match(createdBody, /"credential_source":"user"/);
  const setCookie = created.headers.get("set-cookie") ?? "";
  assert.match(setCookie, /^nanocodex_byok=[A-Za-z0-9_-]{43};/);
  assert.match(setCookie, /Path=\/api/);
  assert.match(setCookie, /HttpOnly/);
  assert.match(setCookie, /SameSite=Strict/);
  assert.match(setCookie, /Max-Age=3600/);
  assert.match(setCookie, /Secure/);
  const cookie = setCookie.split(";", 1)[0]!;
  assert.deepEqual([...credentials.values()], ["user-secret"]);

  const health = await worker.fetch(new Request("https://demo.test/api/health", {
    headers: { cookie },
  }), env);
  assert.deepEqual(await health.json(), {
    agent_configured: true,
    credential_source: "user",
    service: "nanocodex",
    runtime: "cloudflare-workers",
    status: "ok",
  });

  const originalFetch = globalThis.fetch;
  let authorization = "";
  globalThis.fetch = (async (_input: string | URL | Request, init?: RequestInit) => {
    authorization = new Headers(init?.headers).get("authorization") ?? "";
    return Response.json({ output: "ok" });
  }) as typeof fetch;
  try {
    const search = await worker.fetch(new Request("https://demo.test/api/tools/web-search", {
      method: "POST",
      headers: { "content-type": "application/json", origin: "https://demo.test", cookie },
      body: JSON.stringify({
        session_id: "session-1",
        commands: { search_query: [{ q: "nanocodex" }] },
      }),
    }), env);
    assert.equal(search.status, 200);
    assert.equal(authorization, "Bearer user-secret");
  } finally {
    globalThis.fetch = originalFetch;
  }

  const cleared = await worker.fetch(new Request("https://demo.test/api/auth/openai", {
    method: "DELETE",
    headers: { origin: "https://demo.test", cookie },
  }), env);
  assert.equal(cleared.status, 200);
  assert.match(cleared.headers.get("set-cookie") ?? "", /Max-Age=0/);
  assert.equal(credentials.size, 0);
  assert.deepEqual(await cleared.json(), {
    agent_configured: true,
    credential_source: "deployment",
  });
});

test("BYOK creation rejects cross-origin requests before storing a key", async () => {
  const { credentials, namespace } = createByokSessions();
  const response = await worker.fetch(new Request("https://demo.test/api/auth/openai", {
    method: "PUT",
    headers: { "content-type": "application/json", origin: "https://evil.test" },
    body: JSON.stringify({ api_key: "must-not-be-stored" }),
  }), { ENVIRONMENT: "test", BYOK_SESSIONS: namespace });
  assert.equal(response.status, 403);
  assert.equal(credentials.size, 0);
});
