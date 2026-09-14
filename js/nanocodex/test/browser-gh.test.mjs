import assert from "node:assert/strict";
import test from "node:test";

import {
  createGhCompatibilityCommand,
} from "../tools/browser/browserShell.mjs";
import {
  createBrowserEgressFetch,
  createBrowserRuntimeFetch,
} from "../tools/browser/browserEgress.mjs";

const THREAD_ID = "11111111-1111-4111-8111-111111111111";

test("browser egress sends one credential-free thread-scoped envelope", async () => {
  const requests = [];
  const fetch = createBrowserEgressFetch({
    origin: "https://nanocodex.example",
    threadId: THREAD_ID,
    async fetch(input, init) {
      requests.push(new Request(input, init));
      return new Response("drive", { status: 200 });
    },
  });

  const result = await fetch("https://www.googleapis.com/drive/v3/files?pageSize=1", {
    headers: { accept: "application/json" },
  });
  assert.equal(new TextDecoder().decode(result.body), "drive");
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, "https://nanocodex.example/v1/egress");
  assert.equal(requests[0].headers.get("authorization"), null);
  assert.deepEqual(await requests[0].json(), {
    thread_id: THREAD_ID,
    url: "https://www.googleapis.com/drive/v3/files?pageSize=1",
    method: "GET",
    headers: { accept: "application/json" },
  });
});

test("Connect authorization stays on the egress gateway and never reaches its target", async () => {
  const requests = [];
  const fetch = createBrowserRuntimeFetch({
    origin: "https://connect.example",
    threadId: THREAD_ID,
    headers: { authorization: "Bearer grant-session" },
    async fetch(input, init) {
      const request = new Request(input, init);
      requests.push(request);
      if (request.url === "https://connect.example/v1/agent/account-info") {
        return Response.json({ status: "ready" });
      }
      return new Response("github", { status: 200 });
    },
  });

  await fetch("/v1/agent/account-info");
  await fetch("https://api.github.com/user", {
    headers: { accept: "application/vnd.github+json" },
  });

  assert.equal(requests[0].url, "https://connect.example/v1/agent/account-info");
  assert.equal(requests[0].headers.get("authorization"), null);
  assert.equal(requests[1].url, "https://connect.example/v1/egress");
  assert.equal(requests[1].headers.get("authorization"), "Bearer grant-session");
  const envelope = await requests[1].json();
  assert.deepEqual(envelope.headers, { accept: "application/vnd.github+json" });
  assert.equal(JSON.stringify(envelope).includes("grant-session"), false);
});

for (const key of ["PASSWORD", "API_KEY"]) {
test(`browser egress forwards only an opaque Vault reference and closed placeholders (${key})`, async () => {
  const requests = [];
  const fetch = createBrowserEgressFetch({
    origin: "https://nanocodex.example",
    threadId: THREAD_ID,
    async fetch(input, init) {
      requests.push(new Request(input, init));
      return Response.json({ status: 204, ok: true });
    },
  });
  const vaultId = "abcdefghijklmnopqrstuvwxyzABCDEF";
  await fetch("https://example.com/session", {
    method: "POST",
    headers: {
      authorization: `Bearer {{NANOCODEX_VAULT_${key}}}`,
      "content-type": "application/json",
      "x-nanocodex-vault-id": vaultId,
    },
    body: JSON.stringify({ password: `{{NANOCODEX_VAULT_${key}}}` }),
  });

  assert.deepEqual(await requests[0].json(), {
    thread_id: THREAD_ID,
    url: "https://example.com/session",
    method: "POST",
    headers: {
      authorization: `Bearer {{NANOCODEX_VAULT_${key}}}`,
      "content-type": "application/json",
      "x-nanocodex-vault-id": vaultId,
    },
    body: JSON.stringify({ password: `{{NANOCODEX_VAULT_${key}}}` }),
  });
});
}

test("browser egress rejects raw credentials and malformed Vault requests", async () => {
  const fetch = createBrowserEgressFetch({
    origin: "https://nanocodex.example",
    threadId: THREAD_ID,
    async fetch() { throw new Error("must not reach gateway"); },
  });

  await assert.rejects(
    fetch("https://example.com", { headers: { authorization: "Bearer raw-secret" } }),
    /does not accept credential/,
  );
  await assert.rejects(
    fetch("https://example.com", {
      headers: {
        cookie: "{{NANOCODEX_VAULT_PASSWORD}}",
        "x-nanocodex-vault-id": "abcdefghijklmnopqrstuvwxyzABCDEF",
      },
    }),
    /does not accept credential/,
  );
  await assert.rejects(
    fetch("https://example.com", {
      headers: {
        authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
        "x-nanocodex-vault-id": "invalid",
      },
    }),
    /valid Vault item id/,
  );
  await assert.rejects(
    fetch("https://example.com", {
      headers: { "x-nanocodex-vault-id": "abcdefghijklmnopqrstuvwxyzABCDEF" },
    }),
    /require a supported placeholder/,
  );
});

test("browser gh makes useful GitHub calls through the same-origin connector", async () => {
  const requests = [];
  const command = createGhCompatibilityCommand({}, {}, (_name, handler) => handler, {
    async fetch(url, init) {
      requests.push({
        authorization: new Headers(init.headers).get("authorization"),
        body: init.body,
        method: init.method,
        url,
      });
      if (url === "https://api.github.com/user") return secureJson(url, { login: "nano-cat" });
      if (url === "https://api.github.com/repos/gakonst/nanocodex") return secureJson(url, {
        default_branch: "master",
        description: "Tiny agents",
        full_name: "gakonst/nanocodex",
        html_url: "https://github.com/gakonst/nanocodex",
        private: false,
      });
      if (url.includes("/pulls?")) return secureJson(url, [{
        number: 42,
        title: "Keep credentials private",
        head: { ref: "connector" },
      }]);
      return secureJson(url, { full_name: "gakonst/nanocodex" });
    },
  });

  const auth = await command(["auth", "status"]);
  assert.equal(auth.exitCode, 0);
  assert.match(auth.stdout, /nano-cat/);
  const api = await command(["api", "repos/gakonst/nanocodex"]);
  assert.match(api.stdout, /gakonst\/nanocodex/);
  const repo = await command(["repo", "view", "gakonst/nanocodex"]);
  assert.match(repo.stdout, /default branch:\tmaster/);
  const pulls = await command(["pr", "list", "--repo", "gakonst/nanocodex", "--limit", "10"]);
  assert.equal(pulls.stdout, "42\tKeep credentials private\tconnector\n");

  assert.equal(requests.length, 4);
  assert(requests.every(({ url }) => url.startsWith("https://api.github.com/")));
  assert(requests.every(({ method }) => method === "GET"));
  assert(requests.every(({ authorization }) => authorization === null));
  assert.match(requests[3].url, /per_page=10/);
  const write = await command([
    "api", "--method", "POST", "/repos/gakonst/nanocodex/issues", "-f", "title=hello",
  ]);
  assert.equal(write.exitCode, 0);
  assert.equal(requests[4].method, "POST");
  assert.equal(requests[4].body, JSON.stringify({ title: "hello" }));
  assert.equal(requests[4].authorization, null);

  const inferredWrite = await command([
    "api", "/repos/gakonst/nanocodex/issues", "-f", "title=inferred",
  ]);
  assert.equal(inferredWrite.exitCode, 0);
  assert.equal(requests[5].method, "POST");
  assert.equal(requests[5].body, JSON.stringify({ title: "inferred" }));

  const explicitGet = await command([
    "api", "--method", "GET", "/search/issues?sort=updated", "-f", "q=browser shim",
  ]);
  assert.equal(explicitGet.exitCode, 0);
  assert.equal(requests[6].method, "GET");
  assert.equal(requests[6].body, undefined);
  const getUrl = new URL(requests[6].url);
  assert.equal(getUrl.searchParams.get("sort"), "updated");
  assert.equal(getUrl.searchParams.get("q"), "browser shim");
});

test("browser gh rejects unsupported operations instead of presenting successful help", async () => {
  const command = createGhCompatibilityCommand({}, {}, (_name, handler) => handler);

  const unsupported = await command(["issue", "list"]);
  assert.equal(unsupported.exitCode, 1);
  assert.match(unsupported.stderr, /unsupported browser operation 'issue list'/);
  assert.match(unsupported.stderr, /Supported commands:/);

  const help = await command(["--help"]);
  assert.equal(help.exitCode, 0);
  assert.match(help.stdout, /Supported commands:/);
});

function secureJson(url, value) {
  return {
    status: 200,
    statusText: "OK",
    headers: { "content-type": "application/json" },
    body: new TextEncoder().encode(JSON.stringify(value)),
    url,
  };
}


test("browser runtime preserves binary Git uploads and returns the broker response stream", async () => {
  const bytes = new Uint8Array([0, 255, 128, 254, 10]);
  let envelope;
  let cancelled = false;
  const upstream = new Response(new ReadableStream({
    start(controller) { controller.enqueue(bytes); },
    cancel() { cancelled = true; },
  }));
  const fetch = createBrowserRuntimeFetch({
    origin: "https://connect.example",
    threadId: THREAD_ID,
    async fetch(_input, init) {
      envelope = JSON.parse(init.body);
      return upstream;
    },
  });
  const response = await fetch("https://github.com/fixture/large.git/git-upload-pack", {
    method: "POST", body: bytes,
    headers: { "content-type": "application/x-git-upload-pack-request" },
  });
  assert.equal(response, upstream);
  assert.deepEqual(new Uint8Array(Buffer.from(envelope.body_base64, "base64")), bytes);
  assert.equal(envelope.body, undefined);
  const reader = response.body.getReader();
  assert.deepEqual((await reader.read()).value, bytes);
  await reader.cancel();
  assert.equal(cancelled, true);
});
