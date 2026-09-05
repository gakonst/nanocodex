import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const execFileAsync = promisify(execFile);
const appOrigin = "https://nanocodex-connect-playground.gakonst.workers.dev";
const accountAddress = `0x${"1".repeat(40)}`;
const grantId = `0x${"2".repeat(64)}`;
const grantToken = "t".repeat(43);
const alpha = "a".repeat(43);
const bravo = "b".repeat(43);
const later = "c".repeat(43);

test("Worker connector execution fences and forwards the exact approved identity", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-connector-worker-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await execFileAsync(
    process.platform === "win32" ? "npx.cmd" : "npx",
    ["wrangler", "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir],
    { cwd: new URL("..", import.meta.url) },
  );
  const worker = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;
  let grant = activeGrant({ gmail: [alpha, bravo] });
  const forwarded = [];
  const env = {
    CONNECT_STATE: {
      idFromName: (name) => name,
      get: () => ({ fetch: async () => Response.json({
        principal: { accountAddress, appId: "atlas-workspace", appOrigin, grantId },
        grant,
      }) }),
    },
    EGRESS: { fetch: async (request) => {
      forwarded.push(request);
      return new Response("ok", { status: 200 });
    } },
  };
  const context = { waitUntil() {} };

  const accepted = await worker.fetch(egressRequest(bravo), env, context);
  assert.equal(accepted.status, 200, await accepted.clone().text());
  assert.equal(forwarded.length, 1);
  assert.equal(forwarded[0].headers.get("x-nanocodex-connector-connection"), bravo);
  assert.equal(forwarded[0].headers.get("x-nanocodex-connector-instance"), null);
  assert.equal(forwarded[0].headers.get("authorization"), "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
  assert.notEqual(forwarded[0].headers.get("authorization"), `Bearer ${grantToken}`);

  const denied = await worker.fetch(egressRequest(later), env, context);
  assert.equal(denied.status, 403);
  assert.equal((await denied.json()).error.code, "connector_connection_not_granted");
  assert.equal(forwarded.length, 1);

  grant = activeGrant(undefined);
  const legacy = await worker.fetch(egressRequest(), env, context);
  assert.equal(legacy.status, 200);
  assert.equal(forwarded.at(-1).headers.get("x-nanocodex-connector-connection"), null);
  const legacyExpansion = await worker.fetch(egressRequest(alpha), env, context);
  assert.equal(legacyExpansion.status, 403);
  assert.equal((await legacyExpansion.json()).error.code, "connector_connection_not_granted");
});

function activeGrant(connectorConnections) {
  return {
    id: grantId,
    appId: "atlas-workspace",
    appOrigin,
    accountAddress,
    brokerUserId: accountAddress,
    agentId: "agent-1",
    permission: "agent.run",
    status: "active",
    expiresAt: Math.floor(Date.now() / 1_000) + 600,
    capabilities: ["gmail"],
    ...(connectorConnections === undefined ? {} : { connectorConnections }),
    spentAtomics: "0",
    egressSubject: "s".repeat(43),
    sharedEgressSubject: true,
  };
}

function egressRequest(connectionId) {
  return new Request("https://nanocodex-connect-api.gakonst.workers.dev/v1/egress", {
    method: "POST",
    headers: {
      authorization: `Bearer ${grantToken}`,
      "content-type": "application/json",
      origin: appOrigin,
      "x-nanocodex-app-id": "atlas-workspace",
    },
    body: JSON.stringify({
      url: "https://gmail.googleapis.com/gmail/v1/users/me/messages",
      thread_id: "123e4567-e89b-42d3-a456-426614174000",
      ...(connectionId === undefined ? {} : { connection_id: connectionId }),
    }),
  });
}
