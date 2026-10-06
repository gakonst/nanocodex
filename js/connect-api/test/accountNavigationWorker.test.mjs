import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import test from "node:test";
import { Client, Transport } from "../../nanocodex/cloud/index.mjs";

test("SDK navigation links cross the public Connect Worker transport without an account", { timeout: 90000 }, async () => {
  const output = new URL("../../../output/account-workspace/connect-api/", import.meta.url);
  await mkdir(output, { recursive: true });
  await promisify(execFile)("npx", ["wrangler", "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", fileURLToPath(output)], { cwd: new URL("..", import.meta.url) });
  const worker = (await import(new URL("index.js", output))).default;
  const trace = [];
  const server = createServer(async (req, res) => {
    trace.push({ method: req.method, path: req.url });
    // The fixture translates the loopback transport to the public deployment URL.
    const response = await worker.fetch(new Request(`https://api.nanocodex.xyz${req.url}`, {
      method: req.method, headers: { ...req.headers, origin: "https://app.example" },
    }), {}, { waitUntil() {} });
    res.writeHead(response.status, Object.fromEntries(response.headers));
    res.end(Buffer.from(await response.arrayBuffer()));
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  try {
    const origin = `http://127.0.0.1:${server.address().port}`;
    const client = Client.create({ appId: "navigation-example", appOrigin: "https://app.example", transport: Transport.http(origin),
      dialog: { setup: () => ({}) }, provider: {}, session: false });
    const links = await client.account.links({ connect: "github", add: "login" });
    assert.deepEqual(links, { connections: "https://nanocodex.gakonst.workers.dev/connect?connect=github",
      vault: "https://nanocodex.gakonst.workers.dev/connect/vault?add=login", wallet: "https://nanocodex.gakonst.workers.dev/connect/wallet", access: "https://nanocodex.gakonst.workers.dev/connect/access" });
    const response = await fetch(`${origin}/v1/account/links`);
    assert.equal(response.headers.get("access-control-allow-origin"), "https://app.example");
    assert.equal(response.headers.get("cache-control"), "no-store");
    for (const suffix of ["?token=do-not-copy", "?connect=unknown", "?add=login&add=card"])
      assert.equal((await fetch(`${origin}/v1/account/links${suffix}`)).status, 400);
    assert.equal((await fetch(`${origin}/v1/account/links`, { method: "POST" })).status, 405);
    await writeFile(new URL("requests.json", output), JSON.stringify(trace, null, 2));
  } finally { await new Promise(resolve => server.close(resolve)); }
});
