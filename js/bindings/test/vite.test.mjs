import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createServer } from "node:http";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import WebSocket, { WebSocketServer } from "ws";

import { readCodexSubscription } from "../vite/codex-auth-file.mjs";
import { createNanocodexCloudflarePlugins } from "../vite/cloudflare-plugin.mjs";
import { startChatGptWorkerEgress } from "../vite/chatgpt-egress.mjs";
import { nanocodex } from "../vite/index.mjs";

test("one Vite plugin gives Cloudflare only exact development Worker bindings", async () => {
  const fixture = await authFixture();
  const sentinelName = "NANOCODEX_TEST_UNRELATED_HOST_SECRET";
  const previousSentinel = process.env[sentinelName];
  process.env[sentinelName] = "must-not-enter-workerd";
  let cloudflareOptions;
  const plugins = createNanocodexCloudflarePlugins({
    chatGpt: { authFile: fixture.path },
    cloudflare: { config: () => ({ vars: { APPLICATION_VAR: "kept" } }) },
  }, (options) => {
    cloudflareOptions = options;
    return [{ name: "vite-plugin-cloudflare" }];
  });
  const plugin = plugins[0];
  try {
    const config = await plugin.config({
      worker: {},
    }, { command: "serve" });
    const customized = cloudflareOptions.config({ vars: { FROM_WRANGLER: "kept-too" } });
    assert.equal(customized.vars.APPLICATION_VAR, "kept");
    assert.equal(customized.vars.ENVIRONMENT, "development");
    assert.equal(customized.vars.NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN, fixture.accessToken);
    assert.equal(customized.vars.NANOCODEX_DEV_CHATGPT_ACCOUNT_ID, "account-123");
    assert.match(customized.vars.NANOCODEX_DEV_CHATGPT_EGRESS_URL,
      /^http:\/\/127\.0\.0\.1:\d+\/[A-Za-z0-9_-]{43}\/$/);
    assert.match(customized.vars.NANOCODEX_DEV_CHATGPT_SESSION_ID, /^[A-Za-z0-9_-]{43}$/);
    assert.equal(Object.hasOwn(customized.vars, sentinelName), false);
    assert.equal(process.env[sentinelName], "must-not-enter-workerd");
    assert.equal(config.worker.plugins().filter(({ name }) => name === "nanocodex-tools").length, 1);
  } finally {
    await plugin.closeBundle();
    await fixture.close();
    if (previousSentinel === undefined) delete process.env[sentinelName];
    else process.env[sentinelName] = previousSentinel;
  }
});

test("production builds neither read local auth nor start development egress", async () => {
  let cloudflareOptions;
  const [plugin] = createNanocodexCloudflarePlugins({
    chatGpt: { authFile: "/definitely/missing/nanocodex-auth.json" },
  }, (options) => {
    cloudflareOptions = options;
    return [];
  });
  const config = await plugin.config({ plugins: [] }, { command: "build" });
  assert.equal(cloudflareOptions.config({ vars: { PRODUCTION: "yes" } }), undefined);
  assert.equal(typeof config.worker.plugins, "function");
  await plugin.closeBundle();
});

test("the Vite plugin generates a review-first WebMCP manifest", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-vite-webmcp-"));
  try {
    await writeFile(join(directory, "client.ts"), 'fetch("/api/profile");\n');
    const plugin = nanocodex({ chatGpt: false, webMcp: true });
    plugin.configResolved({ root: directory, logger: { info() {} } });
    await plugin.buildStart();
    const manifest = JSON.parse(await readFile(join(directory, "webmcp.manifest.json"), "utf8"));
    assert.equal(manifest.tools.length, 1);
    assert.equal(manifest.tools[0].name, "get_api_profile");
    assert.equal(manifest.tools[0].approved, false);
    await plugin.closeBundle();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("the Vite development watcher regenerates changed WebMCP source", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-vite-webmcp-watch-"));
  try {
    const source = join(directory, "client.ts");
    await writeFile(source, 'fetch("/api/profile");\n');
    const watcher = new EventEmitter();
    const plugin = nanocodex({ chatGpt: false, webMcp: true });
    plugin.configResolved({ root: directory, logger: { info() {} } });
    await plugin.configureServer({
      config: { logger: { error(error) { throw error; } } },
      watcher,
    });
    await writeFile(source, 'fetch("/api/profile", { method: "POST" });\n');
    watcher.emit("change", source);
    await waitFor(async () => {
      const manifest = JSON.parse(await readFile(join(directory, "webmcp.manifest.json"), "utf8"));
      return manifest.tools[0]?.name === "post_api_profile";
    });
    await plugin.closeBundle();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("the Codex auth reader selects current subscription fields but never refresh data", async () => {
  const fixture = await authFixture();
  try {
    const auth = await readCodexSubscription(fixture.path);
    assert.equal(auth.accessToken, fixture.accessToken);
    assert.equal(auth.accountId, "account-123");
    assert.equal(auth.fedramp, false);
    assert.equal(Object.hasOwn(auth, "refreshToken"), false);
    assert.equal(Object.hasOwn(auth, "idToken"), false);

    if (process.platform !== "win32") {
      await chmod(fixture.path, 0o644);
      await assert.rejects(readCodexSubscription(fixture.path), /group or other users/);
    }
  } finally {
    await fixture.close();
  }
});

test("worker egress is loopback-only, fixed to ChatGPT, and forwards bounded HTTP and WebSockets", async () => {
  const upstreamHttp = [];
  const upstreamServer = createServer();
  const upstreamWebSockets = new WebSocketServer({ noServer: true });
  let upstreamHeaders;
  upstreamServer.on("upgrade", (request, socket, head) => {
    upstreamHeaders = request.headers;
    upstreamWebSockets.handleUpgrade(request, socket, head, (connection) => {
      connection.on("message", (data) => connection.send(data));
    });
  });
  await listen(upstreamServer);
  const upstreamAddress = upstreamServer.address();
  assert(upstreamAddress && typeof upstreamAddress !== "string");
  const egress = await startChatGptWorkerEgress({
    fetchImpl: async (url, init) => {
      upstreamHttp.push({ url, headers: new Headers(init.headers) });
      return new Response("ok", { status: 200, headers: { "content-type": "text/plain" } });
    },
    openUpstream(_fixedUrl, options) {
      return new WebSocket(`ws://127.0.0.1:${upstreamAddress.port}/responses`, options);
    },
  });
  try {
    const response = await fetch(new URL("backend-api/codex/alpha/search", egress.url), {
      headers: { authorization: "Bearer fake-access", "chatgpt-account-id": "account-123" },
    });
    assert.equal(await response.text(), "ok");
    assert.equal(upstreamHttp[0].url, "https://chatgpt.com/backend-api/codex/alpha/search");
    assert.equal(upstreamHttp[0].headers.get("authorization"), "Bearer fake-access");

    const websocketUrl = new URL("backend-api/codex/responses", egress.url);
    websocketUrl.protocol = "ws:";
    const downstream = new WebSocket(websocketUrl, {
      headers: { authorization: "Bearer fake-access", "chatgpt-account-id": "account-123" },
    });
    await once(downstream, "open");
    downstream.send("one-worker-path");
    const [message] = await once(downstream, "message");
    assert.equal(message.toString(), "one-worker-path");
    assert.equal(upstreamHeaders.authorization, "Bearer fake-access");
    assert.equal(upstreamHeaders["chatgpt-account-id"], "account-123");
    downstream.close();

    const bypass = new URL(egress.url);
    bypass.pathname = "/backend-api/codex/alpha/search";
    assert.equal((await fetch(bypass)).status, 404);
  } finally {
    await egress.close();
    for (const connection of upstreamWebSockets.clients) connection.terminate();
    await close(upstreamServer);
  }
});

async function authFixture() {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-vite-auth-"));
  const path = join(directory, "auth.json");
  const expiresAt = Math.floor(Date.now() / 1_000) + 3_600;
  const accountClaims = {
    "https://api.openai.com/auth": {
      chatgpt_account_id: "account-123",
      chatgpt_account_is_fedramp: false,
    },
  };
  const accessToken = jwt({ exp: expiresAt, ...accountClaims });
  await writeFile(path, JSON.stringify({
    auth_mode: "chatgpt",
    tokens: {
      access_token: accessToken,
      account_id: "account-123",
      id_token: jwt(accountClaims),
      refresh_token: "must-never-leave-this-file",
    },
  }), { mode: 0o600 });
  return {
    accessToken,
    path,
    close: () => rm(directory, { recursive: true, force: true }),
  };
}

function jwt(payload) {
  return [
    Buffer.from('{"alg":"none"}').toString("base64url"),
    Buffer.from(JSON.stringify(payload)).toString("base64url"),
    "signature",
  ].join(".");
}

function once(target, event) {
  return new Promise((resolve, reject) => {
    target.once(event, (...values) => resolve(values));
    target.once("error", reject);
  });
}

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
}

function close(server) {
  return new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}

async function waitFor(predicate) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (await predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail("timed out waiting for Vite WebMCP regeneration");
}
