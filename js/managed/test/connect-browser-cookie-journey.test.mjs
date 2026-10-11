import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare, Log, LogLevel } from "miniflare";
import { chromium } from "../../connect-dialog/node_modules/@playwright/test/index.mjs";
import { fetch } from "./support/miniflare-fetch.mjs";

// Real HTTPS browser, production account proxy, account routes and SQLite DOs.
// Twilio Verify, wallet provisioning and the requesting app metadata are fixtures.
const root = fileURLToPath(new URL("../../../", import.meta.url));
const output = new URL("../../../output/cookie-profile/", import.meta.url);
const common = { compatibilityDate: "2026-08-23", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
const object = className => ({ className, useSQLite: true });
async function bundle(entry) {
  const bundled = await build({
    stdin: { contents: entry, resolveDir: root }, bundle: true, write: false,
    format: "esm", target: "es2022", platform: "browser",
    external: ["cloudflare:*", "node:*"],
    alias: { "node-rsa": root + "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
  });
  return [{ type: "ESModule", path: "worker.mjs", contents: bundled.outputFiles[0].text }];
}

test("Browser Connect SMS stores and reuses persistent account cookie", { timeout: 180_000 }, async t => {
  const trace = [];
  let passed = false, mf, base, browser;
  let sends = 0, checks = 0, provisions = 0;
  const sids = new Set();
  const phone = "+12025550173", code = "654321";
  const managed = await bundle(`
    import { routeAccountRequest } from "./js/managed/src/account-auth.ts";
    export { UserAccount, Organization, ApiKeyRecord, NonceStorage } from "./js/managed/src/account-auth.ts";
    export default { async fetch(request, env) {
      return await routeAccountRequest(request, env, new URL(request.url)) ?? new Response(null, { status: 404 });
    }};
  `);
  async function outbound(request) {
    const url = new URL(request.url);
    assert.equal(url.origin, "https://verify.twilio.com", `Unexpected external request ${url.origin}${url.pathname}; no live network permitted`);
    const form = new URLSearchParams(await request.text());
    if (url.pathname.endsWith("/Verifications")) {
      assert.equal(form.get("To"), phone);
      const sid = "VE" + (++sends).toString(16).padStart(32, "0");
      sids.add(sid);
      return Response.json({ sid, status: "pending" }, { status: 201 });
    }
    assert.ok(url.pathname.endsWith("/VerificationCheck"));
    checks++;
    const approved = sids.has(form.get("VerificationSid")) && form.get("Code") === code;
    if (approved) sids.delete(form.get("VerificationSid"));
    return Response.json({ status: approved ? "approved" : "pending" });
  }
  const frontend = await build({
    stdin: { contents: 'import React from "react"; import {createRoot} from "react-dom/client"; import {OAuthConsent} from "./js/connect-dialog/src/OAuthConsent.tsx"; import "nanocodex-connect-ui/styles.css"; createRoot(document.getElementById("root")).render(<OAuthConsent />);', resolveDir: root, loader: "tsx" },
    bundle: true, write: false, outdir: root + "output/cookie-profile/frontend", format: "esm",
    jsx: "automatic", define: { "process.env.NODE_ENV": '"production"' },
    alias: { "react": root + "js/connect-dialog/node_modules/react", "react-dom": root + "js/connect-dialog/node_modules/react-dom", "nanocodex-connect-ui/ConnectionLogo": root + "js/nanocodex-connect-ui/src/ConnectionLogo.tsx", "nanocodex-connect-ui/browserAccountSession": root + "js/nanocodex-connect-ui/src/browserAccountSession.ts", "nanocodex-connect-ui/connectPolicy.mjs": root + "js/nanocodex-connect-ui/src/connectPolicy.mts", "nanocodex-connect-ui/App": root + "js/nanocodex-connect-ui/src/App.tsx", "nanocodex-connect-ui/styles.css": root + "js/nanocodex-connect-ui/styles.css" },
  });
  const assets = Object.fromEntries(frontend.outputFiles.map(file => [file.path.endsWith(".css") ? "/fixture.css" : "/fixture.js", file.text]));
  const edge = await bundle(`
    import { routeManaged } from "./js/account/worker/managedProxy.ts";
    import { routeConnectDialog } from "./js/account/worker/connectDialogProxy.ts";
    export default { async fetch(request, env) {
      const url = new URL(request.url);
      if (url.pathname.startsWith("/oauth/requests/")) {
        const appId = "mcp:" + "c".repeat(43);
        const resources = ["urn:nanocodex:app:" + encodeURIComponent(appId), "urn:nanocodex:origin:" + encodeURIComponent(url.origin), "urn:nanocodex:authorization:hosted", "urn:nanocodex:agent:run", "urn:nanocodex:agent:output:final"];
        return Response.json({client_id:"c".repeat(43),client_name:"Synthetic cookie journey",app_id:appId,app_origin:url.origin,redirect_uri:url.origin+"/callback",resource:url.origin+"/mcp",scope:"agent:run",resources,base_resources:resources,scope_resources:{"agent:run":["urn:nanocodex:agent:output:final"]}});
      }
      return await routeConnectDialog(request, env, url) ?? await routeManaged(request, env, url) ?? new Response("missing", {status:404});
    }};
  `);
  try {
    await mkdir(output, {recursive:true});
    mf = new Miniflare({ https: true, log: new Log(LogLevel.ERROR), workers: [
      { name: "edge", ...common, modules: edge, serviceBindings: { NANOCODEX_BACKEND: "managed", NANOCODEX_CONNECT_DIALOG: "assets" } },
      { name: "assets", ...common, modules: true, script: `const assets = ${JSON.stringify(assets)}; export default { fetch(request) { const path = new URL(request.url).pathname; return new Response(assets[path] ?? '<html><head><link rel="stylesheet" href="/connect-dialog/fixture.css"></head><body><div id="root"></div><script type="module" src="/connect-dialog/fixture.js"></script></body></html>', {headers:{"content-type":path.endsWith(".js")?"text/javascript":path.endsWith(".css")?"text/css":"text/html"}}); } };` },
      { name: "managed", ...common, modules: managed, outboundService: outbound,
        bindings: { ENVIRONMENT: "test", NANOCODEX_OTP_HMAC_KEY: "synthetic-connect-journey-otp-key",
          TWILIO_ACCOUNT_SID: "AC" + "1".repeat(32), TWILIO_AUTH_TOKEN: "synthetic-twilio-token", TWILIO_VERIFY_SERVICE_SID: "VA" + "1".repeat(32) },
        serviceBindings: { NANOCODEX: async request => {
          assert.ok(["PUT", "GET"].includes(request.method)); if (request.method === "PUT") provisions++;
          return Response.json({ address: "0x" + "1".repeat(40), created_at: 1 });
        } },
        durableObjects: { NANOCODEX_AUTH: object("NonceStorage"),
          NANOCODEX_USERS: object("UserAccount"), NANOCODEX_ORGANIZATIONS: object("Organization"), NANOCODEX_API_KEYS: object("ApiKeyRecord") } },
    ] });
    base = new URL(await mf.ready); base.hostname = "modal.nanocodex.localhost";
    browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE, args: ["--host-resolver-rules=MAP *.nanocodex.localhost 127.0.0.1"] });
    const context = await browser.newContext({ ignoreHTTPSErrors: true });
    const page = await context.newPage();
    page.on("pageerror", error => trace.push({pageError:error.message}));
    page.on("response", response => { const url = new URL(response.url()); if (url.pathname.startsWith("/v1/")) trace.push({ path: url.pathname, method: response.request().method(), status: response.status() }); });
    await page.goto(new URL("/connect-dialog?oauth_request=" + "a".repeat(43), base).href);
    await page.getByRole("textbox", {name:"Mobile number"}).fill(phone);
    await page.getByRole("button", {name:"Text me a code"}).click();
    await page.getByRole("textbox", {name:"6-digit code"}).fill(code);
    await page.getByRole("button", {name:"Continue", exact:true}).click();
    await page.getByRole("button", {name:"Allow access", exact:true}).waitFor();
    const cookies = await context.cookies();
    const account = cookies.find(cookie => cookie.name === "nanocodex_account");
    assert.ok(account, "SMS sets account cookie in real browser jar");
    assert.equal(account.httpOnly, true); assert.equal(account.secure, true);
    assert.equal(account.sameSite, "Lax"); assert.equal(account.path, "/");
    assert.ok(account.expires > Date.now()/1000 + 364*86400);
    assert.equal(await page.evaluate(() => document.cookie.includes("nanocodex_account")), false);
    trace.push({label:"browser-stored account cookie", httpOnly:account.httpOnly, secure:account.secure, sameSite:account.sameSite, path:account.path, lifetime_days:Math.round((account.expires-Date.now()/1000)/86400)});
    await page.screenshot({path: new URL("first-consent.png", output).pathname});
    await page.close();
    const second = await context.newPage();
    second.on("response", response => { const url = new URL(response.url()); if (url.pathname.startsWith("/v1/")) trace.push({flow:2,path:url.pathname,status:response.status()}); });
    await second.goto(new URL("/connect-dialog?oauth_request=" + "a".repeat(43), base).href);
    await second.getByRole("button", {name:"Allow access", exact:true}).waitFor();
    assert.equal(await second.getByRole("textbox", {name:"Mobile number"}).count(), 0);
    assert.equal(await second.evaluate(() => window.__hostReceipt !== undefined), false);
    await second.screenshot({path: new URL("returning-consent.png", output).pathname});
    const me = await second.evaluate(async () => { const r=await fetch("/v1/me?connect=1"); const value=await r.json(); return {status:r.status, authentication:value.authentication, persistent:value.user?.persistent, timing:r.headers.get("server-timing")}; });
    assert.equal(me.status, 200);
    assert.equal(me.authentication, "account_session");
    assert.equal(me.persistent, true);
    assert.match(me.timing, /^connect_session;dur=\d+\.\d, connect_metadata;dur=\d+\.\d, connect_total;dur=\d+\.\d$/);
    assert.equal(trace.some(entry => entry.path?.endsWith("/authorize")), false);
    trace.push({label:"second browser flow authenticated",...me});
    assert.equal(sends, 1); assert.equal(checks, 1);
    // Exercise the API-key /me path without the browser session cookie, then
    // revoke through the signed-in browser and require immediate rejection.
    const issued = await context.request.post(new URL("/v1/api-keys", base).href, {
      headers: { origin: base.origin }, data: { label: "Synthetic session profile" },
    });
    assert.equal(issued.status(), 201);
    const key = await issued.json();
    const apiContext = await browser.newContext({ ignoreHTTPSErrors: true });
    try {
      const requestMe = () => apiContext.request.get(new URL("/v1/me", base).href, {
        headers: { authorization: "Bearer " + key.api_key },
      });
      const keyed = await requestMe();
      assert.equal(keyed.status(), 200);
      const keyedAccount = await keyed.json();
      assert.equal(keyedAccount.authentication, "api_key");
      assert.equal(keyedAccount.user.persistent, true);
      const revoked = await context.request.delete(new URL("/v1/api-keys/" + key.key.id, base).href, {
        headers: { origin: base.origin },
      });
      assert.equal(revoked.status(), 204);
      assert.equal((await requestMe()).status(), 401);
      trace.push({ label: "API-key account metadata remains live and revoked key is rejected", passed: true });
    } finally { await apiContext.close(); }
    const logout = await second.evaluate(async () => (await fetch("/v1/auth/logout", {method:"POST"})).status);
    assert.equal(logout, 204);
    assert.equal((await context.cookies()).some(c => c.name === "nanocodex_account"), false);
    await second.reload();
    await second.getByRole("textbox", {name:"Mobile number"}).waitFor();
    trace.push({label:"logout clears cookie and returns sign-in", status:logout});
    await second.screenshot({path: new URL("logout-signin.png", output).pathname});
    // Synthetic cookie only: browser expiry must prevent reuse on the next entry.
    await context.addCookies([{...account, expires: Math.floor(Date.now()/1000)-1}]);
    await second.reload();
    await second.getByRole("textbox", {name:"Mobile number"}).waitFor();
    assert.equal((await context.cookies()).some(c => c.name === "nanocodex_account"), false);
    assert.equal(sends, 1); assert.equal(checks, 1);
    trace.push({label:"expired browser cookie returns sign-in without sending SMS", passed:true});
    passed = true;
    t.diagnostic("Fresh browser SMS and second Connect reuse passed through production account proxies");
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(new URL("http-trace.json", output), JSON.stringify({ command: "node --test js/managed/test/connect-browser-cookie-journey.test.mjs", passed,
      inputs: "Synthetic phone and OTP. Session cookies, signatures, IDs and bearer tokens omitted.",
      expected: "Browser SMS login stores persistent protected cookie; closing and reopening Connect reuses session without another SMS and still requires consent.",
      limits: "Synthetic browser/identity, Twilio, wallet provisioning and requesting-app metadata fixtures. Real account proxy, managed auth routes, SQLite state and frontend. No broker/model runtime, grant approval, live SMS or production mutation. Browser expiry and logout are exercised.", counts: {sends, checks, provisions}, trace }, null, 2));
    await browser?.close();
    await mf?.dispose();
  }
});
