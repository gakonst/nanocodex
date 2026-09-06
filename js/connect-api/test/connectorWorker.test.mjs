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
  let reply = () => new Response("ok", { status: 200 });
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
      return reply();
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

  grant = { ...activeGrant({ github: [alpha] }), capabilities: ["github"] };
  const bytes = Uint8Array.from({ length: 300 * 1024 }, (_, index) => index % 256);
  const largeSize = 17 * 1024 * 1024 + 123;
  reply = () => {
    let remaining = largeSize;
    return new Response(new ReadableStream({
      pull(controller) {
        if (!remaining) { controller.close(); return; }
        const size = Math.min(remaining, 64 * 1024);
        controller.enqueue(new Uint8Array(size).fill(255));
        remaining -= size;
      },
    }), { headers: { "content-type": "application/x-git-upload-pack-result" } });
  };
  const cloned = await worker.fetch(egressRequest(undefined, {
    url: "https://github.com/fixture/large.git/git-upload-pack",
    method: "POST",
    headers: { "content-type": "application/x-git-upload-pack-request", "git-protocol": "version=2" },
    body_base64: Buffer.from(bytes).toString("base64"),
  }), env, context);
  assert.equal(cloned.status, 200);
  const git = forwarded.at(-1);
  assert.equal(git.url, "https://github.com/fixture/large.git/git-upload-pack");
  assert.equal(git.headers.get("x-nanocodex-subject"), grant.egressSubject);
  assert.equal(git.headers.get("x-nanocodex-connector-connection"), alpha);
  assert.equal(git.headers.get("git-protocol"), "version=2");
  assert.deepEqual(new Uint8Array(await git.arrayBuffer()), bytes);
  const downloaded = new Uint8Array(await cloned.arrayBuffer());
  assert.equal(downloaded.byteLength, largeSize);
  assert(downloaded.every((byte) => byte === 255));

  reply = () => new Response("public");
  const publicResponse = await worker.fetch(egressRequest(undefined, {
    url: "https://example.com/archive", method: "POST", body_base64: "AP+A/g==",
  }), env, context);
  assert.equal(await publicResponse.text(), "public");
  const publicRequest = forwarded.at(-1);
  assert.equal(publicRequest.url, "https://public-egress.internal/v1/request");
  assert.equal(publicRequest.headers.get("x-nanocodex-target-url"), "https://example.com/archive");
  assert.equal(publicRequest.headers.get("x-nanocodex-subject"), grant.egressSubject);
  assert.equal(publicRequest.headers.get("authorization"), null);
  assert.deepEqual([...new Uint8Array(await publicRequest.arrayBuffer())], [0, 255, 128, 254]);

  const before = forwarded.length;
  const spoofed = await worker.fetch(egressRequest(undefined, {
    url: "https://example.com", headers: { "x-nanocodex-target-url": "https://api.github.com/user" },
  }), env, context);
  assert.equal(spoofed.status, 403);
  const outsideGrant = await worker.fetch(egressRequest(bravo, {
    url: "https://github.com/fixture/large.git/info/refs?service=git-upload-pack",
  }), env, context);
  assert.equal(outsideGrant.status, 403);
  assert.equal(forwarded.length, before);

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

function egressRequest(connectionId, fields = {}) {
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
      ...fields,
    }),
  });
}
