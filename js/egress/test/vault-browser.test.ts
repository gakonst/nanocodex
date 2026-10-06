import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

const PASSWORD = "fixture-browser-password";
const ORIGIN = "https://login.example.test";

// Exercise the shipped Worker fetch entrypoint and its real encrypted Durable
// Object storage. The browser-only service binding is intentionally privileged;
// model HTTP gateway denial is covered by the separate transport journey.
describe("private browser Vault boundary", () => {
  it("uses an owned login directly, treating a saved origin as a hint while rejecting invalid authority", async () => {
    const owner = "vault-browser-owner";
    const subject = "B".repeat(43);
    const other = "C".repeat(43);
    for (const [id, user] of [[subject, owner], [other, "vault-browser-other"]]) {
      expect((await SELF.fetch(`https://broker.internal/subjects/${id}`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ user_id: user }) })).status).toBe(200);
    }
    const create = (kind: string, body: Record<string, string>) => SELF.fetch(`https://broker.internal/users/${owner}/credentials/vault/${kind}`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
    });
    const created = await create("login", { name: "Fixture login", username: "fixture@example.test", password: PASSWORD });
    expect(created.status).toBe(201);
    const entry = await created.json<{ id: string; browser_origin?: string }>();
    expect(entry.browser_origin).toBeUndefined();
    expect(JSON.stringify(entry)).not.toContain(PASSWORD);
    const resolve = (who = subject, origin: unknown = ORIGIN, id = entry.id) => SELF.fetch("https://browser-vault.internal/v1/login", {
      method: "POST", headers: { "content-type": "application/json", "x-nanocodex-subject": who },
      body: JSON.stringify({ vault_id: id, expected_origin: origin }),
    });
    const assertLogin = async (origin = ORIGIN) => {
      const response = await resolve(subject, origin);
      expect(response.status).toBe(200);
      expect(response.headers.get("cache-control")).toBe("no-store");
      expect(await response.json()).toEqual({ username: "fixture@example.test", password: PASSWORD });
    };

    // Saving a login is sufficient: no second metadata approval request.
    await assertLogin();
    const hint = (origin: string, who = owner) => SELF.fetch(`https://broker.internal/users/${who}/credentials/vault/login/${entry.id}/origin`, {
      method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ browser_origin: origin }),
    });
    expect((await hint(ORIGIN, "vault-browser-other")).status).toBe(404);
    const savedHint = await hint("https://old-login.example.test");
    expect(savedHint.status).toBe(200);
    const metadata = await savedHint.json();
    expect(metadata).toMatchObject({ id: entry.id, browser_origin: "https://old-login.example.test" });
    expect(JSON.stringify(metadata)).not.toContain(PASSWORD);
    // The explicit browser destination may differ by host or port from the hint.
    await assertLogin();
    await assertLogin("https://login.example.test:444");
    await assertLogin("https://another-login.example.test");

    expect((await resolve(other)).status).toBe(403);
    expect((await resolve("D".repeat(43))).status).toBe(403);
    const apiKey = await create("api_key", { name: "Fixture API", api_key: "fixture-api-secret" });
    expect(apiKey.status).toBe(201);
    const apiEntry = await apiKey.json<{ id: string }>();
    const wrongKind = await resolve(subject, ORIGIN, apiEntry.id);
    expect(wrongKind.status).toBe(403);
    expect(await wrongKind.text()).not.toContain("fixture-api-secret");
    for (const origin of ["http://login.example.test", `${ORIGIN}/`, `${ORIGIN}/path`, "https://person:secret@login.example.test", `${ORIGIN}?x=y`, `${ORIGIN}#fragment`, "not-an-origin", "", null]) {
      const denied = await resolve(subject, origin);
      expect(denied.status, String(origin)).toBe(400);
      expect(await denied.text()).not.toContain(PASSWORD);
    }
    expect((await SELF.fetch("https://browser-vault.internal/v1/login", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ vault_id: entry.id, expected_origin: ORIGIN }) })).status).toBe(403);
    for (const [kind, id] of [["login", entry.id], ["api_key", apiEntry.id]]) {
      expect((await SELF.fetch(`https://broker.internal/users/${owner}/credentials/vault/${kind}/${id}`, { method: "DELETE" })).status).toBe(204);
    }
    expect((await resolve()).status).toBe(403);
  });
});

const allKinds = [
  {kind:'login',payload:{name:'Private login',username:'private@example.test',password:'synthetic-password'},fields:['username','password']},
  {kind:'api_key',payload:{name:'Private API',api_key:'synthetic-api-key'},fields:['api_key']},
  {kind:'card',payload:{name:'Private card',card_number:'4111111111111111',expiry_month:'09',expiry_year:'2031',billing_zip:'10001'},fields:['card_number','card_expiry']},
  {kind:'address',payload:{name:'Private address',address_line_1:'1 Private Way',city:'Athens',state:'Attica',zip:'10558',country:'GR'},fields:['address_line_1','address_line_2','city','state','zip','country']},
  {kind:'phone',payload:{name:'Private phone',phone_number:'+306900000000'},fields:['phone_number']},
] as const;

describe('all-kind private browser materialization',()=>{
  it('saves once durably, materializes owned fields, and rejects conflicts, deletion, and wrong ownership',async()=>{
    const subject='E'.repeat(43),other='F'.repeat(43),owner='all-kind-vault-owner';
    for(const [id,user] of [[subject,owner],[other,'all-kind-other']]) expect((await SELF.fetch(`https://broker.internal/subjects/${id}`,{method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({user_id:user})})).status).toBe(200);
    const call=(path:string,body:unknown,who=subject)=>SELF.fetch(`https://browser-vault.internal/v1/${path}`,{method:'POST',headers:{'content-type':'application/json','x-nanocodex-subject':who},body:JSON.stringify(body)});
    for(const item of allKinds){
      const operation_id=crypto.randomUUID();
      const save={kind:item.kind,payload:item.payload,operation_id};
      const results=await Promise.all([call('save',save),call('save',save)]);
      expect(results.map(r=>r.status)).toEqual([201,201]);
      const metadata=await results[0]!.json<{id:string}>();
      expect(await results[1]!.json()).toEqual(metadata);
      const request={vault_id:metadata.id,expected_origin:ORIGIN,fields:item.fields};
      const resolved=await call('fields',request);
      expect(resolved.status).toBe(200);expect(resolved.headers.get('cache-control')).toBe('no-store');
      const material=await resolved.json<{kind:string;values:Record<string,string>}>();
      expect(material.kind).toBe(item.kind);
      for(const field of item.fields) expect(material.values[field]).toBe(field==='card_expiry'?'09/2031':field==='address_line_2'?'':(item.payload as Record<string,string>)[field]);
      for(const body of [{...request,fields:[]},{...request,fields:['bogus']},{...request,expected_origin:'http://invalid.test'}])expect((await call('fields',body)).status).toBe(400);
      expect((await call('fields',request,other)).status).toBe(403);
      expect((await call('fields',{...request,fields:[item.kind==='login'?'api_key':'password']})).status).toBe(403);
      if(item.kind==='card')expect((await call('fields',{...request,fields:['cvv']})).status).toBe(403);
      expect((await call('save',{...save,payload:{...item.payload,name:'Changed'}})).status).toBe(409);
      expect((await SELF.fetch(`https://broker.internal/users/${owner}/credentials/vault/${item.kind}/${metadata.id}`,{method:'DELETE'})).status).toBe(204);
      expect((await call('fields',request)).status).toBe(403);
      expect((await call('save',save)).status).toBe(410);
    }
  });
});
