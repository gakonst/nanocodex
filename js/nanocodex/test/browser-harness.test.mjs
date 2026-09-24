import assert from "node:assert/strict";
import { test } from "node:test";

import { bindBrowser } from "../tools/browser/index.mjs";
import * as datasets from "../tools/dataset.mjs";
import { namedTool } from "../tools/namedTool.mjs";
import * as standard from "../tools/standard.mjs";
import { X_API } from "nanocodex-tools/x";

const context = Object.freeze({
  callId: "browser-harness-call",
  parentCallId: "",
  sessionId: "browser-harness-session",
  signal: new AbortController().signal,
});
const LOGIN_ID = "l".repeat(22);
const CARD_ID = "c".repeat(22);
const ADDRESS_ID = "a".repeat(22);
const PHONE_ID = "p".repeat(22);

const shellDescriptor = Object.freeze({
  shell: "nanocodex-just-bash",
  commands: Object.freeze(["curl", "gh", "git", "python3"]),
  customCommands: Object.freeze(["gh", "git", "python3"]),
  cwd: "/workspace",
  limits: Object.freeze({ maxFileSystemBytes: 256 * 1024 * 1024 }),
  network: Object.freeze({ enabled: true, mode: "connector-http-gateway" }),
  pty: false,
  sessions: false,
  sandboxEscalation: false,
});

test("browser browseX reaches the app origin for profiles and posts without X authorization", async () => {
  const requests = [];
  const result = { markdown: "Public X data", data: { posts: [] } };
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async (input, init) => {
      requests.push(new Request(input, init));
      return Response.json(result);
    },
  });
  const tool = runtime.tools.find(({ name }) => name === "browseX");
  assert.ok(tool, "the browser tool catalog must expose browseX");
  assert.deepEqual(await tool.handler({ action: "profile", handle: "gakonst", limit: 1 }, context), result);
  assert.deepEqual(await tool.handler({ action: "post", url: "https://x.com/jack/status/20" }, context), result);
  assert.deepEqual(requests.map(({ url }) => url), [
    "https://demo.test/api/tools/x/browse?resource=profile&handle=gakonst&limit=1&format=json",
    "https://demo.test/api/tools/x/convert?url=https%3A%2F%2Fx.com%2Fjack%2Fstatus%2F20&format=json",
  ]);
  assert.ok(requests.every((request) => request.credentials === "same-origin"));
  assert.ok(requests.every((request) => request.headers.get("x-nanocodex-request") === "1"));
  assert.ok(requests.every((request) => request.headers.get("accept") === "application/json"));
  const aborted = AbortSignal.abort();
  await assert.rejects(tool.handler({ action: "profile", handle: "gakonst" }, { ...context, signal: aborted }));
  assert.equal(requests.length, 2, "cancelled tool calls must not fetch");
});

test("browser browseX preserves provider failure and retry information", async () => {
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async () => Response.json({ error: "rate limited", retry_after: 60 }, { status: 429 }),
  });
  const tool = runtime.tools.find(({ name }) => name === "browseX");
  assert.deepEqual(await tool.handler({ action: "profile", handle: "gakonst" }, context), {
    status: "unavailable", http_status: 429, error: "rate limited", retry_after: 60,
  });
});

test("account connection links reject unexpected provider and callback URLs", async () => {
  for (const authorization_url of [
    "https://attacker.test/oauth?client_id=x&state=y&scope=z&redirect_uri=https%3A%2F%2Fdemo.test%2Fv1%2Fconnectors%2Fgoogle%2Fcallback",
    providerAuthorizationUrl("google", "https://attacker.test/v1/connectors/google/callback"),
    providerAuthorizationUrl("google", "http://127.0.0.1:47891/v1/connectors/google/callback"),
    `${providerAuthorizationUrl("google", "https://demo.test/v1/connectors/google/callback")}&client_secret=secret`,
  ]) {
    const runtime = bindBrowser({
      ...preparedBrowser(),
      fetch: async () => Response.json({ authorization_url }),
    }, { accountConnectionRequests: true });
    const connection = runtime.tools.find(({ name }) => name === "requestAccountConnection");
    await assert.rejects(
      connection.handler({ connector: "gmail" }, context),
      /invalid authorization URL/,
    );
  }
});

test("account connection links accept the fixed local OAuth relay only for local Nanocodex", async () => {
  const authorization_url = providerAuthorizationUrl(
    "google",
    "http://127.0.0.1:47891/v1/connectors/google/callback",
  );
  const runtime = bindBrowser({
    ...preparedBrowser(),
    origin: "https://nanocodex.localhost",
    fetch: async () => Response.json({ authorization_url }),
  }, { accountConnectionRequests: true });
  const connection = runtime.tools.find(({ name }) => name === "requestAccountConnection");
  assert.equal(
    (await connection.handler({ connector: "gdrive" }, context)).authorization_url,
    authorization_url,
  );
});

test("environment adds app authorization without forwarding unknown control-plane fields", async () => {
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async () => Response.json({
      connectors: { chatgpt: { connected: true, label: "Subscription" } },
      identity: { tempoAddress: "0xabc", brokerUserId: "secret" },
      stablecoins: [{
        token: "0x01",
        symbol: "MACH",
        balance: "5000000",
        decimals: 6,
        providerCredential: "secret",
      }],
      authorizations: [{
        appId: "atlas-workspace",
        permission: "agent.run",
        status: "active",
        expiresAt: 2_000_000_000,
        capabilities: ["nanocodex.agent", "x", "chatgpt"],
        connectors: ["x", "chatgpt"],
        connectorConnections: { x: ["x".repeat(43)] },
        accessKey: {
          id: "0x02",
          expiry: 2_000_000_000,
          limits: [{ token: "0x01", symbol: "MACH", limit: "10000000", period: 86_400 }],
          scopes: [{ address: "0x03", selector: "0x12345678", recipients: ["0x04"] }],
          witness: "secret",
        },
        spend: {
          token: "0x01",
          symbol: "MACH",
          spent: "250000",
          limit: "10000000",
          period: 86_400,
          maxPerRequest: "250000",
          credential: "secret",
        },
        grantToken: "secret",
      }],
    }),
  }, { accountInfo: { requireAuthorization: true } });
  const accountInfo = runtime.tools.find(({ name }) => name === "environment");

  assert.deepEqual(await accountInfo.handler({}, context), {
    status: "ready",
    apis: [X_API],
    runtime: "browser-worker", default_cwd: "/workspace", hands: {},
    accounts: { chatgpt: { label: "Subscription", connections: [] } },
    identity: { tempoAddress: "0xabc" },
    stablecoins: [{ token: "0x01", symbol: "MACH", balance: "5000000", decimals: 6 }],
    authorizations: [{
      appId: "atlas-workspace",
      permission: "agent.run",
      status: "active",
      expiresAt: 2_000_000_000,
      capabilities: ["nanocodex.agent", "x", "chatgpt"],
      connectors: ["x", "chatgpt"],
      connectorConnections: { x: ["x".repeat(43)] },
      accessKey: {
        id: "0x02",
        expiry: 2_000_000_000,
        limits: [{ token: "0x01", symbol: "MACH", limit: "10000000", period: 86_400 }],
        scopes: [{ address: "0x03", selector: "0x12345678", recipients: ["0x04"] }],
      },
      spend: {
        token: "0x01",
        symbol: "MACH",
        spent: "250000",
        limit: "10000000",
        period: 86_400,
        maxPerRequest: "250000",
      },
    }],
    vault: [],
  });
});

test("environment projects a bounded host identity and hosted authorization", async () => {
  const hostPrincipalId = "p".repeat(43);
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async () => Response.json({
      connectors: { github: { connected: true, label: "Host GitHub" } },
      identity: {
        hostPrincipal: { kind: "host", id: hostPrincipalId },
        issuer: "private-host-claim",
      },
      stablecoins: [],
      authorizations: [{
        appId: "host-workspace",
        permission: "agent.run",
        status: "active",
        expiresAt: 2_000_000_000,
        capabilities: ["nanocodex.agent", "github"],
        connectors: ["github"],
        authority: "hosted",
        grantToken: "secret",
      }],
    }),
  }, { accountInfo: { requireAuthorization: true } });
  const accountInfo = runtime.tools.find(({ name }) => name === "environment");

  assert.deepEqual(await accountInfo.handler({}, context), {
    status: "ready",
    apis: [X_API],
    runtime: "browser-worker", default_cwd: "/workspace", hands: {},
    accounts: { github: { label: "Host GitHub", connections: [] } },
    identity: { hostPrincipal: { kind: "host", id: hostPrincipalId } },
    stablecoins: [],
    authorizations: [{
      appId: "host-workspace",
      permission: "agent.run",
      status: "active",
      expiresAt: 2_000_000_000,
      capabilities: ["nanocodex.agent", "github"],
      connectors: ["github"],
      authority: "hosted",
    }],
    vault: [],
  });
});

test("environment fails the complete Vault projection closed on unknown secret fields", async () => {
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async () => Response.json({
      connectors: { github: { connected: true, label: "octocat" } },
      vault: [
        { id: PHONE_ID, kind: "phone", name: "Mobile", created_at: 1, phone_number: "+301234567890" },
        {
          id: CARD_ID,
          kind: "card",
          name: "Work card",
          created_at: 2,
          last4: "4242",
          card_number: "4242424242424242",
          cvv: "123",
          expiry_month: "12",
          expiry_year: "2030",
          billing_zip: "10557",
        },
      ],
    }),
  });
  const accountInfo = runtime.tools.find(({ name }) => name === "environment");

  const result = await accountInfo.handler({}, context);
  assert.deepEqual(result.vault, []);
  assert.equal(JSON.stringify(result).includes("4242424242424242"), false);
  assert(accountInfo.outputSchema.required.includes("vault"));
  assert.equal(accountInfo.outputSchema.properties.vault.items.oneOf.length, 4);
  assert(accountInfo.outputSchema.properties.vault.items.oneOf.every((variant) => (
    variant.additionalProperties === false && variant.required.includes("created_at")
  )));
  assert.match(accountInfo.description, /safe Vault references/);
  assert.match(accountInfo.description, /never include passwords, full card numbers, CVVs, expiry details, or billing ZIPs/);
});

test("environment includes an empty required Vault field in login and unavailable outputs", async () => {
  for (const [response, expectedStatus] of [
    [new Response(null, { status: 401 }), "requires_login"],
    [new Response(null, { status: 503 }), "unavailable"],
  ]) {
    const runtime = bindBrowser({
      ...preparedBrowser(),
      fetch: async () => response,
    });
    const accountInfo = runtime.tools.find(({ name }) => name === "environment");

    assert.deepEqual(await accountInfo.handler({}, context), {
      status: expectedStatus,
      apis: [X_API],
      runtime: "browser-worker", default_cwd: "/workspace", hands: {},
      accounts: {},
      identity: {},
      stablecoins: [],
      authorizations: [],
      vault: [],
    });
  }
});

test("environment rejects Vault metadata outside broker-compatible bounds", async () => {
  const malformedVaults = [
    [{ id: "short", kind: "login", name: "Example", created_at: 1, username: "nanocat" }],
    [{ id: LOGIN_ID, kind: "login", name: " Example", created_at: 1, username: "nanocat" }],
    [{ id: CARD_ID, kind: "card", name: "Card", created_at: 1, last4: "123" }],
    [{
      id: ADDRESS_ID,
      kind: "address",
      name: "Office",
      created_at: 1,
      address_line_1: "a".repeat(257),
      city: "Athens",
      state: "Attica",
      zip: "10557",
      country: "GR",
    }],
    Array.from({ length: 101 }, (_, index) => ({
      id: index.toString().padStart(22, "p"),
      kind: "phone",
      name: "Mobile",
      created_at: index,
      phone_number: "+301234567890",
    })),
  ];
  for (const vault of malformedVaults) {
    const runtime = bindBrowser({
      ...preparedBrowser(),
      fetch: async () => Response.json({ connectors: {}, vault }),
    });
    const accountInfo = runtime.tools.find(({ name }) => name === "environment");

    assert.deepEqual((await accountInfo.handler({}, context)).vault, []);
  }
});

function providerAuthorizationUrl(provider, redirectUri) {
  const authorization = new URL(provider === "google"
    ? "https://accounts.google.com/o/oauth2/v2/auth"
    : `https://${provider}.example/authorize`);
  authorization.search = new URLSearchParams({
    client_id: "client-id",
    redirect_uri: redirectUri,
    response_type: "code",
    scope: "openid email",
    state: "opaque-state",
    code_challenge: "A".repeat(43),
    code_challenge_method: "S256",
  }).toString();
  return authorization.href;
}

function preparedBrowser() {
  const workspace = { async readFile() { return new Uint8Array(); } };
  return {
    datasets,
    fetch: async () => Response.json({ connectors: {} }),
    origin: "https://demo.test",
    standard,
    threadId: "browser-harness-overrides",
    shell: {
      descriptor: shellDescriptor,
      artifactTool: namedTool("render_artifact", {
        description: "Render an artifact.",
        handler: async () => ({ artifactId: "ui" }),
      }),
      execTool: { description: "Run a command.", handler: async () => ({}) },
      instructions: "browser harness",
      projectInstructions: "project instructions",
      workspace,
    },
  };
}


test("browser agents request music account connections through the phone", async () => {
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async () => { throw new Error("Music connections must use the phone OAuth flow"); },
  }, { accountConnectionRequests: true });
  const tool = runtime.tools.find(({ name }) => name === "requestAccountConnection");
  for (const [connector, label] of [["spotify", "Spotify"], ["soundcloud", "SoundCloud"]]) {
    const result = await tool.handler({ connector }, context);
    assert.equal(result.authorization_url, `nanocodex://connect/${connector}`);
    assert.equal(result.status, "authorization_required");
    assert.equal(result.label, label);
    assert.equal(result.expires_in_seconds, undefined, "the app link has no OAuth expiry before consent starts");
  }
  assert(!tool.outputSchema.required.includes("expires_in_seconds"));
});

test("browser agents accept Link device authorization and reject lookalike origins", async () => {
  let authorizationUrl = "https://login.link.com/verify?code=test";
  const requests = [];
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async (url, init) => {
      requests.push({ url: String(url), init });
      return Response.json({ authorization_url: authorizationUrl, attempt: "a".repeat(43) });
    },
  }, { accountConnectionRequests: true });
  const tool = runtime.tools.find(({ name }) => name === "requestAccountConnection");
  const result = await tool.handler({ connector: "link" }, context);
  assert.equal(result.authorization_url, authorizationUrl);
  assert.equal(result.label, "Stripe Link");
  assert.equal(result.status, "authorization_required");
  assert.equal(requests[0].url, "https://demo.test/v1/connectors/link");
  assert.equal(requests[0].init.credentials, "same-origin");
  authorizationUrl = "https://login.link.com.evil.example/verify?code=test";
  await assert.rejects(tool.handler({ connector: "link" }, context), /authorization/i);
});
