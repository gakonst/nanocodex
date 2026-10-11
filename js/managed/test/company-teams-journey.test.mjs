import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { fetch } from "./support/miniflare-fetch.mjs";

const source = `
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, routeAccountRequest } from "./src/account-auth.ts";
import { routeManaged } from "../account/worker/managedProxy.ts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export default { async fetch(request, env) {
  const url = new URL(request.url);
  if (env.EDGE) return await routeManaged(request, env, url) ?? new Response(null,{status:404});
  return await routeAccountRequest(request, env, url) ?? new Response(null,{status:404});
}};
`;

test("company teams over public HTTP: private accounts, invitations, roles, hierarchy, revocation and restart", { timeout: 120_000 }, async () => {
  const trace = [];
  const output = new URL("../../../output/company-teams/", import.meta.url);
  const persistence = fileURLToPath(new URL("store-" + crypto.randomUUID(), output));
  const bundled = await build({
    stdin: { contents: source, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser",
    external: ["cloudflare:workers", "node:*"],
    alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
  });
  const options = {
    durableObjectsPersist: persistence + "/sqlite",
    workers: [
      { name:"edge", script:bundled.outputFiles[0].text, modules:true, compatibilityDate:"2026-07-29", compatibilityFlags:["nodejs_compat"],
        bindings:{EDGE:true}, serviceBindings:{NANOCODEX_BACKEND:"managed"} },
      { name:"managed", script:bundled.outputFiles[0].text, modules:true, compatibilityDate:"2026-07-29", compatibilityFlags:["nodejs_compat"],
        bindings:{ ENVIRONMENT:"development", NANOCODEX_MOCK_TWILIO_VERIFY_CODE:"654321", NANOCODEX_OTP_HMAC_KEY:"synthetic-company-teams-otp-secret-key" },
        serviceBindings:{NANOCODEX:async () => Response.json({address:"0x"+"1".repeat(40),created_at:1})},
        durableObjects:{
          NANOCODEX_AUTH:{className:"NonceStorage",useSQLite:true}, NANOCODEX_USERS:{className:"UserAccount",useSQLite:true},
          NANOCODEX_ORGANIZATIONS:{className:"Organization",useSQLite:true}, NANOCODEX_API_KEYS:{className:"ApiKeyRecord",useSQLite:true},
        } },
    ],
  };
  let mf = new Miniflare(options), base;
  async function http(path, { method="GET", cookie, body, origin="same", expected=200, token }={}) {
    const response = await fetch(new URL(path, base), {method, headers:{
      ...(cookie ? {cookie}:{}), ...(token ? {authorization:"Bearer "+token}:{}),
      ...(body !== undefined ? {"content-type":"application/json"}:{}),
      ...(origin ? {origin:origin === "same" ? new URL(base).origin : origin}:{}),
    }, ...(body !== undefined ? {body:JSON.stringify(body)}:{})});
    const value = await response.json();
    trace.push({path,method,expected,observed:response.status,result:JSON.parse(JSON.stringify(value, (k,v) => ["token","api_key","challenge_id"].includes(k) ? "redacted" : v))});
    assert.equal(response.status,expected,JSON.stringify(value));
    return {value,headers:response.headers};
  }
  async function login(phone) {
    const {value} = await http("/v1/auth/sms/start",{method:"POST",body:{phone},expected:202});
    const result = await http("/v1/auth/sms/verify",{method:"POST",body:{phone,challenge_id:value.challenge_id,code:"654321"}});
    return result.headers.get("set-cookie").split(";")[0];
  }
  try {
    base = await mf.ready;
    const alice = await login("+12025550171"), bob = await login("+12025550172"), eve = await login("+12025550173");
    const aliceBefore = (await http("/v1/me",{cookie:alice})).value;
    const bobBefore = (await http("/v1/me",{cookie:bob})).value;
    const bobId = bobBefore.user.id;
    await http("/v1/teams",{expected:401});
    await http("/v1/teams",{method:"POST",cookie:alice,body:{name:"Example Company"},origin:"https://foreign.example",expected:403});
    const team = (await http("/v1/teams",{method:"POST",cookie:alice,body:{name:"Example Company"},expected:201})).value;
    const path = `/v1/teams/${team.id}`;
    assert.notEqual(team.id,aliceBefore.organization.id);
    await http(path,{cookie:bob,expected:404});
    const invitation = (await http(path+"/invitations",{method:"POST",cookie:alice,body:{role:"writer",user_id:bobId},expected:201})).value;
    await http(path+"/invitations/accept",{method:"POST",cookie:eve,body:{token:invitation.token},expected:404});
    await http(path+"/invitations/accept",{method:"POST",cookie:bob,body:{token:invitation.token}});
    await http(path+"/invitations/accept",{method:"POST",cookie:bob,body:{token:invitation.token}});
    assert.equal((await http("/v1/teams",{cookie:bob})).value.teams[0].role,"writer");
    await http(path+"/invitations",{method:"POST",cookie:bob,body:{role:"reader"},expected:403});
    await http(path+"/members/"+aliceBefore.user.id,{method:"DELETE",cookie:alice,expected:409});
    await http(path+"/members/"+bobId,{method:"PATCH",cookie:alice,body:{role:"reader"}});
    assert.equal((await http(path,{cookie:bob})).value.role,"reader");
    const ownerDetails = (await http(path,{cookie:alice})).value;
    assert.equal(ownerDetails.invitations[0].id,invitation.id);
    assert.equal(ownerDetails.invitations[0].accepted_by,bobId);
    assert.ok(ownerDetails.invitations.every(i => !("digest" in i) && !("token" in i)));
    assert.equal((await http(path,{cookie:bob})).value.invitations,undefined);
    await http("/v1/teams",{method:"POST",cookie:bob,body:{name:"Forbidden child",company_id:team.id},expected:403});
    await http("/v1/teams",{method:"POST",cookie:eve,body:{name:"Forbidden child",company_id:team.id},expected:404});
    await http("/v1/teams",{method:"POST",cookie:alice,body:{name:"Invalid parent",company_id:"invalid"},expected:400});
    const engineering = (await http("/v1/teams",{method:"POST",cookie:alice,body:{name:"Engineering",company_id:team.id},expected:201})).value;
    const finance = (await http("/v1/teams",{method:"POST",cookie:alice,body:{name:"Finance",company_id:team.id},expected:201})).value;
    const childPath = `/v1/teams/${engineering.id}`;
    assert.equal(engineering.company_id,team.id);
    assert.equal(finance.company_id,team.id);
    await http("/v1/teams",{method:"POST",cookie:alice,body:{name:"Too deep",company_id:engineering.id},expected:400});
    await http(childPath,{cookie:bob,expected:404});
    const childInvite = (await http(childPath+"/invitations",{method:"POST",cookie:alice,body:{role:"writer"},expected:201})).value;
    await http(childPath+"/invitations/accept",{method:"POST",cookie:eve,body:{token:childInvite.token},expected:404});
    await http(childPath+"/invitations/accept",{method:"POST",cookie:bob,body:{token:childInvite.token}});
    assert.equal((await http(childPath,{cookie:bob})).value.role,"writer");
    assert.equal((await http(path,{cookie:bob})).value.role,"reader");
    await http(`/v1/teams/${finance.id}`,{cookie:bob,expected:404});
    const listedChild = (await http("/v1/teams",{cookie:bob})).value.teams.find(t => t.id === engineering.id);
    assert.equal(listedChild.company_id,team.id);
    const revoked = (await http(path+"/invitations",{method:"POST",cookie:alice,body:{role:"reader"},expected:201})).value;
    await http(path+"/invitations/"+revoked.id,{method:"DELETE",cookie:alice});
    await http(path+"/invitations/accept",{method:"POST",cookie:eve,body:{token:revoked.token},expected:404});
    const second = (await http("/v1/teams",{method:"POST",cookie:bob,body:{name:"Second Company"},expected:201})).value;
    assert.equal((await http("/v1/teams",{cookie:bob})).value.teams.length,3);
    assert.deepEqual((await http("/v1/me",{cookie:alice})).value.organization,aliceBefore.organization);
    assert.deepEqual((await http("/v1/me",{cookie:bob})).value.organization,bobBefore.organization);
    assert.deepEqual((await http("/v1/me",{cookie:bob})).value.team,bobBefore.team);
    const aliceKey = (await http("/v1/api-keys",{method:"POST",cookie:alice,body:{label:"company-test"},expected:201})).value.api_key;
    const bobKey = (await http("/v1/api-keys",{method:"POST",cookie:bob,body:{label:"company-test"},expected:201})).value.api_key;
    assert.equal((await http(path,{token:bobKey,origin:undefined})).value.role,"reader");
    await http(path+"/invitations",{method:"POST",token:bobKey,body:{role:"reader"},expected:403});
    await http(path+"/invitations",{method:"POST",token:aliceKey,body:{role:"reader"},expected:201});
    await http(path+"/members/"+bobId,{method:"DELETE",cookie:alice});
    await http(path,{cookie:bob,expected:404});
    await http(path,{token:bobKey,expected:404});
    await http(childPath,{cookie:bob,expected:404});
    await http(childPath,{token:bobKey,expected:404});
    await http(childPath+"/invitations/accept",{method:"POST",cookie:bob,body:{token:childInvite.token},expected:404});
    await http(path+"/invitations/accept",{method:"POST",cookie:bob,body:{token:invitation.token},expected:403});
    await mf.dispose(); mf = new Miniflare(options); base = await mf.ready;
    assert.equal((await http("/v1/teams",{cookie:bob})).value.teams[0].id,second.id);
    assert.equal((await http("/v1/teams",{cookie:bob})).value.teams.length,1);
    await http(path,{cookie:bob,expected:404});
    assert.equal((await http(path,{cookie:alice})).value.members.length,1);
    assert.equal((await http(childPath,{cookie:alice})).value.company_id,team.id);
    const rejoin = (await http(path+"/invitations",{method:"POST",cookie:alice,body:{role:"reader",user_id:bobId},expected:201})).value;
    await http(path+"/invitations/accept",{method:"POST",cookie:bob,body:{token:rejoin.token}});
    await http(childPath,{cookie:bob,expected:404});
    assert.ok(!(await http("/v1/teams",{cookie:bob})).value.teams.some(t => t.id === engineering.id));
    await http(childPath+"/invitations/accept",{method:"POST",cookie:bob,body:{token:childInvite.token},expected:403});
    const childRejoin = (await http(childPath+"/invitations",{method:"POST",cookie:alice,body:{role:"reader",user_id:bobId},expected:201})).value;
    await http(childPath+"/invitations/accept",{method:"POST",cookie:bob,body:{token:childRejoin.token}});
    assert.equal((await http(childPath,{cookie:bob})).value.role,"reader");
  } finally {
    await mkdir(output,{recursive:true});
    await writeFile(new URL("trace.json",output),JSON.stringify(trace,null,2));
    await mf.dispose(); await rm(persistence,{recursive:true,force:true});
  }
});
