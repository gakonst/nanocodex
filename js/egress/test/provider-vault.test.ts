import { env } from 'cloudflare:workers';
import { SELF, runInDurableObject } from 'cloudflare:test';
import { describe, it, expect } from 'vitest';
import type { EgressEnv } from '../src/egress';
import type { UserCredentialBroker } from '../src/broker';
const call = (owner: string, route: string, body: any) => {
    if (route === 'capture' && body.source?.provider === 'laso') body = {...body,source:{...body.source,
        transport:'defaultMCP:https://mercator.sh/mcp',owner_id:owner,operation_id:body.operation_id,
        job_id:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',node_id:'issue',card_id:body.payload.card.card_id}};
    return SELF.fetch(`https://broker.internal/users/${owner}/credentials/provider-${route}`, {method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
};
const input = (card = 'ready') => ({ operation_id: crypto.randomUUID(), source: { provider: 'laso', schema: 'laso-us-card-v1', request_id: 'synthetic-host-binding' }, payload: { user_id:'synthetic-owner', auth: { id_token: 'token-secret', refresh_token: 'refresh-secret', expires_in: '3600' }, card: { card_id: card } } });
const safe = (value: unknown) => expect(JSON.stringify(value)).not.toMatch(/4111111111111111|"123"|token-secret|refresh-secret|rotated-secret/);
describe('private provider capture HTTP journey', () => {
    it('captures encrypted, saves once across concurrent replay, reads actual balance and isolates owners', async () => {
        const owner = 'provider-journey';
        const data = input();
        const captures = await Promise.all([call(owner, 'capture', data), call(owner, 'capture', data)]);
        const captured = await captures[0]!.json<any>();
        safe(captured);
        expect(captured.status).toBe('captured');
        expect(await captures[1]!.json()).toEqual(captured);
        expect(await (await call(owner, 'capture', data)).json()).toEqual(captured);
        expect((await call(owner, 'capture', { ...data, operation_id: crypto.randomUUID(), source: { ...data.source, request_id: 'different-binding' } })).status).toBe(409);
        const conflict = await call(owner, 'capture', { ...data, payload: { ...data.payload, card: { card_id: 'different' } } });
        expect(conflict.status).toBe(409);
        const args = { capture_id: captured.capture_id, operation_id: crypto.randomUUID(), name: 'Synthetic card' };
        const results = await Promise.all([call(owner, 'store', args), call(owner, 'store', args)]);
        const saved = await results[0]!.json<any>();
        expect(await results[1]!.json()).toEqual(saved);
        safe(saved);
        expect(saved.status).toBe('saved');
        expect(saved.balance).toBe(4.25);
        expect((await call('another-owner', 'store', args)).status).toBe(404);
        const balance = await (await call(owner, 'card', { vault_id: saved.vault_id, operation: 'balance' })).json<any>();
        expect(balance.balance).toBe(4.25);
        expect(balance.freshness).toBe('current');
        safe(balance);
        const refresh = { vault_id: saved.vault_id, operation: 'refresh', operation_id: crypto.randomUUID() };
        expect((await (await call(owner, 'card', refresh)).json<any>()).status).toBe('balance_pending');
        expect((await call(owner, 'card', { ...refresh, operation_id: crypto.randomUUID() })).status).toBe(429);
        const vault = await (await SELF.fetch(`https://broker.internal/users/${owner}/credentials/vault`)).json<any>();
        expect(vault.vault).toHaveLength(1);
        const binding = (env as unknown as EgressEnv).USER_CREDENTIALS;
        await runInDurableObject(binding.get(binding.idFromName(owner)), async (_instance: UserCredentialBroker, state) => { safe(Object.fromEntries(await state.storage.list())); });
    });
    it('persists pending and requires legitimate billing ZIP; rejects redirects', async () => {
        for (const [card, status] of [['pending', 'awaiting_card'], ['no-zip', 'awaiting_billing_address'], ['redirect', undefined]]) {
            const captured = await (await call('provider-' + card, 'capture', input(card))).json<any>();
            const response = await call('provider-' + card, 'store', { capture_id: captured.capture_id, operation_id: crypto.randomUUID() });
            const result = await response.json<any>();
            safe(result);
            if (status)
                expect(result.status).toBe(status);
            else
                expect(response.status).toBe(503);
        }
    });
    it('captures and stores a generic key without exposing plaintext', async () => {
        const owner = 'provider-generic';
        const capture = await (await call(owner, 'capture', { operation_id: crypto.randomUUID(), source: { provider: 'generic', schema: 'api-key-v1', request_id: 'host-key-request' }, payload: { api_key: 'token-secret' } })).json<any>();
        const saved = await (await call(owner, 'store', { capture_id: capture.capture_id, operation_id: crypto.randomUUID() })).json<any>();
        safe(saved);
        expect(saved.status).toBe('saved');
        expect(saved.kind).toBe('api_key');
    });
    it('reopens encrypted pending capture and rotated tokens after actual broker restart', async () => {
        const owner = 'provider-restart';
        const data = input('pending-ready');
        data.payload.auth.expires_in = '1';
        data.payload.auth.refresh_token = 'restart-refresh-secret';
        const captured = await (await call(owner, 'capture', data)).json<any>();
        const args = { capture_id: captured.capture_id, operation_id: crypto.randomUUID() };
        expect(await (await call(owner, 'store', args)).json()).toMatchObject({ status: 'awaiting_card' });
        const binding = (env as unknown as EgressEnv).USER_CREDENTIALS;
        await expect(runInDurableObject(binding.getByName(owner), (_instance, state) => state.abort('synthetic provider restart'))).rejects.toThrow();
        const saved = await (await call(owner, 'store', args)).json<any>();
        safe(saved);
        expect(saved.status).toBe('saved');
        const rotation = input('rotated');
        rotation.payload.auth.expires_in = '1';
        rotation.payload.auth.refresh_token = 'second-refresh-secret';
        const rotated = await (await call(owner, 'capture', rotation)).json<any>();
        expect(await (await call(owner, 'store', { capture_id: rotated.capture_id, operation_id: crypto.randomUUID() })).json()).toMatchObject({ status: 'saved' });
        console.info('PROVIDER_JOURNEY', JSON.stringify({ journey: 'pending-restart-ready-and-flat-auth', saved: true }));
    });
    it('retains fixed failed refresh replay, stale timestamps, and exact-card mismatch', async () => {
        for (const card of ['refresh-failure', 'stale', 'mismatch']) {
            const owner = 'provider-' + card;
            const captured = await (await call(owner, 'capture', input(card))).json<any>();
            const args = { capture_id: captured.capture_id, operation: 'balance' };
            const response = await call(owner, 'card', args);
            const result = await response.json<any>();
            safe(result);
            if (card === 'mismatch') {
                expect(result).toEqual({ error: 'provider_card_mismatch' });
                continue;
            }
            expect(result.balance).toBe(4.25);
            expect(result.observed_at).toBeGreaterThan(0);
            if (card === 'stale') {
                expect(result.freshness).toBe('stale');
                expect(result.provider_updated_at).toBe(1000);
                continue;
            }
            const refresh = { ...args, operation: 'refresh', operation_id: crypto.randomUUID() };
            expect((await call(owner, 'card', refresh)).status).toBe(503);
            expect(await (await call(owner, 'card', refresh)).json()).toEqual({ error: 'provider_refresh_reconciliation_required' });
        }
    });
    it('uses authorized real address and recovers failed save with no provider reread', async () => {
        const owner = 'provider-save-recovery';
        const base = `https://broker.internal/users/${owner}/credentials/vault`;
        // Exercise the real Vault capacity failure rather than mocking its save method.
        const entries = [];
        for (let i = 0; i < 100; i++) {
            const response = await SELF.fetch(base + '/api_key', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ name: 'Synthetic ' + i, api_key: 'synthetic-key-' + i }) });
            expect(response.status).toBe(201);
            entries.push(await response.json<any>());
        }
        const captured = await (await call(owner, 'capture', input('one-read'))).json<any>();
        const args = { capture_id: captured.capture_id, operation_id: crypto.randomUUID() };
        expect(await (await call(owner, 'store', args)).json()).toEqual({ error: 'vault_save_failed' });
        const binding = (env as unknown as EgressEnv).USER_CREDENTIALS;
        const broker = binding.getByName(owner);
        const captureKey = 'provider-capture:' + captured.capture_id;
        const beforeCommit = await runInDurableObject(broker, async (_instance,state) => state.storage.get(captureKey));
        expect((await SELF.fetch(base + '/api_key/' + entries[0].id, { method: 'DELETE' })).status).toBe(204);
        const saved = await (await call(owner, 'store', args)).json<any>();
        safe(saved);
        expect(saved.status).toBe('saved');
        expect(await (await call(owner, 'store', args)).json()).toEqual(saved);
        // Reproduce the durable crash window: Vault committed, provider receipt
        // write absent. Keep the real Vault transaction and restore only capture.
        await runInDurableObject(broker, async (_instance,state) => {
            await state.storage.put(captureKey,beforeCommit);
            await state.storage.delete('provider-vault:'+saved.vault_id);
        });
        await expect(runInDurableObject(broker,(_instance,state)=>state.abort('synthetic post-save restart'))).rejects.toThrow();
        expect(await (await call(owner,'store',args)).json()).toEqual(saved);
        const vault = await (await SELF.fetch(base)).json<any>();
        expect(vault.vault).toHaveLength(100);
        const addressOwner = 'provider-real-address';
        const address = await (await SELF.fetch(`https://broker.internal/users/${addressOwner}/credentials/vault/address`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ name: 'Synthetic billing', address_line_1: '10 Test Street', city: 'New York', state: 'NY', zip: '10002', country: 'US' }) })).json<any>();
        expect(address.id).toBeTruthy();
        const nozip = await (await call(addressOwner, 'capture', input('no-zip'))).json<any>();
        const result = await (await call(addressOwner, 'store', { capture_id: nozip.capture_id, address_vault_id: address.id, operation_id: crypto.randomUUID() })).json<any>();
        safe(result);
        expect(result.status).toBe('saved');
        console.info('PROVIDER_JOURNEY', JSON.stringify({ journey: 'capacity-save-replay-real-address', saved: true }));
    }, 30000);
    it('rejects changed issuer identity and invalid card fields without poisoning recovery', async () => {
        const owner='provider-validation';
        const data={...input('identity-check'),payload:{...input('identity-check').payload,user_id:'synthetic-issuer-wallet'}};
        data.payload.auth.expires_in='1';data.payload.auth.refresh_token='identity-refresh-secret';
        const identity=await (await call(owner,'capture',data)).json<any>();
        const identityArgs={capture_id:identity.capture_id,operation_id:crypto.randomUUID()};
        expect(await (await call(owner,'store',identityArgs)).json()).toEqual({error:'provider_auth_invalid'});
        expect(await (await call(owner,'store',identityArgs)).json()).toEqual({error:'provider_auth_reconciliation_required'});
        const capture=await (await call(owner,'capture',input('invalid-ready'))).json<any>();
        const args={capture_id:capture.capture_id,operation_id:crypto.randomUUID()};
        expect(await (await call(owner,'store',args)).json()).toEqual({error:'provider_card_invalid'});
        const saved=await (await call(owner,'store',args)).json<any>();safe(saved);expect(saved.status).toBe('saved');
    });

    it('retains encrypted host job bindings across restart and rejects concurrent stale commits',async()=>{
        const owner='provider-binding-owner';const key='mercator-private:v2:job:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
        expect(await (await call(owner,'bindings',{operation:'read',key})).json()).toEqual({value:null,version:0});
        const commit={operation:'commit',reads:[{key,version:0}],writes:[{key,value:{job:'synthetic',secret:'token-secret'}}]};
        const responses=await Promise.all([call(owner,'bindings',commit),call(owner,'bindings',commit)]);
        expect(responses.map(response=>response.status).sort()).toEqual([200,409]);
        const binding=(env as unknown as EgressEnv).USER_CREDENTIALS;
        await runInDurableObject(binding.getByName(owner),async(_instance,state)=>{const stored=Object.fromEntries(await state.storage.list());safe(stored);expect(JSON.stringify(stored)).not.toContain('bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb');});
        await expect(runInDurableObject(binding.getByName(owner),(_instance,state)=>state.abort('synthetic binding restart'))).rejects.toThrow();
        expect(await (await call(owner,'bindings',{operation:'read',key})).json()).toEqual({value:commit.writes[0].value,version:1});
        expect(await (await call('another-binding-owner','bindings',{operation:'read',key})).json()).toEqual({value:null,version:0});
        expect((await call(owner,'bindings',{operation:'read',key:'other:key'})).status).toBe(400);
    });

});
