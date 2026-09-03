import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createServer } from "node:http";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import WebSocket, { WebSocketServer } from "ws";

import { readCodexSubscription } from "../codex-auth-file.mjs";
import { createNanocodexCloudflarePlugins } from "../cloudflare-plugin.mjs";
import { createNanocodexVitePlugin, oauthBindingsFromEnvironment } from "../plugin.mjs";
import { startChatGptWorkerEgress } from "../chatgpt-egress.mjs";
import { isLocalNanocodexOrigin } from "../oauth-relay.mjs";

test("development applications mount their own Vite graph at safe path boundaries", async () => {
  const childRequests = [];
  const childOptions = [];
  let childClosed = false;
  let middleware;
  const httpServer = new EventEmitter();
  const plugin = createNanocodexVitePlugin({
    chatGpt: false,
    devApplications: [{
      headers: { "content-security-policy": "frame-ancestors 'self'" },
      path: "/dialog/",
      root: new URL("../../connect-dialog", import.meta.url),
    }],
  }, {
    target: "vite",
    buildJsPackage: async () => {},
    createViteServer: async (options) => {
      childOptions.push(options);
      return {
        close: async () => { childClosed = true; },
        middlewares(request, response, next) {
          childRequests.push({ method: request.method, url: request.url });
          request.url = request.url.replace("/dialog", "");
          if (request.url.startsWith("/fallthrough")) next();
          else response.emit("finish");
        },
      };
    },
  });
  await plugin.configureServer({
    config: { root: "/parent", logger: { info() {}, error() {} } },
    httpServer,
    middlewares: { use(value) { middleware = value; } },
    watcher: { add() {}, on() {}, off() {} },
  });
  assert.equal(childOptions[0].base, "/dialog/");
  assert.equal(childOptions[0].appType, "spa");
  assert.equal(childOptions[0].server.middlewareMode, true);
  assert.equal(childOptions[0].server.hmr.server, httpServer);
  assert.equal(childOptions[0].server.ws, undefined);

  const get = mockMiddlewareCall(middleware, { method: "GET", url: "/dialog/src/main.tsx?raw=one" });
  assert.equal(get.nextCalled, false);
  assert.equal(get.request.url, "/dialog/src/main.tsx?raw=one");
  assert.equal(get.responseHeaders.get("content-security-policy"), "frame-ancestors 'self'");
  assert.deepEqual(childRequests.pop(), { method: "GET", url: "/dialog/src/main.tsx?raw=one" });

  const head = mockMiddlewareCall(middleware, { method: "HEAD", url: "/dialog/?head=yes" });
  assert.equal(head.nextCalled, false);
  assert.equal(head.request.url, "/dialog/?head=yes");
  assert.deepEqual(childRequests.pop(), { method: "HEAD", url: "/dialog/?head=yes" });

  const post = mockMiddlewareCall(middleware, { method: "POST", url: "/dialog/?post=yes" });
  assert.equal(post.nextCalled, true);
  assert.equal(childRequests.length, 0);

  const outside = mockMiddlewareCall(middleware, { method: "GET", url: "/dialogue?outside=yes" });
  assert.equal(outside.nextCalled, true);
  assert.equal(childRequests.length, 0);

  const fallthrough = mockMiddlewareCall(middleware, { method: "GET", url: "/dialog/fallthrough?kept=yes" });
  assert.equal(fallthrough.nextCalled, true);
  assert.equal(fallthrough.request.url, "/dialog/fallthrough?kept=yes");
  await plugin.closeBundle();
  assert.equal(childClosed, true);
});

test("development application mounts reject ambiguous or unsafe paths", () => {
  const create = (devApplications) => createNanocodexVitePlugin(
    { chatGpt: false, devApplications },
    { target: "vite" },
  );
  assert.throws(() => create([{ path: "/", root: "." }]), /non-root URL path/);
  assert.throws(() => create([{ path: "/safe/%2e%2e", root: "." }]), /safe URL segments/);
  assert.throws(() => create([
    { path: "/same", root: "." },
    { path: "/same/", root: "." },
  ]), /duplicate path/);
});

test("the OAuth relay accepts canonical and high-port secure worktree origins", () => {
  assert.equal(isLocalNanocodexOrigin("https://nanocodex.localhost"), true);
  assert.equal(isLocalNanocodexOrigin("https://deploy-simplify.nanocodex.localhost"), true);
  assert.equal(isLocalNanocodexOrigin("https://deploy-simplify.nanocodex.localhost:1355"), true);
  assert.equal(isLocalNanocodexOrigin("https://deploy-simplify.nanocodex.localhost:4430.evil.test"), false);
});

test("one Vite plugin gives only the selected broker exact development bindings", async () => {
  const fixture = await authFixture();
  const sentinelName = "NANOCODEX_TEST_UNRELATED_HOST_SECRET";
  const previousSentinel = process.env[sentinelName];
  process.env[sentinelName] = "must-not-enter-workerd";
  let cloudflareOptions;
  const plugins = createNanocodexCloudflarePlugins({
    chatGpt: {
      authFile: fixture.path,
      credentialBrokerWorker: "nanocodex-egress",
    },
    oauthRelay: true,
    cloudflare: {
      config: () => ({ vars: { APPLICATION_VAR: "kept" } }),
      auxiliaryWorkers: [
        {
          configPath: "egress.jsonc",
          config: () => ({ vars: { BROKER_VAR: "kept" } }),
          devOnly: true,
        },
        { configPath: "managed.jsonc", devOnly: true },
      ],
    },
  }, (options) => {
    cloudflareOptions = options;
    return [{ name: "vite-plugin-cloudflare" }];
  }, {
    buildJsPackage: async () => {},
    loadOAuthBindings: async () => ({
      GITHUB_OAUTH_CLIENT_ID: "github-id",
      GITHUB_OAUTH_CLIENT_SECRET: "github-secret",
      SLACK_OAUTH_CLIENT_ID: "slack-id",
      SLACK_OAUTH_CLIENT_SECRET: "slack-secret",
    }),
  });
  const plugin = plugins[0];
  try {
    const config = await plugin.config({
      worker: {},
    }, { command: "serve" });
    assert.equal(config.define.__NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET__, "true");
    const application = cloudflareOptions.config({ vars: { FROM_WRANGLER: "kept-too" } });
    assert.deepEqual(application.vars, { APPLICATION_VAR: "kept" });
    const broker = cloudflareOptions.auxiliaryWorkers[0].config({
      name: "nanocodex-egress",
      vars: { ENVIRONMENT: "development", FROM_WRANGLER: "kept-too" },
    }, { entryWorkerConfig: {} });
    assert.equal(broker.vars.BROKER_VAR, "kept");
    assert.equal(broker.vars.ENVIRONMENT, "development");
    assert.equal(broker.vars.FROM_WRANGLER, "kept-too");
    assert.equal(broker.vars.ALLOW_INSECURE_LOOPBACK_RELAY, "true");
    assert.equal(broker.vars.NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET, "true");
    assert.equal(
      broker.vars.NANOCODEX_SPONSORED_CHATGPT_USER_ID,
      "00000000-0000-4000-8000-000000000001",
    );
    assert.equal(Object.hasOwn(broker.vars, "NANOCODEX_LOCAL_CHATGPT_AUTO_CLAIM"), false);
    assert.equal(broker.vars.GITHUB_OAUTH_CLIENT_ID, "github-id");
    assert.equal(broker.vars.GITHUB_OAUTH_CLIENT_SECRET, "github-secret");
    assert.equal(broker.vars.SLACK_OAUTH_CLIENT_ID, "slack-id");
    assert.equal(broker.vars.SLACK_OAUTH_CLIENT_SECRET, "slack-secret");
    assert.match(
      broker.vars.CODEX_RELAY_URL,
      /^http:\/\/127\.0\.0\.1:\d+\/v1\/[A-Za-z0-9_-]{43}$/,
    );
    const bootstrap = JSON.parse(broker.vars.LOCAL_CHATGPT_BOOTSTRAP);
    assert.deepEqual(bootstrap, {
      access_token: fixture.accessToken,
      account_id: "account-123",
      expires_at: fixture.expiresAt,
      fedramp: false,
    });
    assert.equal(Object.hasOwn(broker.vars, "NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN"), false);
    assert.equal(Object.hasOwn(broker.vars, sentinelName), false);
    const managed = cloudflareOptions.auxiliaryWorkers[1].config({
      name: "nanocodex-durable-agent",
      vars: { MANAGED_VAR: "kept" },
    }, { entryWorkerConfig: {} });
    assert.equal(managed, undefined);
    assert.equal(process.env[sentinelName], "must-not-enter-workerd");
    assert.equal(config.worker.plugins().filter(({ name }) => name === "nanocodex-tools").length, 1);
  } finally {
    await plugin.closeBundle();
    await fixture.close();
    if (previousSentinel === undefined) delete process.env[sentinelName];
    else process.env[sentinelName] = previousSentinel;
  }
});

test("Slack OAuth credentials are paired and normalized only into broker bindings", () => {
  assert.deepEqual(oauthBindingsFromEnvironment({
    NANOCODEX_SLACK_OAUTH_CLIENT_ID: " slack-id ",
    NANOCODEX_SLACK_OAUTH_CLIENT_SECRET: " slack-secret ",
  }), {
    SLACK_OAUTH_CLIENT_ID: "slack-id",
    SLACK_OAUTH_CLIENT_SECRET: "slack-secret",
  });
  assert.deepEqual(oauthBindingsFromEnvironment({
    SLACK_CLIENT_ID: "fallback-id",
    SLACK_CLIENT_SECRET: "fallback-secret",
  }), {
    SLACK_OAUTH_CLIENT_ID: "fallback-id",
    SLACK_OAUTH_CLIENT_SECRET: "fallback-secret",
  });
  assert.throws(() => oauthBindingsFromEnvironment({
    SLACK_CLIENT_ID: "orphan-id",
  }), /Slack OAuth client ID and secret must be configured together/);
});

test("production builds neither read local auth nor start development egress", async () => {
  let cloudflareOptions;
  const packageBuildModes = [];
  const [plugin] = createNanocodexCloudflarePlugins({
    chatGpt: { authFile: "/definitely/missing/nanocodex-auth.json" },
  }, (options) => {
    cloudflareOptions = options;
    return [];
  }, { buildJsPackage: async (release) => packageBuildModes.push(release) });
  const config = await plugin.config({ plugins: [] }, { command: "build" });
  assert.deepEqual(packageBuildModes, [true]);
  assert.equal(config.define.__NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET__, "false");
  assert.equal(cloudflareOptions.config({ vars: { PRODUCTION: "yes" } }), undefined);
  assert.equal(cloudflareOptions.config({
    vars: { NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET: "must-not-be-overridden" },
  }), undefined);
  assert.equal(typeof config.worker.plugins, "function");
  await plugin.closeBundle();
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

    const relayedSearch = await fetch(`${egress.relayUrl}/http/codex-web-search`, {
      headers: { authorization: "Bearer relay-access" },
    });
    assert.equal(await relayedSearch.text(), "ok");
    assert.equal(upstreamHttp[1].url, "https://chatgpt.com/backend-api/codex/alpha/search");
    assert.equal(upstreamHttp[1].headers.get("authorization"), "Bearer relay-access");

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

    const relayWebsocketUrl = new URL(egress.relayUrl);
    relayWebsocketUrl.protocol = "ws:";
    const relayedDownstream = new WebSocket(relayWebsocketUrl, {
      headers: { authorization: "Bearer relay-access" },
    });
    await once(relayedDownstream, "open");
    relayedDownstream.send("broker-relay-path");
    const [relayedMessage] = await once(relayedDownstream, "message");
    assert.equal(relayedMessage.toString(), "broker-relay-path");
    relayedDownstream.close();

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
    expiresAt: expiresAt * 1_000,
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

function mockMiddlewareCall(middleware, request) {
  const response = new EventEmitter();
  const responseHeaders = new Map();
  response.setHeader = (name, value) => responseHeaders.set(name.toLowerCase(), String(value));
  let nextCalled = false;
  middleware(request, response, (error) => {
    if (error) throw error;
    nextCalled = true;
  });
  return {
    request,
    responseHeaders,
    get nextCalled() { return nextCalled; },
  };
}
