import assert from "node:assert/strict";
import { test } from "node:test";
import { execFile } from "node:child_process";
import { createServer } from "node:net";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

// Actual HTTP -> shipped account proxy -> shipped managed router/auth ->
// thread SQLite + R2 publish, then curl -> shipped Sites Worker by hostname.
// Fixture routes only create a synthetic account, thread, mount, and files.
const source = `
import worker, { DurableAgentSession } from "./src/index.ts";
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, attachAgent } from "./src/account-auth.ts";
import { createBrainBucket } from "./src/brain-bucket.ts";
import { createBrainWorkspace } from "./src/brain-workspace.ts";
import { routeManaged } from "../account/worker/managedProxy.ts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/__seed") {
      const b = await request.json();
      // A fresh Session creates its tables on its first request; this one is refused without an owner.
      await super.fetch(new Request("https://session.internal/sites"));
      this.ctx.storage.sql.exec("INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,accepted_turns,last_active) VALUES(1,?,?,?,?,?,'https://fixture.test','managed',0,123)",b.id,b.user,b.organizationId,b.teamId,b.authorizationEpoch);
      this.ctx.storage.sql.exec("INSERT INTO managed_mounts(id,provider,name,root,provider_resource_id,configuration_json,state,created_at,updated_at) VALUES(?,'cloudflare','linux','/cloudflare-linux',?,'{\\"namespace_slot\\":0}','mounted',123,123)",crypto.randomUUID(),b.id);
      return new Response(null,{status:204});
    }
    if (url.pathname === "/__brain") {
      const b = await request.json(); const id = url.searchParams.get("id");
      await createBrainWorkspace(createBrainBucket(this.ctx.storage,this.env.NANOCODEX_WORKSPACES,id),id).writeFile(b.path,b.contents);
      return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export default { async fetch(request, env, ctx) {
  const url = new URL(request.url);
  if (env.EDGE) return await routeManaged(request,env,url) ?? new Response('not_found',{status:404});
  if (url.pathname === '/__fixture') {
    const b = await request.json(); await ensureAccount(env,b.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    if (b.id) {
      await attachAgent(env,b.user,b.id);
      const r = await env.NANOCODEX_SESSIONS.getByName(b.id).fetch('https://session.internal/__seed',{method:'POST',body:JSON.stringify({...b,...auth.grant})});
      if (r.status !== 204) return r;
    }
    return Response.json(await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,credentialId:'fixture',capabilities:b.readOnly?['agents:read']:auth.grant.capabilities},'synthetic-sites-journey'));
  }
  if (url.pathname === '/__workspace') {
    const b = await request.json();
    for (const [key, contents] of Object.entries(b.files)) contents === null ? await env.NANOCODEX_WORKSPACES.delete(key) : await env.NANOCODEX_WORKSPACES.put(key, contents);
    return new Response(null,{status:204});
  }
  if (url.pathname === '/__brain') return env.NANOCODEX_SESSIONS.getByName(url.searchParams.get('id')).fetch(request);
  if (url.pathname === '/__r2') {
    const list = await env.NANOCODEX_SITES.list({ prefix: url.searchParams.get('prefix') });
    return Response.json(list.objects.map(object => object.key));
  }
  return worker.fetch(request,env,ctx);
}};
`;

const run = promisify(execFile);
const freePort = () => new Promise((resolve, reject) => {
  const server = createServer().listen(0, "127.0.0.1", () => { const { port } = server.address(); server.close(() => resolve(port)); });
  server.on("error", reject);
});

// Host links give every site its own subdomain; path links serve every site
// from one host (workers.dev), with public pages sandboxed into opaque origins.
for (const mode of ["host", "path"]) test(`published static sites are owner-only until shared, immutable per version, and stop resolving when revoked or deleted (${mode} links)`, { timeout: 180_000 }, async () => {
  const hostMode = mode === "host";
  const root = fileURLToPath(new URL("..", import.meta.url));
  const output = fileURLToPath(new URL("../../../output", import.meta.url));
  const persistence = output + "/sites-journey-store-" + crypto.randomUUID();
  const wasm = [];
  let wasmIndex = 0;
  const bundled = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false, format: "esm", target: "es2022", platform: "node", banner: { js: 'import { createRequire } from "node:module"; const require = createRequire("/worker.mjs");' }, external: ["cloudflare:*", "node:*"], alias: { "node-rsa": root + "/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" }, plugins: [{ name: "wasm-modules", setup(build) {
    build.onResolve({ filter: /\.wasm$/ }, async args => {
      const path = fileURLToPath(new URL(args.path, "file://" + args.resolveDir + "/"));
      const name = "./fixture-" + wasmIndex++ + ".wasm";
      wasm.push({ type: "CompiledWasm", path: name, contents: await readFile(path) });
      return { path: name, external: true };
    });
  } }] });
  const sites = await build({ entryPoints: [fileURLToPath(new URL("../../sites/src/index.ts", import.meta.url))], bundle: true, write: false, format: "esm", target: "es2022" });
  const port = await freePort();
  const modules = [{ type: "ESModule", path: "worker.mjs", contents: bundled.outputFiles[0].text }, ...wasm];
  const options = { port, host: "127.0.0.1", durableObjectsPersist: persistence, r2Persist: persistence + "/r2", workers: [
    { name: "edge", modules, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"], bindings: { EDGE: true }, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { name: "managed", modules, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      bindings: { NANOCODEX_SITES_ORIGIN: hostMode ? `http://*.sites.test:${port}` : `http://sites.test:${port}/*` },
      r2Buckets: { NANOCODEX_WORKSPACES: "nanocodex-sandbox-workspaces", NANOCODEX_SITES: "nanocodex-sites" },
      durableObjects: {
        NANOCODEX_SESSIONS: { className: "FixtureSession", useSQLite: true }, NANOCODEX_USERS: { className: "UserAccount", useSQLite: true }, NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true }, NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true }, NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
      } },
    { name: "sites", modules: [{ type: "ESModule", path: "sites.mjs", contents: sites.outputFiles[0].text }], compatibilityDate: "2026-07-29",
      ...(hostMode ? { routes: ["*.sites.test/*"], bindings: { SITES_DOMAIN: "sites.test" } } : { routes: ["sites.test/*"], bindings: { SITES_DOMAIN: "" } }),
      r2Buckets: { SITES: "nanocodex-sites" } },
  ] };
  let mf = new Miniflare(options);
  const trace = [];
  try {
    let backend = await mf.getWorker("managed"), base = await mf.ready;
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    const fixture = async body => { const r = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify(body) }); assert.equal(r.status, 200, await r.clone().text()); return r.json(); };
    const seed = async (path, body) => { const r = await backend.fetch(`https://fixture.test${path}?id=${id}`, { method: "POST", body: JSON.stringify(body) }); assert.equal(r.status, 204, await r.clone().text()); };
    const { token } = await fixture({ user: owner, id });
    const other = (await fixture({ user: crypto.randomUUID() })).token;
    const readOnly = (await fixture({ user: owner, readOnly: true })).token;
    async function call(path, method = "GET", body, credential = token, expected = 200) {
      const r = await fetch(new URL(path, base), { method, headers: { ...(credential ? { authorization: "Bearer " + credential } : {}), "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const text = await r.text(); let data; try { data = JSON.parse(text); } catch { data = text; }
      trace.push({ kind: "api", method, path, status: r.status, request: body, response: data });
      assert.equal(r.status, expected, `${method} ${path}: ${text}`);
      return data;
    }
    // Every site request is the curl executable resolving the site hostname to the local server.
    async function site(url, { method = "GET", headers = [], expected } = {}) {
      const target = new URL(url);
      const args = ["-sS", "-i", "-X", method, "--resolve", `${target.hostname}:${port}:127.0.0.1`, ...headers.flatMap(header => ["-H", header]), target.href];
      const { stdout } = await run("curl", args, { maxBuffer: 1 << 24 });
      const split = stdout.indexOf("\r\n\r\n");
      const [statusLine, ...lines] = stdout.slice(0, split).split("\r\n");
      const status = Number(statusLine.split(" ")[1]);
      const responseHeaders = Object.fromEntries(lines.map(line => [line.slice(0, line.indexOf(":")).toLowerCase(), line.slice(line.indexOf(":") + 1).trim()]));
      const body = stdout.slice(split + 4);
      trace.push({ kind: "site", command: ["curl", ...args].join(" "), status, headers: responseHeaders, body: body.slice(0, 200) });
      if (expected !== undefined) assert.equal(status, expected, `${method} ${url}: ${body.slice(0, 200)}`);
      return { status, headers: responseHeaders, body };
    }

    const prefix = `sessions/${id}/app/dist/`;
    await seed("/__workspace", { files: {
      [`${prefix}index.html`]: "<!doctype html><title>Launch</title><script src=/assets/app.js></script><h1>Version one</h1>",
      [`${prefix}assets/app.js`]: "console.log('v1')",
      [`${prefix}assets/logo.svg`]: "<svg xmlns='http://www.w3.org/2000/svg'/>",
      [`${prefix}docs/index.html`]: "<h1>Docs</h1>",
      [`${prefix}docs/`]: "",
      [`${prefix}.env`]: "API_KEY=synthetic-secret",
      [`${prefix}node_modules/left-pad/index.js`]: "module.exports = 1",
    } });

    const sitesPath = `/v1/agents/${id}/sites`;
    // Authorization: anonymous, another account, and a read-only key.
    await call(sitesPath, "GET", undefined, null, 401);
    await call(sitesPath, "GET", undefined, other, 404);
    await call(sitesPath, "POST", { path: "/workspace/app/dist" }, readOnly, 403);
    assert.deepEqual(await call(sitesPath, "GET", undefined, readOnly), { data: [] });

    // Representative publish failures.
    assert.equal((await call(sitesPath, "POST", { path: "/workspace/missing" }, token, 404)).error, "site_source_not_found");
    assert.equal((await call(sitesPath, "POST", { path: "/workspace/app/dist/.env" }, token, 422)).error, "site_source_excluded");
    assert.equal((await call(sitesPath, "POST", { path: "/workspace/app/dist", id: "Not Valid" }, token, 400)).error, "invalid_site_id");
    assert.equal((await call(sitesPath, "POST", { path: "/workspace/app/dist", unexpected: true }, token, 400)).error, "invalid_request");
    assert.equal((await call(sitesPath, "POST", { path: "/workspace/app/dist", entry: "missing.html" }, token, 422)).error, "site_entry_missing");

    const v1 = await call(sitesPath, "POST", { path: "/workspace/app/dist", id: "launch", title: "Launch page" }, token, 201);
    assert.deepEqual(v1, { type: "nanocodex.site", site_id: "launch", title: "Launch page", entry: "index.html", files: 4, bytes: v1.bytes, excluded: 2, version: 1, created: true });
    // Replaying an identical publish (as a recovered tool call would) does not mint a version.
    assert.deepEqual(await call(sitesPath, "POST", { path: "/workspace/app/dist", id: "launch", title: "Launch page" }), { ...v1, created: false });
    const keys = await (await backend.fetch(`https://fixture.test/__r2?prefix=threads/${id}/`)).json();
    assert.equal(keys.filter(key => key.includes("/blobs/")).length, 4, "the secret and dependency are never uploaded");
    assert.equal((await (await backend.fetch("https://fixture.test/__r2?prefix=hosts/")).json()).length, 0, "publishing alone exposes nothing");

    // The owner's private view is a short-lived host minted by the authenticated API. Its URL
    // carries a single-use grant that the first browser exchanges for a cookie scoped to the site.
    const view = await call(`${sitesPath}/launch/open`, "POST", {});
    assert.equal(view.version, 1);
    assert.ok(view.expires_at > Date.now() && view.expires_at <= Date.now() + 3_600_000);
    assert.match(view.url, /\?__nanocodex_grant=[a-z2-7]{26}$/);
    const viewBase = view.url.slice(0, view.url.indexOf("?"));
    const viewPath = new URL(viewBase).pathname;
    await site(viewBase, { expected: 404 });
    const exchanged = await site(view.url, { expected: 303 });
    assert.equal(new URL(exchanged.headers.location, viewBase).href, viewBase);
    assert.match(exchanged.headers["set-cookie"], new RegExp(`^nanocodex_site=[a-z2-7]{52}; Path=${viewPath}; HttpOnly; SameSite=Lax; Max-Age=\\d+$`));
    const session = `Cookie: ${exchanged.headers["set-cookie"].split(";")[0]}`;
    const viewed = await site(viewBase, { headers: [session], expected: 200 });
    assert.match(viewed.body, /Version one/);
    assert.equal(viewed.headers["cache-control"], "private, no-cache");
    assert.doesNotMatch(viewed.headers["content-security-policy"], /sandbox/);
    assert.equal((await site(viewBase + "assets/app.js", { headers: [session], expected: 200 })).headers["access-control-allow-origin"], undefined);
    // Without the session, nothing in the view resolves: not a replayed grant, a guessed cookie, or assets.
    await site(viewBase + "assets/app.js", { expected: 404 });
    await site(view.url, { expected: 404 });
    await site(viewBase, { headers: [`Cookie: nanocodex_site=${"a".repeat(52)}`], expected: 404 });

    // Share: anyone with the link gets the pinned version with the response policy.
    const share = await call(`${sitesPath}/launch/shares`, "POST", {}, token, 201);
    assert.equal(share.version, 1);
    assert.match(share.url, hostMode ? new RegExp(`^http://[a-z2-7]{26}\\.sites\\.test:${port}/$`) : new RegExp(`^http://sites\\.test:${port}/[a-z2-7]{26}/$`));
    const sharePath = new URL(share.url).pathname;
    const home = await site(share.url, { expected: 200 });
    assert.match(home.body, /Version one/);
    assert.equal(home.headers["content-type"], "text/html; charset=utf-8");
    if (hostMode) {
      assert.match(home.headers["content-security-policy"], /connect-src 'self'/);
      assert.doesNotMatch(home.headers["content-security-policy"], /sandbox/);
    } else {
      // Public path links share an origin, so pages are sandboxed and limited to their own prefix.
      assert.match(home.headers["content-security-policy"], /^sandbox allow-scripts /);
      assert.doesNotMatch(home.headers["content-security-policy"], /allow-same-origin/);
      assert.match(home.headers["content-security-policy"], new RegExp(`connect-src http://sites\\.test:${port}${sharePath};`));
      assert.equal(home.headers["access-control-allow-origin"], "*");
      assert.equal((await site(share.url.slice(0, -1), { expected: 308 })).headers.location, sharePath);
      await site(`http://sites.test:${port}/`, { expected: 404 });
    }
    assert.equal(home.headers["x-content-type-options"], "nosniff");
    assert.equal(home.headers["x-robots-tag"], "noindex, nofollow");
    assert.equal(home.headers["referrer-policy"], "no-referrer");
    assert.equal(home.headers["set-cookie"], undefined);
    assert.equal((await site(share.url + "assets/app.js", { expected: 200 })).headers["content-type"], "text/javascript; charset=utf-8");
    assert.equal((await site(share.url + "assets/logo.svg", { expected: 200 })).headers["content-type"], "image/svg+xml");
    await site(share.url + ".env", { expected: 404 });
    await site(share.url + "node_modules/left-pad/index.js", { expected: 404 });
    assert.equal((await site(share.url + "docs", { expected: 308 })).headers.location, `${sharePath}docs/`);
    assert.match((await site(share.url + "docs/", { expected: 200 })).body, /Docs/);
    await site(share.url + "missing", { expected: 404 });
    await site(share.url, { headers: [`If-None-Match: ${home.headers.etag}`], expected: 304 });
    await site(share.url, { method: "POST", expected: 405 });
    await site(hostMode ? `http://${"a".repeat(26)}.sites.test:${port}/` : `http://sites.test:${port}/${"a".repeat(26)}/`, { expected: 404 });

    // A new version never changes what an existing link serves.
    await seed("/__workspace", { files: { [`${prefix}index.html`]: "<!doctype html><title>Launch</title><h1>Version two</h1>" } });
    const v2 = await call(sitesPath, "POST", { path: "/cloudflare-linux/app/dist", id: "launch" }, token, 201);
    assert.equal(v2.version, 2);
    assert.equal(v2.title, "Launch page");
    assert.match((await site(share.url, { expected: 200 })).body, /Version one/);
    const latest = await call(`${sitesPath}/launch/shares`, "POST", {}, token, 201);
    assert.equal(latest.version, 2);
    assert.match((await site(latest.url, { expected: 200 })).body, /Version two/);
    assert.notEqual(latest.url, share.url);

    // A single generated file under /brain is served at /.
    await seed("/__brain", { path: "/brain/outputs/report.html", contents: "<h1>Quarterly report</h1>" });
    const report = await call(sitesPath, "POST", { path: "/brain/outputs/report.html" }, token, 201);
    assert.deepEqual([report.site_id, report.entry, report.files], ["report", "report.html", 1]);
    const reportShare = await call(`${sitesPath}/report/shares`, "POST", { expires_at: Date.now() + 86_400_000 }, token, 201);
    assert.match((await site(reportShare.url, { expected: 200 })).body, /Quarterly report/);
    assert.equal((await call(`${sitesPath}/report/shares`, "POST", { expires_at: Date.now() - 1 }, token, 400)).error, "invalid_expiry");

    const listed = await call(sitesPath);
    assert.deepEqual(listed.data.map(item => [item.id, item.latest_version, item.shares.length]).sort(), [["launch", 2, 2], ["report", 1, 1]]);
    assert.deepEqual((await call(`${sitesPath}/launch/shares`)).data.map(item => item.id).sort(), [share.id, latest.id].sort());

    // Links and versions survive a restart of every Worker.
    await mf.dispose(); mf = new Miniflare(options); backend = await mf.getWorker("managed"); base = await mf.ready;
    assert.match((await site(share.url, { expected: 200 })).body, /Version one/);

    // Revocation takes effect on the very next request and is not repeatable.
    await call(`${sitesPath}/launch/shares/${share.id}`, "DELETE", undefined, token, 204);
    await site(share.url, { expected: 404 });
    await site(share.url + "assets/app.js", { expected: 404 });
    await call(`${sitesPath}/launch/shares/${share.id}`, "DELETE", undefined, token, 404);
    await call(`${sitesPath}/launch/shares/${share.id}`, "DELETE", undefined, other, 404);
    assert.deepEqual((await call(`${sitesPath}/launch/shares`)).data.map(item => item.id), [latest.id]);
    await site(latest.url, { expected: 200 });

    // Deleting the thread removes its public links before slower cleanup completes.
    const deleted = await fetch(new URL(`/v1/agents/${id}`, base), { method: "DELETE", headers: { authorization: "Bearer " + token } });
    trace.push({ kind: "api", method: "DELETE", path: `/v1/agents/${id}`, status: deleted.status });
    await site(latest.url, { expected: 404 });
    await site(reportShare.url, { expected: 404 });
    assert.deepEqual(await (await backend.fetch("https://fixture.test/__r2?prefix=hosts/")).json(), []);
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(output + `/sites-journey-${mode}.json`, JSON.stringify({ trace }, null, 2));
    await mf.dispose();
    await rm(persistence, { recursive: true, force: true });
  }
});
