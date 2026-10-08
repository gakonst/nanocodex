import { env, exports } from 'cloudflare:workers';
import { SELF, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it, vi } from 'vitest';
import type { EgressEnv } from '../src/egress';

const worker = env as unknown as EgressEnv;
const privateModel = (exports as unknown as {SessionModelEgress: Fetcher}).SessionModelEgress;
const subject = `managed-session-v1_${'c'.repeat(64)}`;
function control(user: string, suffix = '', method = 'GET', code?: string) {
  return SELF.fetch(`https://broker.internal/users/${user}/credentials${suffix}`, {
    method, ...(code === undefined ? {} : {headers:{'content-type':'application/json'},body:JSON.stringify({code})}),
  });
}
async function begin(user: string) {
  const response = await control(user,'/claude/login','POST'); expect(response.status).toBe(200);
  const result = await response.json<{state:string;authorization_url:string;expires_at:number}>();
  expect(result.state).toBe('pending');
  expect(result.expires_at).toBeGreaterThan(Date.now());
  const url = new URL(result.authorization_url);
  expect(url.origin).toBe('https://claude.com');
  expect(url.pathname).toBe('/cai/oauth/authorize');
  expect(url.hash).toBe('');
  expect(url.searchParams.get('code')).toBe('true');
  expect(url.searchParams.get('client_id')).toBe('9d1c250a-e61b-44d9-88ed-5944d1962f5e');
  expect(url.searchParams.get('response_type')).toBe('code');
  expect(url.searchParams.get('scope')?.split(' ')).toEqual([
    'org:create_api_key','user:profile','user:inference','user:sessions:claude_code','user:mcp_servers','user:file_upload','user:plugins',
  ]);
  expect(/^[A-Za-z0-9_-]{43}$/.test(url.searchParams.get('code_challenge') ?? '')).toBe(true);
  expect(/^[A-Za-z0-9_-]{43}$/.test(url.searchParams.get('state') ?? '')).toBe(true);
  expect(url.searchParams.get('redirect_uri')).toBe('https://platform.claude.com/oauth/code/callback');
  expect(url.searchParams.get('code_challenge_method')).toBe('S256');
  return url.searchParams.get('state')!;
}
async function login(user: string, scenario: string) {
  const state = await begin(user);
  const complete = await control(user,'/claude/login/complete','POST',`${scenario}#${state}`);
  expect(complete.status).toBe(200); expect(await complete.json()).toMatchObject({state:'authenticated'});
}
function messages(user: string, extra: Record<string,string> = {}) {
  return new Request('https://nanocodex.internal/v1/messages',{method:'POST',
    headers:{'content-type':'application/json',authorization:'Bearer NANOCODEX_PROVIDER_CREDENTIAL',
      'x-nanocodex-subject':subject,'x-nanocodex-session-model-owner':user,'x-private':'ignored','user-agent':'claude-cli/2.1.280 (external, cli)',...extra},
    body:JSON.stringify({model:'claude-synthetic-a',max_tokens:64,messages:[{role:'user',content:'Hello'}]})});
}
async function trace(scenario: string) {
  return (await fetch(`https://claude-fixture.invalid/trace?scenario=${scenario}`)).json<Record<string,number>>();
}

describe('real workerd broker and Rust WASM Claude account journeys', () => {
  it('manual login, encrypted persistence, catalog, private Messages and disconnect', async () => {
    const user='claude-managed-lifecycle';
    expect(await (await control(user)).json()).toMatchObject({claude:{connected:false,state:'signed_out'}});
    await login(user,'lifecycle');
    expect(await (await control(user)).json()).toMatchObject({ready:true,claude:{connected:true,state:'authenticated'}});
    const snapshot = await runInDurableObject(worker.USER_CREDENTIALS.getByName(user),async (_instance,state) => {
      const row=await state.storage.get<{revision:string;envelope:unknown}>('claude-subscription-v1');
      return {revision:row?.revision, encrypted:JSON.stringify(row?.envelope)};
    });
    expect(Number(snapshot.revision)).toBeGreaterThan(0);
    for (const value of ['synthetic-claude-lifecycle','synthetic-refresh-lifecycle','authorization_url','code_verifier']) expect(snapshot.encrypted).not.toContain(value);
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(200);
    expect(await catalog.json()).toEqual({models:[{id:'claude-synthetic-a',display_name:'Synthetic A'},{id:'claude-synthetic-b',display_name:'Synthetic B'}],has_more:false});
    const result=await privateModel.fetch(messages(user)); expect(result.status).toBe(200);
    expect(await result.json()).toMatchObject({content:[{text:'Synthetic Claude'}]});
    for (const header of ['authorization','set-cookie','x-api-key']) expect(result.headers.has(header)).toBe(false);
    const deleted=await control(user,'/claude','DELETE'); expect(deleted.status).toBe(200);
    expect(await deleted.json()).toEqual({connected:false,state:'signed_out'});
    expect((await privateModel.fetch(messages(user))).status).toBe(409);
    expect(await trace('lifecycle')).toEqual({exchange:1,profile:1,models:1,messages:1,revoke:1});
    console.info('CLAUDE_JOURNEY', {journey:'login/catalog/messages/disconnect',ciphertext:true,revoked:true,providerCalls:await trace('lifecycle')});
  });
  it('preserves native feature betas and fixed subscription query/headers without caller destination authority',async()=>{
    const user='claude-profile-features'; await login(user,'features');
    const result=await privateModel.fetch(messages(user,{
      'anthropic-beta':'oauth-2025-04-20,context-management-2025-06-27,effort-2025-11-24',
      'x-app':'caller-cannot-override', 'x-claude-code-request-class':'caller-cannot-override',
    }));
    expect(result.status).toBe(200); expect(await result.json()).toMatchObject({content:[{text:'Synthetic Claude'}]});
    expect((await privateModel.fetch(new Request('https://nanocodex.internal/v1/messages?beta=false',{
      method:'POST',headers:messages(user).headers,body:'{}',
    }))).status).toBe(403);
    expect(await trace('features')).toEqual({exchange:1,profile:1,messages:1});
    console.info('CLAUDE_JOURNEY',{journey:'native-subscription-profile',query:'beta=true',featureBetasPreserved:true,oauthBetaDeduplicated:true,ompWireIdentity:true});
  });
  it('keeps unsupported OAuth catalog access unavailable instead of guessing entitlement',async()=>{
    const user='claude-catalog-unsupported'; await login(user,'catalog-unsupported');
    expect(await (await control(user)).json()).toMatchObject({claude:{connected:true}});
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(503);
    expect(await catalog.json()).toEqual({error:'claude_models_unavailable'});
    expect(await trace('catalog-unsupported')).toEqual({exchange:1,profile:1,models:1});
    console.info('CLAUDE_JOURNEY',{journey:'catalog-rollout-gate',unsupportedOAuthCatalog:'unavailable',guessedModels:0});
  });
  it('follows later catalog pages and deduplicates without hiding supported entitlement', async () => {
    const user='claude-catalog-pages'; await login(user,'catalog-pages');
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(200);
    expect(await catalog.json()).toEqual({models:[{id:'claude-synthetic-a',display_name:'Synthetic A'},
      {id:'claude-sonnet-4-6',display_name:'Entitled Sonnet'}],has_more:false});
    expect(await trace('catalog-pages')).toEqual({exchange:1,profile:1,models:2});
  });
  it('accepts bounded rich model metadata exceeding the old64KiB page cap', async () => {
    const user='claude-catalog-rich'; await login(user,'catalog-rich');
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(200);
    const value=await catalog.json<{models:{id:string}[];has_more:boolean}>();
    expect(value.models).toHaveLength(100); expect(value.has_more).toBe(false);
    expect(JSON.stringify(value)).not.toContain('capabilities');
    expect(await trace('catalog-rich')).toEqual({exchange:1,profile:1,models:1});
  });
  it('retries the identical later-page cursor after one explicit401 and retains rotation', async () => {
    const user='claude-catalog-refresh'; await login(user,'catalog-refresh');
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(200);
    expect(await catalog.json()).toMatchObject({models:[{id:'claude-synthetic-a'},{id:'claude-sonnet-4-6'}],has_more:false});
    expect(await trace('catalog-refresh')).toEqual({exchange:1,profile:2,models:3,refresh:1});
    expect((await control(user,'/claude/models')).status).toBe(200);
    expect(await trace('catalog-refresh')).toEqual({exchange:1,profile:2,models:3,refresh:1}); // second listing served from the isolate cache
  });
  it('rechecks earlier-page names against a later rotated private credential', async () => {
    const user='claude-catalog-reflection'; await login(user,'catalog-reflection');
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(200);
    expect(await catalog.json()).toEqual({models:[{id:'claude-synthetic-a',display_name:'claude-synthetic-a'},
      {id:'claude-sonnet-4-6',display_name:'Entitled Sonnet'}],has_more:false});
    expect(await trace('catalog-reflection')).toEqual({exchange:1,profile:2,models:3,refresh:1});
  });
  it('uses one cumulative provider deadline across slow catalog pages', async () => {
    const user='claude-catalog-deadline'; await login(user,'catalog-deadline');
    const started=Date.now(); const catalog=await control(user,'/claude/models');
    expect(catalog.status).toBe(503); expect(await catalog.json()).toEqual({error:'claude_models_unavailable'});
    expect(Date.now()-started).toBeGreaterThanOrEqual(14000); expect(Date.now()-started).toBeLessThan(19500);
    expect(await trace('catalog-deadline')).toEqual({exchange:1,profile:1,models:2});
  }, 25000);
  it('never refreshes twice within one paginated catalog lookup', async () => {
    const user='claude-catalog-repeat-401'; await login(user,'catalog-repeat-401');
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(503);
    expect(await catalog.json()).toEqual({error:'claude_models_unavailable'});
    expect(await trace('catalog-repeat-401')).toEqual({exchange:1,profile:2,models:3,refresh:1});
  });
  it.each([
    ['catalog-later-failure',2], ['catalog-missing-cursor',1], ['catalog-repeat-cursor',2],
    ['catalog-wrong-cursor',1], ['catalog-nonboolean',1], ['catalog-limit',10], ['catalog-large-page',1], ['catalog-large-body',1],
  ])('fails closed for %s instead of advertising an exhausted first page', async (scenario, count) => {
    const user='claude-'+scenario; await login(user,scenario);
    const catalog=await control(user,'/claude/models'); expect(catalog.status).toBe(503);
    expect(await catalog.json()).toEqual({error:'claude_models_unavailable'});
    expect(await trace(scenario)).toEqual({exchange:1,profile:1,models:count});
  });
  it('reopens encrypted grant after actual DO abort and preserves current authorization',async()=>{
    const user='claude-reopen'; await login(user,'reopen');
    await expect(runInDurableObject(worker.USER_CREDENTIALS.getByName(user),(_instance,state)=>{
      state.abort('synthetic Claude broker restart');
    })).rejects.toThrow();
    expect(await (await control(user)).json()).toMatchObject({claude:{connected:true,state:'authenticated'}});
    const result=await privateModel.fetch(messages(user)); expect(result.status).toBe(200);
    expect(await result.json()).toMatchObject({content:[{text:'Synthetic Claude'}]});
    expect(await trace('reopen')).toEqual({exchange:1,profile:1,messages:1});
    console.info('CLAUDE_JOURNEY',{journey:'DO-reopen',persistedAuthorization:true,providerCalls:await trace('reopen')});
  });
  it('public status resumes staged validation after actual DO reopen without repeating token exchange',async()=>{
    const user='claude-validating-reopen'; const state=await begin(user);
    const response=await control(user,'/claude/login/complete','POST',`profile-uncertain#${state}`);
    expect(response.status).toBe(400); expect(await response.json()).toMatchObject({state:'validating',error:'claude_login_failed'});
    await expect(runInDurableObject(worker.USER_CREDENTIALS.getByName(user),(_instance,state)=>{
      state.abort('synthetic staged-validation restart');
    })).rejects.toThrow();
    expect(await (await control(user,'/claude/login/status')).json()).toMatchObject({state:'authenticated'});
    expect(await (await control(user)).json()).toMatchObject({claude:{connected:true,state:'authenticated'}});
    const catalog = await control(user,'/claude/models'); expect(catalog.status).toBe(200);
    const result=await privateModel.fetch(messages(user)); expect(result.status).toBe(200);
    expect(await result.json()).toMatchObject({content:[{text:'Synthetic Claude'}]});
    expect(await trace('profile-uncertain')).toEqual({exchange:1,profile:2,models:1,messages:1});
    console.info('CLAUDE_JOURNEY',{journey:'validation-reopen',oneTokenPost:true,providerCalls:await trace('profile-uncertain')});
  });
  it('public credential metadata makes staged grant connected after actual DO reopen',async()=>{
    const user='claude-metadata-validation-reopen'; const state=await begin(user);
    const response=await control(user,'/claude/login/complete','POST',`profile-metadata-reopen#${state}`);
    expect(response.status).toBe(400); expect(await response.json()).toMatchObject({state:'validating'});
    await expect(runInDurableObject(worker.USER_CREDENTIALS.getByName(user),(_instance,state)=>{
      state.abort('synthetic metadata-validation restart');
    })).rejects.toThrow();
    expect(await (await control(user)).json()).toMatchObject({claude:{connected:true,state:'authenticated'}});
    expect(await (await control(user,'/claude/login/status')).json()).toMatchObject({state:'authenticated'});
    expect(await trace('profile-metadata-reopen')).toEqual({exchange:1,profile:2});
    console.info('CLAUDE_JOURNEY',{journey:'public-metadata-validation-reopen',oneTokenPost:true,connected:true});
  });
  it('public polling tolerates repeated profile GET failure without exposing tokens or retrying exchange',async()=>{
    const user='claude-validation-polling'; const state=await begin(user);
    expect((await control(user,'/claude/login/complete','POST',`profile-polling#${state}`)).status).toBe(400);
    expect(await (await control(user,'/claude/login/status')).json()).toEqual({state:'validating'});
    expect(await trace('profile-polling')).toEqual({exchange:1,profile:2});
    expect(await (await control(user,'/claude/login/status')).json()).toMatchObject({state:'authenticated'});
    expect(await trace('profile-polling')).toEqual({exchange:1,profile:3});
    console.info('CLAUDE_JOURNEY',{journey:'public-validation-polling',oneTokenPost:true,replayableProfileGets:3});
  });
  it('fences replay and cross-account completion; no tokens through generic/tool egress',async()=>{
    const a='claude-replay-a', b='claude-replay-b';
    const state=await begin(a); await begin(b);
    expect((await control(b,'/claude/login/complete','POST',`replay#${state}`)).status).toBe(400);
    expect(await trace('replay')).toEqual({});
    expect((await control(a,'/claude/login/complete','POST',`replay#${state}`)).status).toBe(200);
    expect((await control(a,'/claude/login/complete','POST',`replay#${state}`)).status).toBe(400);
    expect(await trace('replay')).toEqual({exchange:1,profile:1});
    const generic=messages(a); generic.headers.delete('x-nanocodex-session-model-owner');
    expect((await SELF.fetch(generic)).status).toBe(403);
    expect((await privateModel.fetch(messages(b))).status).toBe(409);
    expect((await privateModel.fetch(messages(a,{authorization:'Bearer caller-secret'}))).status).toBe(403);
    expect((await privateModel.fetch(messages(a,{'x-api-key':'caller-secret'}))).status).toBe(403);
    console.info('CLAUDE_JOURNEY',{journey:'replay/cross-account/tool-denial',providerCalls:await trace('replay')});
  });
  it('refreshes once after explicit401, persists rotation, and never retries uncertain Messages POST',async()=>{
    const user='claude-refresh'; await login(user,'refresh');
    expect((await privateModel.fetch(messages(user))).status).toBe(200);
    expect((await privateModel.fetch(messages(user))).status).toBe(200);
    expect(await trace('refresh')).toEqual({exchange:1,profile:2,messages:3,refresh:1});
    const other='claude-message-uncertain'; await login(other,'message-uncertain');
    expect((await privateModel.fetch(messages(other))).status).toBe(503);
    expect(await trace('message-uncertain')).toEqual({exchange:1,profile:1,messages:1});
    console.info('CLAUDE_JOURNEY',{journey:'refresh/uncertain-model',refreshCalls:await trace('refresh'),uncertainCalls:await trace('message-uncertain')});
  });
  it('reports secret-safe failure phases through Messages without retrying uncertain dispatch',async()=>{
    const failureLogs=vi.spyOn(console,'error').mockImplementation(()=>{});
    const records: Record<string,unknown>[]=[];
    try {
      for (const scenario of ['rpc','wire','transport']) {
        const user=`claude-diagnostic-${scenario}`;
        await login(user,`diagnostic-${scenario}`);
        const stub=worker.USER_CREDENTIALS.getByName(user);
        if (scenario==='rpc') await runInDurableObject(stub,instance=>{
          // Fault only the external credential RPC; keep the Worker HTTP
          // boundary, subscription persistence and provider transport real.
          Object.defineProperty(instance,'resolveClaudeCredential',{configurable:true,value:async()=>{
            const error=new Error('synthetic-private-diagnostic');
            error.name='synthetic-private-error-name'; throw error;
          }});
        });
        const originalFetch=globalThis.fetch;
        const transportFault=scenario==='transport'?vi.spyOn(globalThis,'fetch').mockImplementation(async(input,init)=>{
          const response=await originalFetch(input,init);
          const url=typeof input==='string'?input:input instanceof URL?input.href:input.url;
          if(url==='https://api.anthropic.com/v1/messages?beta=true') {
            // The external fixture has accepted and counted this actual POST.
            // Lose its response before the gateway learns its outcome.
            await response.body?.cancel();
            const error=new TypeError('synthetic-private-diagnostic');
            error.name='synthetic-private-error-name'; throw error;
          }
          return response;
        }):undefined;
        let response: Response;
        try {
          response=await privateModel.fetch(messages(user,scenario==='wire'
            ? {'user-agent':'synthetic-private-diagnostic'} : {}));
        } finally {
          transportFault?.mockRestore();
          if (scenario==='rpc') await runInDurableObject(stub,instance=>{
            Reflect.deleteProperty(instance,'resolveClaudeCredential');
          });
        }
        expect(response.status).toBe(502);
        const body=await response.json<{error:string;egress_request_id:string}>();
        expect(body.error).toBe('claude_upstream_unavailable');
        expect(body.egress_request_id).toMatch(/^[0-9a-f-]{36}$/);
        expect(response.headers.get('x-nanocodex-egress-request-id')).toBe(body.egress_request_id);
        expect(response.headers.get('cache-control')).toBe('no-store');
        const record=failureLogs.mock.calls.flat().find(value=>value?.egress_request_id===body.egress_request_id);
        expect(record).toMatchObject({type:'egress.claude.failure',status:502,
          phase:scenario==='rpc'?'credential_resolution':scenario==='wire'?'request_construction':'upstream_dispatch',
          outcome:scenario==='transport'?'unknown':'not_dispatched',
          upstream_attempts:scenario==='transport'?1:0});
        expect(['Error','TypeError','RangeError','SyntaxError','non_error']).toContain(record.error_kind);
        const serialized=JSON.stringify({body,record,headers:[...response.headers]});
        for (const secret of ['synthetic-private-diagnostic','synthetic-private-error-name',
          `synthetic-claude-diagnostic-${scenario}`,user]) expect(serialized).not.toContain(secret);
        expect(await trace(`diagnostic-${scenario}`)).toEqual({exchange:1,profile:1,
          ...(scenario==='transport'?{messages:1}:{})});
        records.push(record);
      }
    } finally { failureLogs.mockRestore(); }
    console.info('CLAUDE_JOURNEY',{journey:'failure-phase/correlation/no-replay',records});
  });
  it('fences uncertain refresh until a new login or disconnect',async()=>{
    const user='claude-refresh-uncertain'; await login(user,'refresh-uncertain');
    expect((await privateModel.fetch(messages(user))).status).toBe(409);
    expect((await privateModel.fetch(messages(user))).status).toBe(409);
    expect(await (await control(user,'/claude/login/status')).json()).toMatchObject({state:'exchange_uncertain'});
    expect(await trace('refresh-uncertain')).toEqual({exchange:1,profile:1,messages:1,refresh:1});
    expect((await control(user,'/claude','DELETE')).status).toBe(200);
    console.info('CLAUDE_JOURNEY',{journey:'uncertain-refresh',fenced:true,providerCalls:await trace('refresh-uncertain')});
  });
  it('fences uncertain OAuth exchange, redirects and oversized replies without POST retry',async()=>{
    for (const scenario of ['uncertain','redirect','oversized']) {
      const user=`claude-${scenario}`; const state=await begin(user);
      const first=await control(user,'/claude/login/complete','POST',`${scenario}#${state}`);
      expect(first.status).toBe(409); expect(await first.json()).toMatchObject({state:'exchange_uncertain',error:'claude_login_failed'});
      expect((await control(user,'/claude/login/complete','POST',`${scenario}#${state}`)).status).toBe(409);
      expect(await trace(scenario)).toEqual({exchange:1});
      await control(user,'/claude','DELETE'); expect(await (await control(user,'/claude/login/status')).json()).toEqual({state:'signed_out'});
    }
    console.info('CLAUDE_JOURNEY',{journey:'uncertain-oauth/redirect/bound',onePostEach:true});
  });
  it('preserves streamed Unicode through byte splits and credential redaction tails',async()=>{
    const user='claude-unicode'; await login(user,'unicode');
    const result=await privateModel.fetch(messages(user)); expect(result.status).toBe(200);
    const expected='data: abc😀'+'x'.repeat('synthetic-claude-unicode'.length-2)+'\n\n';
    expect(await result.text()).toBe(expected);
    console.info('CLAUDE_JOURNEY',{journey:'unicode-stream',roundTrip:true,utf8AndSurrogateBoundaries:true});
  });
  it('prevents token reflection across streamed chunks',async()=>{
    const user='claude-reflection'; await login(user,'reflection');
    const result=await privateModel.fetch(messages(user)); expect(result.status).toBe(200);
    await expect(result.text()).rejects.toThrow('Claude response unavailable');
    expect(await trace('reflection')).toEqual({exchange:1,profile:1,messages:1});
    console.info('CLAUDE_JOURNEY',{journey:'streamed-private-reflection',denied:true});
  });
});
