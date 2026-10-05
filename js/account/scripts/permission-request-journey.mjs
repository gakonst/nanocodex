// Real Chromium -> account proxy -> managed auth/permission routes -> SQLite DOs.
// Only initial account/key/session enrollment is synthetic; consent and data access are real.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { chromium } from "playwright-core";
const managedRequire = createRequire(new URL("../../managed/package.json", import.meta.url));
const { Miniflare } = managedRequire("miniflare");
const output = new URL("../../../output/permission-request-web/", import.meta.url);
await mkdir(output, { recursive: true });
const trace = [];
const ui = await build({ loader: { ".png": "dataurl" }, stdin: { contents: `
import React from "react"; import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query"; import { appQueryClient } from "./src/queryClient";
import { AccountSessionProvider } from "./src/AccountSession"; import { PermissionRequestPage } from "./src/PermissionRequestPage";
import "nanocodex-connect-ui/styles.css";
createRoot(document.getElementById("root")).render(<QueryClientProvider client={appQueryClient}><AccountSessionProvider><PermissionRequestPage url={new URL(location.href)} /></AccountSessionProvider></QueryClientProvider>);
`, resolveDir: fileURLToPath(new URL("..", import.meta.url)), loader: "tsx" },
  bundle: true, write: false, outfile: "app.js", jsx: "automatic", tsconfigRaw: { compilerOptions: { jsx: "react-jsx" } } });
const backend = await build({ stdin: { contents: `
import { Kv } from "accounts/server";
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, routeAccountRequest } from "./src/account-auth.ts";
import { routeUserDataRequest } from "./src/user-data-route.ts";
export { UserDataScope } from "./src/user-data-scope.ts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export default { async fetch(request, env) {
 const url = new URL(request.url);
 if (url.pathname === "/__enroll") {
   const input = await request.json(); await ensureAccount(env, input.user, true);
   const auth = await (await env.NANOCODEX_USERS.getByName(input.user).fetch("https://user.internal/authorization")).json();
   const key = await createApiKey(env, { kind:"api_key", userId:input.user, ...auth.grant,
     subjectId:"api_key:"+input.user, credentialId:"fixture", capabilities:["agents:read","agents:write","tools:use"] }, "Synthetic existing laptop");
   const cookie = "s_" + btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32)))).replaceAll("+","-").replaceAll("/","_").replaceAll("=","");
   await Kv.durableObject(env.NANOCODEX_AUTH, {name:"account"}).set("session:"+cookie, { authentication:"sms_otp", userId:input.user,
     issuedAt:Math.floor(Date.now()/1000), expiresAt:Math.floor(Date.now()/1000)+3600 }, {ttl:3600});
   return Response.json({key, cookie});
 }
 return await routeAccountRequest(request, env, url) ?? await routeUserDataRequest(request, env, url) ?? new Response(null,{status:404});
}};`, resolveDir: fileURLToPath(new URL("../../managed", import.meta.url)) },
  bundle: true, write: false, format: "esm", target: "es2022", platform: "browser",
  external: ["cloudflare:workers", "node:*"], alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" } });
const edge = await build({ stdin: { contents: `
import { routeManaged } from "./worker/managedProxy.ts";
export default { async fetch(request, env) {
 const url = new URL(request.url);
 if (url.pathname === "/ui.js") return new Response(env.UI_JS,{headers:{"content-type":"text/javascript"}});
 if (url.pathname === "/ui.css") return new Response(env.UI_CSS,{headers:{"content-type":"text/css"}});
 if (url.pathname === "/") return new Response('<meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/ui.css"><div id="root"></div><script src="/ui.js"></script>',{headers:{"content-type":"text/html"}});
 return await routeManaged(request, env, url) ?? new Response(null,{status:404});
}};`, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
  bundle: true, write: false, format: "esm", platform: "browser", target: "es2022", external: ["cloudflare:workers", "node:*"] });
const mf = new Miniflare({ workers: [
  { name: "edge", script: edge.outputFiles[0].text, modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    bindings: { UI_JS: ui.outputFiles.find(f => f.path.endsWith(".js")).text, UI_CSS: ui.outputFiles.find(f => f.path.endsWith(".css"))?.text ?? "" },
    serviceBindings: { NANOCODEX_BACKEND: "managed" } },
  { name: "managed", script: backend.outputFiles[0].text, modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    durableObjects: Object.fromEntries(Object.entries({ NANOCODEX_AUTH: "NonceStorage", NANOCODEX_USERS: "UserAccount", NANOCODEX_ORGANIZATIONS: "Organization", NANOCODEX_API_KEYS: "ApiKeyRecord", NANOCODEX_USER_DATA: "UserDataScope" }).map(([name,className]) => [name,{className,useSQLite:true}])),
    r2Buckets: ["NANOCODEX_USER_DATA_OBJECTS"] },
] });
let browser;
try {
  const origin = (await mf.ready).origin;
  const managed = await mf.getWorker("managed");
  async function enroll() {
    const response = await managed.fetch("https://fixture.test/__enroll", { method: "POST", body: JSON.stringify({user:crypto.randomUUID()}) });
    assert.equal(response.status,200,await response.clone().text());
    return response.json();
  }
  async function api(path, {token, method="GET", body, expected=200}={}) {
    const response = await fetch(origin + path, {method, headers:{ ...(token ? {authorization:"Bearer "+token}:{}), "content-type":"application/json" }, ...(body ? {body:JSON.stringify(body)}:{})});
    const value = await response.json(); trace.push({method,path,expected,observed:response.status,status:value.status,error:value.error});
    assert.equal(response.status,expected,JSON.stringify(value)); return value;
  }
  async function request(fixture, capabilities=["data:read","data:write"], reason="Save and read my app records") {
    return api("/v1/permission-requests", {token:fixture.key.token,method:"POST",body:{operation_id:crypto.randomUUID(),capabilities,reason}});
  }
  browser = await chromium.launch({ headless:true, ...(process.env.CHROME_PATH ? {executablePath:process.env.CHROME_PATH}:{}) });
  const errors=[];
  async function pageFor(fixture, width=1000) {
    const context = await browser.newContext({viewport:{width,height:900}});
    if(fixture) await context.addCookies([{name:"nanocodex_account",value:fixture.cookie,url:origin,httpOnly:true,sameSite:"Lax"}]);
    const page = await context.newPage(); page.setDefaultTimeout(15_000);
    page.on("pageerror",e=>errors.push(e.message));
    page.on("request",r=>{if(new URL(r.url()).pathname.startsWith("/v1/"))trace.push({browser:true,method:r.method(),path:new URL(r.url()).pathname});});
    return {page,context};
  }
  for (const width of [1100,390]) {
    const fixture=await enroll(), pending=await request(fixture);
    await api("/v1/data",{token:fixture.key.token,method:"POST",body:{operation:"document_put",key:"com.example/consent",value:width},expected:403});
    await api(`/v1/permission-requests/${pending.key_id}/${pending.request_id}/approve`,{token:fixture.key.token,method:"POST",body:{},expected:403});
    const {page,context}=await pageFor(fixture,width);
    const start=trace.length;
    await page.goto(pending.approval_url);
    await page.getByRole("button",{name:"Approve permissions",exact:true}).waitFor();
    assert.equal(trace.slice(start).some(t=>t.method==="POST"),false,"Opening consent cannot decide or log in");
    assert.equal(await page.getByText("data:read",{exact:true}).count(),1);
    assert.equal(await page.getByText("data:write",{exact:true}).count(),1);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth <= innerWidth),true);
    await page.screenshot({path:fileURLToPath(new URL(`consent-${width}.png`,output)),fullPage:true});
    await page.getByRole("button",{name:"Approve permissions",exact:true}).click();
    await page.getByRole("status").filter({hasText:"Permissions approved"}).waitFor();
    assert.equal(trace.slice(start).filter(t=>t.browser && t.method==="POST").length,1);
    assert.equal(trace.slice(start).some(t=>t.path.startsWith("/v1/auth/")),false,"Existing session must not reauthenticate");
    await api("/v1/data",{token:fixture.key.token,method:"POST",body:{operation:"document_put",key:"com.example/consent",value:width}});
    const stored=await api("/v1/data",{token:fixture.key.token,method:"POST",body:{operation:"document_get",key:"com.example/consent"}});
    assert.equal(stored.document.value,width,"Same API key works after explicit browser approval");
    await context.close();
    trace.push({journey:"approve-existing-key",width,result:"passed"});
  }
  // A different authenticated account sees no consent controls and cannot approve.
  const owner=await enroll(), other=await enroll(), denied=await request(owner);
  let {page,context}=await pageFor(other);
  await page.goto(denied.approval_url); await page.getByRole("alert").filter({hasText:"unavailable for this account"}).waitFor();
  assert.equal(await page.getByRole("button",{name:"Approve permissions",exact:true}).count(),0); await context.close();
  ({page,context}=await pageFor(owner));
  await page.goto(denied.approval_url); await page.getByRole("button",{name:"Deny",exact:true}).click();
  await page.getByRole("status").filter({hasText:"Request denied"}).waitFor();
  await api("/v1/data",{token:owner.key.token,method:"POST",body:{operation:"document_list"},expected:403});
  await page.reload(); await page.getByRole("status").filter({hasText:"Request denied"}).waitFor(); await context.close();
  trace.push({journey:"wrong-account-and-denial",result:"passed"});
  // The real decision commits but the network loses its response: no automatic retry.
  const uncertainOwner=await enroll(), uncertain=await request(uncertainOwner);
  ({page,context}=await pageFor(uncertainOwner));
  let approvals=0;
  await page.route("**/approve",async route=>{approvals++;const response=await route.fetch();assert.equal(response.status(),200);await route.abort("failed");});
  await page.goto(uncertain.approval_url); await page.getByRole("button",{name:"Approve permissions",exact:true}).click();
  await page.getByRole("alert").waitFor(); assert.equal(approvals,1);
  assert.equal(await page.getByRole("button",{name:"Approve permissions",exact:true}).isDisabled(),true);
  await page.getByRole("button",{name:"Check request status",exact:true}).click();
  await page.getByRole("status").filter({hasText:"Permissions approved"}).waitFor(); assert.equal(approvals,1); await context.close();
  trace.push({journey:"lost-response-reconcile-with-get",result:"passed"});
  // Signed-out UI keeps the original review URL and resumes after session restoration.
  const resumeOwner=await enroll(), resume=await request(resumeOwner, ["data:read"], "<img src=x onerror=alert(1)> requester text");
  ({page,context}=await pageFor());
  await page.clock.install();
  await page.goto(resume.approval_url); await page.getByRole("textbox",{name:"Mobile number",exact:true}).waitFor();
  assert.equal(page.url(),resume.approval_url);
  await context.addCookies([{name:"nanocodex_account",value:resumeOwner.cookie,url:origin,httpOnly:true,sameSite:"Lax"}]);
  await page.reload(); await page.getByRole("button",{name:"Approve permissions",exact:true}).waitFor();
  assert.equal(page.url(),resume.approval_url); assert.equal(await page.locator(".permission-request-reason img").count(),0);
  assert.equal(await page.locator(".permission-request-reason").textContent(),"<img src=x onerror=alert(1)> requester text");
  await page.clock.fastForward(16*60*1000);
  await page.getByRole("status").filter({hasText:"has expired"}).waitFor();
  assert.equal(await page.getByRole("button",{name:"Approve permissions",exact:true}).count(),0);
  await page.goto(resume.approval_url+"&permission_request="+crypto.randomUUID());
  await page.getByRole("alert").filter({hasText:"Invalid permission request link"}).waitFor();
  await context.close(); trace.push({journey:"resume-query-escape-requester-text-expiry-invalid-link",result:"passed"});
  assert.deepEqual(errors,[]);
  await writeFile(new URL("trace.json",output),JSON.stringify({command:"pnpm --filter nanocodex-web run test:permissions",initialEnrollment:"synthetic only",transport:"Chromium -> real account proxy -> real managed auth and permission routes -> SQLite and R2",trace},null,2));
  console.log("Permission consent browser journeys passed. Evidence:",fileURLToPath(output));
} catch(error) {
  await writeFile(new URL("trace-failed.json",output),JSON.stringify({error:String(error),trace},null,2)); throw error;
} finally { await browser?.close(); await mf.dispose(); }
