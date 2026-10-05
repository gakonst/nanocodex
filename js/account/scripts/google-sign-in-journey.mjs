// Chromium -> shipped AccountChooser/AccountSession -> account proxy -> managed routes -> SQLite DOs.
// Google and Twilio are the only simulated services; wallet creation uses the real credential broker.
import assert from "node:assert/strict";
import { generateKeyPairSync, createHash, sign } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { chromium } from "playwright-core";
const managedRequire = createRequire(new URL("../../managed/package.json", import.meta.url));
const { Miniflare } = managedRequire("miniflare");
const output = process.env.JOURNEY_OUTPUT ? new URL("file://"+process.env.JOURNEY_OUTPUT+"/") : new URL("../../../output/google-sign-in-web/", import.meta.url);
await mkdir(output, { recursive: true });
const trace = [], browserErrors = [];
const command = "pnpm --filter nanocodex-web run test:google-sign-in";
const { publicKey, privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = { ...publicKey.export({format:"jwk"}), kid:"journey-rsa", alg:"RS256", use:"sig" };
const clientId = "journey-client.apps.googleusercontent.com";
const codes = new Map();
let subject = "synthetic-google-subject-001";
function jwt(nonce, sub) {
  const now = Math.floor(Date.now()/1000);
  const header = Buffer.from(JSON.stringify({alg:"RS256",typ:"JWT",kid:jwk.kid})).toString("base64url");
  const payload = Buffer.from(JSON.stringify({iss:"https://accounts.google.com",aud:clientId,sub,nonce,iat:now,exp:now+3600,email:"journey@example.test",email_verified:true})).toString("base64url");
  const unsigned = header+"."+payload;
  return unsigned+"."+sign("RSA-SHA256",Buffer.from(unsigned),privateKey).toString("base64url");
}
async function outbound(request) {
  const url = new URL(request.url);
  trace.push({external:true,method:request.method,host:url.host,path:url.pathname});
  if (url.href === "https://www.googleapis.com/oauth2/v3/certs") return Response.json({keys:[jwk]});
  if (url.href === "https://oauth2.googleapis.com/token") {
    const form = new URLSearchParams(await request.text());
    const fixture = codes.get(form.get("code"));
    assert.ok(fixture,"Provider receives the code issued by its authorization page");
    assert.equal(form.get("client_id"),clientId);
    assert.equal(form.get("client_secret"),"synthetic-client-secret");
    assert.equal(form.get("redirect_uri"),fixture.redirect);
    assert.equal(createHash("sha256").update(form.get("code_verifier")).digest("base64url"),fixture.challenge);
    codes.delete(form.get("code"));
    return Response.json({id_token:jwt(fixture.nonce,fixture.sub),access_token:"synthetic-unused-access-token",token_type:"Bearer",expires_in:3600});
  }
  if (url.host === "verify.twilio.com") {
    const form = new URLSearchParams(await request.text());
    if(url.pathname.endsWith("/Verifications")) { assert.equal(form.get("To"),"+12025550100"); return Response.json({sid:"VE"+"0".repeat(32),status:"pending"}); }
    if(url.pathname.endsWith("/VerificationCheck")) return Response.json({status:form.get("Code")==="123456"?"approved":"pending"});
  }
  throw new Error("Unexpected external request: "+request.method+" "+url.origin+url.pathname);
}
const ui = await build({ stdin:{contents:`
import React from "react"; import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query"; import { appQueryClient } from "./src/queryClient";
import { AccountSessionProvider, useAccountSession } from "./src/AccountSession";
import { AccountSignInMethods } from "./src/AccountSignInMethods";
import { AccountChooser } from "nanocodex-connect-ui/AccountChooser"; import "nanocodex-connect-ui/styles.css";
function Journey() { const session = useAccountSession(); return session.status === "checking" ? <p role="status">Checking account</p> : session.account?.persistent ? <main><h1>Account ready</h1><p data-testid="account-id">{session.account.id}</p><AccountSignInMethods/><button onClick={()=>session.signOut()}>Sign out</button></main> : <AccountChooser disabled={session.operation!==null} failure={session.error} onChooseAccount={selection=>void session.chooseAccount(selection)}/>; }
createRoot(document.getElementById("root")).render(<QueryClientProvider client={appQueryClient}><AccountSessionProvider><div className="connect-onboarding dialog-shell"><Journey/></div></AccountSessionProvider></QueryClientProvider>);
`,resolveDir:fileURLToPath(new URL("..",import.meta.url)),loader:"tsx"},bundle:true,write:false,outfile:"app.js",loader:{".png":"dataurl"},alias:{"nanocodex-connect-ui/GoogleSignInButton":fileURLToPath(new URL("../../nanocodex-connect-ui/src/GoogleSignInButton.tsx",import.meta.url)),"nanocodex-connect-ui/AccountChooser":fileURLToPath(new URL("../../nanocodex-connect-ui/src/AccountChooser.tsx",import.meta.url)),"nanocodex-connect-ui/browserAccountSession":fileURLToPath(new URL("../../nanocodex-connect-ui/src/browserAccountSession.ts",import.meta.url))},jsx:"automatic",tsconfigRaw:{compilerOptions:{jsx:"react-jsx"}} });
const backend = await build({ stdin:{contents:`
import { routeAccountRequest } from "./src/account-auth.ts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage } from "./src/account-auth.ts";
export default { async fetch(request,env) { return await routeAccountRequest(request,env,new URL(request.url)) ?? new Response(null,{status:404}); }};
`,resolveDir:fileURLToPath(new URL("../../managed",import.meta.url))},bundle:true,write:false,format:"esm",target:"es2022",platform:"browser",external:["cloudflare:workers","node:*"],alias:{"nanocodex-tools/hosted":fileURLToPath(new URL("../../nanocodex-tools/src/hosted/index.ts",import.meta.url)),"node-rsa":fileURLToPath(new URL("../../nanocodex/tools/browser/unsupportedNodeRsa.mjs",import.meta.url))} });
const edge = await build({ stdin:{contents:`
import { routeManaged } from "./worker/managedProxy.ts";
import { routeConnectApi } from "./worker/connectApiProxy.ts";
export default { async fetch(request,env) {
 const url = new URL(request.url);
 if(url.pathname === "/ui.js") return new Response(env.UI_JS,{headers:{"content-type":"text/javascript"}});
 if(url.pathname === "/ui.css") return new Response(env.UI_CSS,{headers:{"content-type":"text/css"}});
 if(url.pathname === "/") return new Response('<html class="connect-dialog-standalone"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/ui.css"><div id="root"></div><script src="/ui.js"></script>',{headers:{"content-type":"text/html"}});
 return await routeConnectApi(request,env,url) ?? await routeManaged(request,env,url) ?? new Response(null,{status:404});
}};
`,resolveDir:fileURLToPath(new URL("..",import.meta.url))},bundle:true,write:false,format:"esm",platform:"browser",target:"es2022",external:["cloudflare:workers","node:*"] });
const broker = await build({stdin:{contents:`
export { UserCredentialBroker } from "./src/broker.ts";
export { GoogleSignInProvider } from "./src/google-sign-in-provider.ts";
export default { fetch(request,env) {
 const match = new URL(request.url).pathname.match(/^\\/users\\/([^/]+)\\/wallet$/);
 if(!match) return new Response(null,{status:404});
 return env.CREDENTIALS.getByName(match[1]).fetch(new Request("https://credentials.internal/v1/wallet",request));
}};
`,resolveDir:fileURLToPath(new URL("../../egress",import.meta.url))},bundle:true,write:false,format:"esm",platform:"node",banner:{js:"import { createRequire } from 'node:module'; const require = createRequire('file:///worker.js');"},target:"es2022",external:["cloudflare:*","node:*"],alias:{"nanocodex-tools/hosted":fileURLToPath(new URL("../../nanocodex-tools/src/hosted/index.ts",import.meta.url)),"node-rsa":fileURLToPath(new URL("../../nanocodex/tools/browser/unsupportedNodeRsa.mjs",import.meta.url))},plugins:[{name:"wasm-module",setup(b){if(process.env.NANOCODEX_WASM_PATH) b.onResolve({filter:/pkg-web\/nanocodex\.js$/},()=>({path:process.env.NANOCODEX_WASM_PATH.replace(/nanocodex_bg\.wasm$/,"nanocodex.js")})); b.onResolve({filter:/^nanocodex\/wasm$/},()=>({path:"./nanocodex_bg.wasm",external:true}));}}] });
const common = {compatibilityDate:"2026-07-29",compatibilityFlags:["nodejs_compat"]};
const mf = new Miniflare({ workers:[
 {name:"edge",script:edge.outputFiles[0].text,modules:true,...common,bindings:{UI_JS:ui.outputFiles.find(f=>f.path.endsWith(".js")).text,UI_CSS:ui.outputFiles.find(f=>f.path.endsWith(".css"))?.text??""},serviceBindings:{NANOCODEX_BACKEND:"managed",NANOCODEX_CONNECT_API:()=>{throw new Error("Sign-in must not route to Connect-scoped OAuth")}}},
 {name:"managed",script:backend.outputFiles[0].text,modules:true,...common,bindings:{
   NANOCODEX_OTP_HMAC_KEY:"synthetic-otp-key-"+"0".repeat(32),
   TWILIO_ACCOUNT_SID:"AC"+"0".repeat(32),TWILIO_AUTH_TOKEN:"synthetic-twilio-secret",TWILIO_VERIFY_SERVICE_SID:"VA"+"0".repeat(32)},
   durableObjects:Object.fromEntries(Object.entries({NANOCODEX_AUTH:"NonceStorage",NANOCODEX_USERS:"UserAccount",NANOCODEX_ORGANIZATIONS:"Organization",NANOCODEX_API_KEYS:"ApiKeyRecord"}).map(([name,className])=>[name,{className,useSQLite:true}])),serviceBindings:{NANOCODEX:"broker",GOOGLE_SIGN_IN:{name:"broker",entrypoint:"GoogleSignInProvider"}},outboundService:request=>{assert.notEqual(request.url,"https://oauth2.googleapis.com/token");return outbound(request)}},
 {name:"broker",...common,modules:[{type:"ESModule",path:"worker.js",contents:broker.outputFiles[0].text},{type:"CompiledWasm",path:"nanocodex_bg.wasm",contents:await readFile(process.env.NANOCODEX_WASM_PATH || new URL("../../nanocodex/pkg-web/nanocodex_bg.wasm",import.meta.url))}],bindings:{ENVIRONMENT:"test",GOOGLE_OAUTH_CLIENT_ID:clientId,GOOGLE_OAUTH_CLIENT_SECRET:"synthetic-client-secret",CREDENTIAL_ENCRYPTION_KEY:Buffer.alloc(32, 7).toString("base64url")},durableObjects:{CREDENTIALS:{className:"UserCredentialBroker",useSQLite:true}},outboundService:outbound},
] });
let browser;
try {
 const origin = (await mf.ready).origin;
 browser = await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
 async function pageFor(width=1100) {
   const context = await browser.newContext({viewport:{width,height:900}});
   let proof; context.on("request",request=>{ if(new URL(request.url()).pathname==="/v1/auth/google/status") proof=request.postDataJSON(); if(new URL(request.url()).pathname==="/v1/connectors/google/callback") trace.push({callback:request.url()}); });
   await context.tracing.start({screenshots:true,snapshots:true});
   context.on("page",async page=>{
     const cdp=await context.newCDPSession(page);
     await cdp.send("Fetch.enable",{patterns:[{urlPattern:"https://accounts.google.com/*"}]});
     cdp.on("Fetch.requestPaused",async event=>{
       try { await cdp.send("Fetch.fulfillRequest",{requestId:event.requestId,responseCode:200,responseHeaders:[{name:"content-type",value:"text/html"}],body:Buffer.from(providerPage(event.request.url)).toString("base64")}); }
       catch(error) { browserErrors.push(String(error)); }
     });
     page.setDefaultTimeout(15_000);
     page.on("pageerror",error=>browserErrors.push(error.message));
     page.on("response",response=>{const url=new URL(response.url()); if(url.origin===origin&&url.pathname.startsWith("/v1/")) trace.push({browser:true,method:response.request().method(),path:url.pathname,status:response.status()});});
   });
   function providerPage(providerURL) {
     const url=new URL(providerURL);
     if(url.pathname!=="/o/oauth2/v2/auth") return "";
     assert.equal(url.searchParams.get("client_id"),clientId);
     assert.equal(url.searchParams.get("response_type"),"code");
     assert.equal(url.searchParams.get("code_challenge_method"),"S256");
     assert.ok(url.searchParams.get("nonce")); assert.ok(url.searchParams.get("state"));
     const code="fixture-"+crypto.randomUUID();
     codes.set(code,{sub:subject,nonce:url.searchParams.get("nonce"),challenge:url.searchParams.get("code_challenge"),redirect:url.searchParams.get("redirect_uri")});
     const callback=new URL(url.searchParams.get("redirect_uri"));
     assert.equal(callback.origin,origin);
     callback.searchParams.set("state",url.searchParams.get("state"));
     const approved=new URL(callback); approved.searchParams.set("code",code);
     const denied=new URL(callback); denied.searchParams.set("error","access_denied");
     return `<h1>Synthetic Google account</h1><a href="${approved.href.replaceAll("&","&amp;")}">Approve fixture</a><a href="${denied.href.replaceAll("&","&amp;")}">Deny fixture</a>`;
   }
   const page=await context.newPage();
   return {page,context,proof:()=>proof};
 }
 async function finish(context,name) {await context.tracing.stop({path:fileURLToPath(new URL(name+".zip",output))});await context.close();}
 async function openGoogle(page,context,button="Sign in with Google") {
   const popupPromise=context.waitForEvent("page");
   await page.getByRole("button",{name:button,exact:true}).click();
   const popup=await popupPromise;
   try { await popup.getByRole("heading",{name:"Synthetic Google account"}).waitFor(); } catch(error) { trace.push({popupURL:popup.url(),popupBody:await popup.locator("body").innerText()}); throw error; }
   return popup;
 }
 async function api(path,body,expected=200) {
   const response=await fetch(origin+path,{method:"POST",headers:{"content-type":"application/json",origin},body:JSON.stringify(body)});
   const text=await response.text(); const value=text?JSON.parse(text):null;
   trace.push({api:true,path,expected,observed:response.status,status:value?.status,error:value?.error});
   assert.equal(response.status,expected,text);return value;
 }
 let canonicalId;
 for(const width of [1100,390]) {
   const {page,context,proof}=await pageFor(width);
   await page.goto(origin);
   await page.getByRole("button",{name:"Sign in with Google",exact:true}).waitFor();
   await page.getByRole("button",{name:"Text me a code",exact:true}).waitFor();
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
   await page.screenshot({path:fileURLToPath(new URL(`chooser-${width}.png`,output)),fullPage:true});
   const popup=await openGoogle(page,context);
   await api("/v1/auth/google/status",{...proof(),code_verifier:"A".repeat(43)},400);
   await popup.getByRole("link",{name:"Approve fixture"}).click();
   await page.getByRole("heading",{name:"Account ready",exact:true}).waitFor();
   await api("/v1/auth/google/complete",proof(),400);
   const id=await page.getByTestId("account-id").textContent();
   assert.match(id,/^[0-9a-f-]{36}$/);
   if(canonicalId) assert.equal(id,canonicalId,"Repeated Google subject resolves to the existing canonical account"); else canonicalId=id;
   const me=await page.evaluate(async()=>(await fetch("/v1/me")).json());
   assert.equal(me.user.id,id); assert.equal(me.user.persistent,true);
   assert.match(me.user.address,/^0x[0-9a-f]{40}$/);
   const cookies=await context.cookies(); const session=cookies.find(c=>c.name==="nanocodex_account");
   assert.ok(session?.httpOnly); assert.match(session.value,/^s_/);
   await page.reload(); await page.getByRole("heading",{name:"Account ready",exact:true}).waitFor();
   assert.equal(await page.getByTestId("account-id").textContent(),id);
   await page.screenshot({path:fileURLToPath(new URL(`authenticated-${width}.png`,output)),fullPage:true});
   await page.getByRole("button",{name:"Sign out",exact:true}).click();
   await page.getByRole("button",{name:"Sign in with Google",exact:true}).waitFor();
   await finish(context,`success-${width}`);
   trace.push({journey:"initial-options-google-session-reload-logout",width,canonicalId:id,result:"passed"});
 }
 for(const outcome of ["denied","closed"]) {
   const {page,context}=await pageFor(390); await page.goto(origin);
   const popup=await openGoogle(page,context);
   if(outcome==="denied") await popup.getByRole("link",{name:"Deny fixture"}).click(); else { await popup.close(); await page.getByRole("button",{name:"Cancel Google sign-in",exact:true}).click(); }
   await page.getByRole("button",{name:"Cancel Google sign-in",exact:true}).waitFor({state:"hidden"});
   await page.getByRole("button",{name:"Sign in with Google",exact:true}).waitFor();
   assert.equal(await page.getByRole("button",{name:"Sign in with Google",exact:true}).isEnabled(),true);
   assert.equal(await page.getByRole("button",{name:"Text me a code",exact:true}).isEnabled(),true);
   await page.screenshot({path:fileURLToPath(new URL(`recovered-${outcome}.png`,output)),fullPage:true});
   if(outcome==="denied") {
     await page.getByRole("textbox",{name:"Mobile number",exact:true}).fill("+12025550100");
     await page.getByRole("button",{name:"Text me a code",exact:true}).click();
     await page.getByRole("textbox",{name:"6-digit code",exact:true}).fill("123456");
     await page.getByRole("button",{name:"Continue",exact:true}).click();
     await page.getByRole("heading",{name:"Account ready",exact:true}).waitFor();
     const phoneId=await page.getByTestId("account-id").textContent();
     assert.notEqual(phoneId,canonicalId,"Distinct SMS identity remains independent of Google");
     // An already-bound Google subject cannot be silently moved onto this phone account.
     const conflict=await openGoogle(page,context,"Link Google account");
     await conflict.getByRole("link",{name:"Approve fixture"}).click();
     await page.getByRole("alert").waitFor();
     assert.equal(await page.getByTestId("account-id").textContent(),phoneId);
     subject="synthetic-google-subject-linked-to-phone";
     const link=await openGoogle(page,context,"Link Google account");
     await link.getByRole("link",{name:"Approve fixture"}).click();
     await page.getByRole("status").filter({hasText:"Google is linked"}).waitFor();
     await page.screenshot({path:fileURLToPath(new URL("phone-google-linked.png",output)),fullPage:true});
     await page.getByRole("button",{name:"Sign out",exact:true}).click();
     const returning=await openGoogle(page,context);
     await returning.getByRole("link",{name:"Approve fixture"}).click();
     await page.getByRole("heading",{name:"Account ready",exact:true}).waitFor();
     assert.equal(await page.getByTestId("account-id").textContent(),phoneId,"Explicitly linked Google returns the original phone account");
     trace.push({journey:"phone-google-link-conflict-and-canonical-return",result:"passed"});
   }
   await finish(context,`recovery-${outcome}`);trace.push({journey:`google-${outcome}-recovery`,smsVerified:outcome==="denied",result:"passed"});
 }
 assert.deepEqual(browserErrors,[]);
 await writeFile(new URL("trace.json",output),JSON.stringify({command,transport:"Chromium -> account proxy -> managed routes -> SQLite DOs and real encrypted wallet broker",fixtures:"External Google authorization/token/JWKS and Twilio delivery/verification only; synthetic identities; no auth bypass",trace},null,2));
 console.log("Google sign-in Chromium journeys passed. Evidence:",fileURLToPath(output));
} catch(error) {
 await writeFile(new URL("trace-failed.json",output),JSON.stringify({command,error:String(error),browserErrors,trace},null,2));throw error;
} finally {await browser?.close();await mf.dispose();}
