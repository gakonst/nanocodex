import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

import { connectorCapabilities } from "../src/connectorPolicy.mts";

const execFileAsync = promisify(execFile);
const dialogOrigin = "https://nanocodex.gakonst.workers.dev";
const brokerUserId = "00000000-0000-4000-8000-000000000000";
const connectionId = "m".repeat(43);
const sessionToken = "t".repeat(43);
const grantToken = "g".repeat(43);
const grantId = `0x${"a".repeat(64)}`;
const oauthConnectionId = "c".repeat(43);
const principal = Object.freeze({
  kind: "host",
  id: "p".repeat(43),
  app_id: "acme",
  app_origin: "https://app.example",
  issuer: "https://identity.example/",
  tenant: "acme-production",
  session_epoch: 7,
  session_digest: "s".repeat(43),
});

test("host principal OAuth and account-info protocol retains exact private fences", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-host-principal-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await execFileAsync(
    process.platform === "win32" ? "npx.cmd" : "npx",
    ["wrangler", "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir],
    { cwd: new URL("..", import.meta.url) },
  );
  const worker = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;
  const context = { waitUntil() {} };

  await t.test("connector and MCP start states persist the complete private principal", async () => {
    const entries = new Map([
      [`hosted-browser-session:${sessionToken}`, {
        kind: "host",
        appId: principal.app_id,
        appOrigin: principal.app_origin,
        brokerUserId,
        connectors: ["github"],
        expiresAt: Math.floor(Date.now() / 1_000) + 300,
        mcpIds: [connectionId],
        principal,
      }],
    ]);
    const created = [];
    const env = protocolEnv({
      entries,
      created,
      accountResponse: () => Response.json({ active: true, user_id: brokerUserId }),
      brokerResponse(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname.endsWith("/connectors/github")) {
          return Response.json({
            authorization_url: "https://github.com/login/oauth/authorize?state=github-state-1234",
          });
        }
        if (request.method === "GET" && url.pathname.endsWith("/mcp-connections")) {
          return Response.json({
            mcp_connections: [{ id: connectionId, name: "Hosted MCP", status: "authorization_required" }],
          });
        }
        if (request.method === "POST" && url.pathname.endsWith(`/mcp-connections/${connectionId}/start`)) {
          return Response.json({ authorization_url: "https://mcp.example/authorize?state=mcp-state" });
        }
        throw new Error(`unexpected broker request ${request.method} ${url.pathname}`);
      },
    });
    const headers = { authorization: `Bearer ${sessionToken}`, origin: dialogOrigin };
    const connectorResponse = await worker.fetch(new Request(`${dialogOrigin}/v1/connectors/github`, {
      method: "POST",
      headers,
    }), env, context);
    assert.equal(connectorResponse.status, 200);
    const mcpResponse = await worker.fetch(new Request(`${dialogOrigin}/v1/mcp-connections/${connectionId}`, {
      method: "POST",
      headers: { ...headers, "content-type": "application/json" },
      body: "{}",
    }), env, context);
    assert.equal(mcpResponse.status, 200);

    assert.equal(created.length, 2);
    for (const { value } of created) {
      assert.deepEqual(value.hostPrincipal, principal);
      assert.equal("hostPrincipalId" in value, false);
      assert.equal(value.accountAddress, undefined);
      assert.equal(value.brokerUserId, brokerUserId);
    }
  });

  await t.test("connector and MCP callbacks consume state then reject a rotated session before broker mutation", async () => {
    const states = new Map([
      ["connector-state:connector-state", {
        hostPrincipal: principal,
        brokerUserId,
        dialogOrigin,
        provider: "github",
      }],
      ["mcp-connection-state:mcp-state", {
        hostPrincipal: principal,
        brokerUserId,
        connectionId,
        dialogOrigin,
      }],
      ["connector-state:legacy-state", {
        hostPrincipalId: principal.id,
        brokerUserId,
        dialogOrigin,
        provider: "github",
      }],
    ]);
    const validations = [];
    let brokerMutations = 0;
    const env = protocolEnv({
      entries: states,
      accountResponse: async (request) => {
        validations.push(await request.json());
        return Response.json({ error: "host_session_fenced" }, { status: 403 });
      },
      brokerResponse() {
        brokerMutations += 1;
        return Response.json({ connected: true });
      },
    });

    const connectorResponse = await worker.fetch(new Request(
      `${dialogOrigin}/v1/connectors/github/callback?state=connector-state&code=code`,
    ), env, context);
    assert.equal(connectorResponse.status, 403);
    assert.match(await connectorResponse.text(), /host_principal_inactive/);

    const mcpResponse = await worker.fetch(new Request(
      `${dialogOrigin}/v1/mcp-connections/${connectionId}/callback?state=mcp-state&code=code`,
    ), env, context);
    assert.equal(mcpResponse.status, 403);
    assert.match(await mcpResponse.text(), /host_principal_inactive/);

    assert.deepEqual(validations, [{ principal }, { principal }]);
    assert.equal(brokerMutations, 0);

    const legacyState = await worker.fetch(new Request(
      `${dialogOrigin}/v1/connectors/github/callback?state=legacy-state&code=code`,
    ), env, context);
    assert.equal(legacyState.status, 400, "an ID-only host correlation fails closed");
    assert.equal(validations.length, 2);
    assert.equal(brokerMutations, 0);
    assert.equal(states.size, 0, "all callback correlations are atomically consumed");

    const replay = await worker.fetch(new Request(
      `${dialogOrigin}/v1/connectors/github/callback?state=connector-state&code=code`,
    ), env, context);
    assert.equal(replay.status, 400);
  });

  await t.test("host account-info exposes only grant connectors with public identity and hosted authority", async () => {
    const grant = {
      id: grantId,
      appId: principal.app_id,
      appOrigin: principal.app_origin,
      brokerUserId,
      agentId: "agent-1",
      permission: "agent.run",
      status: "active",
      expiresAt: Math.floor(Date.now() / 1_000) + 300,
      capabilities: ["nanocodex.agent", "github"],
      connectorConnections: { github: [oauthConnectionId] },
      mcpConnections: [],
      spentAtomics: "0",
      egressSubject: "e".repeat(43),
      sharedEgressSubject: true,
      hostPrincipal: principal,
      resources: ["urn:nanocodex:agent:run"],
    };
    const grantSession = {
      appId: principal.app_id,
      appOrigin: principal.app_origin,
      grantId,
      hostPrincipal: principal,
    };
    const env = protocolEnv({
      resolvedGrant: { principal: grantSession, grant },
      accountResponse: () => Response.json({ active: true, user_id: brokerUserId }),
      brokerResponse(request) {
        const url = new URL(request.url);
        if (url.pathname.endsWith("/connectors")) {
          return Response.json({
            connectors: {
              github: {
                connected: true,
                connections: [{ id: oauthConnectionId, label: "approved-github" }],
              },
              gmail: {
                connected: true,
                connections: [{ id: "d".repeat(43), label: "ambient-gmail" }],
              },
            },
          });
        }
        if (url.pathname.endsWith("/credentials")) {
          return Response.json({ chatgpt: { connected: true, label: "ambient-chatgpt" } });
        }
        throw new Error(`unexpected broker request ${request.method} ${url.pathname}`);
      },
    });
    const response = await worker.fetch(new Request(
      "https://nanocodex-connect-api.gakonst.workers.dev/v1/agent/account-info",
      {
        headers: {
          authorization: `Bearer ${grantToken}`,
          origin: principal.app_origin,
          "x-nanocodex-app-id": principal.app_id,
        },
      },
    ), env, context);
    assert.equal(response.status, 200);
    const body = await response.json();
    assert.deepEqual(body.connectors.github, {
      connected: true,
      connections: [{ id: oauthConnectionId, label: "approved-github" }],
      label: "approved-github",
    });
    assert.deepEqual(Object.keys(body.connectors), [...connectorCapabilities]);
    for (const capability of connectorCapabilities.filter((value) => value !== "github")) {
      assert.deepEqual(body.connectors[capability], { connected: false, connections: [] });
    }
    assert.deepEqual(body.identity, {
      hostPrincipal: { kind: "host", id: principal.id },
    });
    assert.deepEqual(body.stablecoins, []);
    assert.equal(body.authorizations.length, 1);
    assert.equal(body.authorizations[0].authority, "hosted");
    assert.deepEqual(body.authorizations[0].connectors, ["github"]);
  });
});

function protocolEnv({
  entries = new Map(),
  created = [],
  accountResponse,
  brokerResponse,
  resolvedGrant,
}) {
  const stub = {
    async fetch(input, init) {
      const request = input instanceof Request ? input : new Request(input, init);
      const url = new URL(request.url);
      if (url.pathname === "/resolve-grant") return Response.json(resolvedGrant ?? {});
      const key = url.searchParams.get("key");
      if (!key) return Response.json({ error: "missing key" }, { status: 400 });
      if (url.pathname === "/get") return Response.json({ value: entries.get(key) });
      if (url.pathname === "/take") {
        const value = entries.get(key);
        entries.delete(key);
        return Response.json({ value });
      }
      if (url.pathname === "/create") {
        const { value } = await request.json();
        if (entries.has(key)) return Response.json({ created: false });
        entries.set(key, value);
        created.push({ key, value });
        return Response.json({ created: true });
      }
      return Response.json({ error: `unexpected state operation ${url.pathname}` }, { status: 500 });
    },
  };
  return {
    CONNECT_STATE: { idFromName: (name) => name, get: () => stub },
    ACCOUNTS: { fetch: accountResponse },
    EGRESS: { fetch: brokerResponse },
    NANOCODEX: { fetch: () => Response.json({ error: "unexpected managed request" }, { status: 500 }) },
  };
}
