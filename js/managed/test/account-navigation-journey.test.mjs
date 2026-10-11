import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { connectActions } from "../../nanocodex/cloud/Decorator.mjs";
import { http } from "../../nanocodex/cloud/Transport.mjs";
import { fetch } from "./support/miniflare-fetch.mjs";

test("public account navigation through managed HTTP and account proxy", { timeout: 90000 }, async () => {
  const trace = [];
  const output = new URL("../../../output/account-workspace/", import.meta.url);
  const bundle = async contents => (await build({ stdin: { contents, resolveDir: fileURLToPath(new URL("..", import.meta.url)) }, bundle: true, write: false, format: "esm", platform: "browser", target: "es2022", external: ["cloudflare:workers", "node:*"], alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" } })).outputFiles[0].text;
  const managed = await bundle(`import {routeAccountNavigation} from './src/account-navigation.ts'; export default {fetch(r) {return routeAccountNavigation(r,new URL(r.url)) ?? new Response(null,{status:404});}}`);
  const account = await bundle(`import {routeManaged} from '../account/worker/managedProxy.ts'; export default {async fetch(r,e) {return await routeManaged(r,e,new URL(r.url)) ?? new Response(null,{status:404});}}`);
  const mf = new Miniflare({ workers: [
    { name: "edge", modules: true, compatibilityDate: "2026-07-29", serviceBindings: { MANAGED: "managed", ACCOUNT: "account" }, script: `export default {fetch(r,e) {const u=new URL(r.url); const proxy=u.pathname.startsWith('/proxy/'); if(proxy) u.pathname=u.pathname.slice(6); const origin=u.searchParams.get('__origin');u.searchParams.delete('__origin');if(origin){const o=new URL(origin);u.protocol=o.protocol;u.host=o.host;}return e[proxy?'ACCOUNT':'MANAGED'].fetch(new Request(u,r));}}` },
    { name: "managed", modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"], script: managed },
    { name: "account", modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"], script: account, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
  ] });
  try {
    const base = await mf.ready;
    const call = async (path, method = "GET") => { trace.push({method,path}); return fetch(new URL(path,base), {method, headers:{origin:"https://attacker.example",forwarded:"host=attacker.example"}}); };
    for (const prefix of ["", "/proxy"]) {
      const r = await call(`${prefix}/v1/account/links`);
      assert.equal(r.status,200); assert.equal(r.headers.get("cache-control"),"no-store");
      assert.deepEqual(await r.json(), {connections:`${base.origin}/connect`,vault:`${base.origin}/connect/vault`,wallet:`${base.origin}/connect/wallet`,access:`${base.origin}/connect/access`});
      for (const query of ["connect=bad","add=bad","extra=1","connect=github&connect=google","add=card&add=login","connect=","add="]) assert.equal((await call(`${prefix}/v1/account/links?${query}`)).status,400,query);
      const post=await call(`${prefix}/v1/account/links`,"POST"); assert.equal(post.status,405); assert.equal(post.headers.get("allow"),"GET");
      const transport=http(base.href,{fetch:(url,init)=>{const target=new URL(url);target.pathname=prefix+target.pathname;return fetch(target,init);}}).setup({appId:"account-links-journey"});
      const client=connectActions()({request: request => {trace.push({method:request.method,path:request.path});return transport.request(request);}});
      for(const connect of ["cloudflare","github","google","slack","x","spotify","soundcloud","link","whatsapp","claude","chatgpt","openai","mcp"]) {
        const links=await client.account.links({connect,add:"login"});
        assert.equal(links.connections,`${base.origin}/connect?connect=${connect}`); assert.equal(links.vault,`${base.origin}/connect/vault?add=login`);
      }
      for(const add of ["api_key","card","address","phone","totp"]) assert.equal((await client.account.links({add})).vault,`${base.origin}/connect/vault?add=${add}`);
    }
    for(const origin of ["https://managed.nanocodex.gakonst.workers.dev","https://nanocodex.gakonst.workers.dev","https://nanocodex-connect-api.gakonst.workers.dev"]) {
      const r=await call(`/v1/account/links?__origin=${encodeURIComponent(origin)}`); assert.equal(r.status,200); assert.equal((await r.json()).connections,"https://nanocodex.gakonst.workers.dev/connect");
    }
    assert.equal((await (await call("/v1/account/links?__origin=https%3A%2F%2Funknown.example")).json()).connections,"https://nanocodex.gakonst.workers.dev/connect");
  } finally {
    await mkdir(output,{recursive:true}); await writeFile(new URL("account-links-requests.json",output),JSON.stringify(trace,null,2)+"\n"); await mf.dispose();
  }
});
