import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { saveDeviceHandLogin } from "../src/device-hand.mjs";

const binary = process.env.NANOCODEX_DEVICE_TEST_BINARY;
test("app login uses the real native private store without changing the CLI login", { skip: !binary, timeout: 15_000 }, async t => {
  const home = await mkdtemp(join(tmpdir(), "ncx-app-login-"));
  const globalLogin = join(home, ".codex/nanocodex-account.json");
  await mkdir(join(home, ".codex"));
  await writeFile(globalLogin, "global-cli-login-must-remain-unchanged", { mode: 0o600 });
  const accountFile = join(home, "app/hand-accounts/synthetic-device.json");
  let denied = false, identityRequests = 0;
  const server = createServer((request, response) => {
    assert.equal(request.url, "/v1/me");
    assert.match(request.headers.authorization, /^Bearer ncx_live_/);
    identityRequests++;
    response.setHeader("content-type", "application/json");
    if (denied) { response.writeHead(401); response.end('{"error":"unauthorized"}'); return; }
    response.end(JSON.stringify({ authentication: "api_key", user: { id: "synthetic-user" }, organization: { id: "synthetic-org" }, team: { id: "synthetic-team" }, role: "owner" }));
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); await rm(home, { recursive: true, force: true }); });
  const origin = `http://127.0.0.1:${server.address().port}`;
  const key = character => `ncx_live_${character.repeat(12)}_${character.repeat(43)}`;
  const env = character => ({ PATH: process.env.PATH, HOME: home, USERPROFILE: home, NANOCODEX_MANAGED_URL: origin,
    NANOCODEX_API_KEY: key(character), NC_API_KEY: key("x") });
  let launches = 0;
  const spawnProcess = (executable, args, options) => {
    launches++;
    assert.deepEqual(args, ["account", "login", "--with-api-key", "--no-hand"]);
    assert.equal(options.env.NANOCODEX_API_KEY, undefined);
    assert.equal(options.env.NC_API_KEY, undefined);
    assert.equal(options.env.NANOCODEX_ACCOUNT_FILE, accountFile);
    assert.deepEqual(options.stdio, ["pipe", "ignore", "ignore"]);
    return spawn(executable, args, options);
  };
  await saveDeviceHandLogin(binary, env("a"), accountFile, { spawnProcess });
  assert.equal(JSON.parse(await readFile(accountFile, "utf8")).accounts[origin].api_key, key("a"));
  if (process.platform !== "win32") assert.equal((await stat(accountFile)).mode & 0o777, 0o600);
  await saveDeviceHandLogin(binary, env("b"), accountFile, { spawnProcess });
  const rotated = await readFile(accountFile, "utf8");
  assert.equal(JSON.parse(rotated).accounts[origin].api_key, key("b"));
  denied = true;
  await assert.rejects(saveDeviceHandLogin(binary, env("c"), accountFile, { spawnProcess }), /Could not save the verified Hand login/);
  assert.equal(await readFile(accountFile, "utf8"), rotated, "Rejected credentials preserve the existing service login");
  assert.equal(await readFile(globalLogin, "utf8"), "global-cli-login-must-remain-unchanged");
  assert.equal(identityRequests, 3);
  assert.equal(launches, 3);
});
