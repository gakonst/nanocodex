import { CredentialVault, type EncryptedEnvelope } from './credential-vault';
// Only the authenticated host service can reach these routes. Provider payloads
// never leave this module; all outward failures are a finite code. The broker
// invokes this under its owner-wide #exclusive queue, including network awaits.
// Do not call concurrently outside that queue: token rotation and capture/store
// read-modify-write operations rely on this serialization.
type RecordValue = Record<string, any>;
type Capture = {
    id: string;
    operation: string;
    fingerprint: string;
    source: RecordValue;
    payload: RecordValue;
    created: number;
    saveOperation: string;
    vaultId?: string;
    last4?: string;
    authPending?: boolean;
    name?: string;
    addressId?: string;
    entry?: RecordValue;
    balance?: number;
    observed?: number;
    updated?: number | undefined;
    refreshes: number[];
};
export type ProviderVaultDependencies = {
    storage: DurableObjectStorage;
    vault: CredentialVault;
    validEntry(value: unknown, kind: "card" | "api_key"): boolean;
    dispatch(request: Request): Promise<Response>;
};
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const opaque = (v: unknown): v is string => typeof v === 'string' && /^[A-Za-z0-9_-]{22,64}$/.test(v);
const record = (v: unknown): v is RecordValue => !!v && typeof v === 'object' && !Array.isArray(v);
const text = (v: unknown, max = 8192): v is string => typeof v === 'string' && v.length > 0 && v.length <= max && !/[\u0000-\u001f\u007f]/.test(v);
const fail = (code: string, status = 400) => Response.json({ error: code }, { status, headers: { 'cache-control': 'no-store' } });
const reply = (c: Capture, status: string) => Response.json({ capture_id: c.id, provider: c.source.provider, kind: c.source.schema === 'api-key-v1' ? 'api_key' : 'card', status, ...(c.vaultId ? { vault_id: c.vaultId, ...(c.last4 ? { last4: c.last4 } : {}) } : {}), ...(c.balance === undefined ? {} : { balance: c.balance, currency: 'USD', observed_at: c.observed, provider_updated_at: c.updated ?? null, freshness: c.updated === undefined ? 'unknown' : c.updated > Date.now() + 60000 ? 'unknown' : Date.now() - c.updated > 300000 ? 'stale' : 'current' }) }, { headers: { 'cache-control': 'no-store' } });
async function boundedJSON(response: Response): Promise<RecordValue> {
    if (!response.ok || !response.body)
        throw new Error('provider_unavailable');
    const reader = response.body.getReader();
    let size = 0;
    const chunks: Uint8Array[] = [];
    try {
        for (;;) {
            const { done, value } = await reader.read();
            if (done)
                break;
            size += value.length;
            if (size > 65536)
                throw new Error('provider_invalid');
            chunks.push(value);
        }
    }
    finally {
        await reader.cancel().catch(() => { });
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) {
        bytes.set(chunk, offset);
        offset += chunk.length;
    }
    const data: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (!record(data))
        throw new Error('provider_invalid');
    return data;
}
function validAuth(auth: unknown): auth is RecordValue {
    return record(auth) && text(auth.id_token) && text(auth.refresh_token) && Number.isFinite(Number(auth.expires_in)) && Number(auth.expires_in) > 0 && Number(auth.expires_in) <= 31536000;
}
export async function providerVaultRoute(request: Request, deps: ProviderVaultDependencies): Promise<Response> {
    if (request.method !== 'POST')
        return fail('method_not_allowed', 405);
    try {
        const body = await boundedJSON(new Response(request.body));
        const path = new URL(request.url).pathname;
        const get = async (id: string): Promise<Capture | undefined> => { const row = await deps.storage.get<EncryptedEnvelope>('provider-capture:' + id); return row ? (await deps.vault.open<Capture>(row)).value : undefined; };
        const put = async (c: Capture) => deps.storage.put('provider-capture:' + c.id, await deps.vault.seal(c));
        // Host-only custody for managed job bindings, shared across conversations.
        // Values are encrypted; callers use CAS to retain their dispatch fence.
        if (path === '/v1/provider-bindings') {
            const validKey = (key: unknown): key is string => text(key,1024) && key.startsWith('mercator-private:v2:');
            const storageKey = async (key: string) => 'provider-binding:' + Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(key)))).map(byte=>byte.toString(16).padStart(2,'0')).join('');
            const read = async (key: string) => {
                const sealed = await deps.storage.get<EncryptedEnvelope>(await storageKey(key));
                return sealed ? (await deps.vault.open<{value:unknown;version:number}>(sealed)).value : {value:null,version:0};
            };
            if (body.operation === 'read' && validKey(body.key)) {
                return Response.json(await read(body.key),{headers:{'cache-control':'no-store'}});
            }
            if (body.operation !== 'commit' || !Array.isArray(body.reads) || !Array.isArray(body.writes)
                || body.reads.length > 32 || body.writes.length > 32) return fail('invalid_binding');
            const versions = new Map<string,number>();
            for (const item of body.reads) {
                if (!record(item) || !validKey(item.key) || !Number.isSafeInteger(item.version) || item.version < 0 || versions.has(item.key)) return fail('invalid_binding');
                versions.set(item.key,item.version);
                if ((await read(item.key)).version !== item.version) return fail('provider_binding_conflict',409);
            }
            const writes = new Map<string,EncryptedEnvelope>();
            for (const item of body.writes) {
                if (!record(item) || !validKey(item.key) || !versions.has(item.key) || writes.has(item.key) || !Object.hasOwn(item,'value')) return fail('invalid_binding');
                writes.set(item.key,await deps.vault.seal({value:item.value,version:versions.get(item.key)!+1}));
            }
            const sealedWrites = await Promise.all(Array.from(writes,async ([key,value]) => [await storageKey(key),value] as const));
            await deps.storage.transaction(async txn => {
                for (const [key,value] of sealedWrites) await txn.put(key,value);
            });
            return Response.json({committed:true},{headers:{'cache-control':'no-store'}});
        }
        if (path === '/v1/provider-capture') {
            if (!UUID.test(body.operation_id) || !record(body.source) || !text(body.source.request_id, 256) || !text(body.source.provider, 64) || !record(body.payload))
                return fail('invalid_capture');
            if (!['laso', 'generic'].includes(body.source.provider))
                return fail('unsupported_provider');
            if (body.source.schema !== 'laso-us-card-v1' && body.source.schema !== 'api-key-v1')
                return fail('unsupported_capture_schema');
            if (body.source.schema === 'laso-us-card-v1') {
                const fields = ['provider','schema','request_id','transport','owner_id','operation_id','job_id','node_id','card_id'];
                if (Object.keys(body.source).length !== fields.length || fields.some(key => !text(body.source[key],512))
                    || body.source.provider !== 'laso' || body.source.transport !== 'defaultMCP:https://mercator.sh/mcp'
                    || body.source.owner_id !== request.headers.get('x-nanocodex-provider-owner')
                    || body.source.operation_id !== body.operation_id || !UUID.test(body.source.job_id)
                    || !validAuth(body.payload.auth) || !text(body.payload.user_id,256)
                    || !record(body.payload.card) || !text(body.payload.card.card_id,256)
                    || body.source.card_id !== body.payload.card.card_id) return fail('invalid_capture');
            }
            if (body.payload.user_id !== undefined && !text(body.payload.user_id, 256))
                return fail('invalid_capture');
            if (body.source.schema === 'api-key-v1' && !text(body.payload.api_key))
                return fail('invalid_capture');
            const fingerprint = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(JSON.stringify({ source: body.source, payload: body.payload }))))).map(x => x.toString(16).padStart(2, '0')).join('');
            const key = 'provider-operation:' + body.operation_id.toLowerCase();
            const previous = await deps.storage.get<{
                id: string;
                fingerprint: string;
            }>(key);
            if (previous) {
                const c = await get(previous.id);
                return c && previous.fingerprint === fingerprint ? reply(c, c.vaultId ? 'saved' : 'captured') : fail('capture_operation_conflict', 409);
            }
            const identityKey = body.source.schema === 'laso-us-card-v1' ? 'provider-card-identity:' + Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(body.payload.card.card_id)))).map(x => x.toString(16).padStart(2, '0')).join('') : undefined;
            const existingId = identityKey ? await deps.storage.get<string>(identityKey) : undefined;
            if (existingId) {
                const existing = await get(existingId);
                if (!existing)
                    return fail('capture_unavailable', 503);
                if (existing.fingerprint !== fingerprint)
                    return fail('capture_operation_conflict', 409);
                await deps.storage.put(key, { id: existingId, fingerprint });
                return reply(existing, existing.vaultId ? 'saved' : 'captured');
            }
            const c: Capture = { id: crypto.randomUUID().replaceAll('-', ''), operation: body.operation_id.toLowerCase(), fingerprint, source: body.source, payload: body.source.schema === 'api-key-v1' ? { api_key: body.payload.api_key } : { user_id: body.payload.user_id, auth: body.payload.auth, card: { card_id: body.payload.card.card_id }, expires_at: Date.now() + Number(body.payload.auth.expires_in) * 1000 }, created: Date.now(), saveOperation: crypto.randomUUID(), refreshes: [] };
            const sealed = await deps.vault.seal(c);
            await deps.storage.transaction(async (txn) => { await txn.put('provider-capture:' + c.id, sealed); await txn.put(key, { id: c.id, fingerprint }); if (identityKey)
                await txn.put(identityKey, c.id); });
            return reply(c, 'captured');
        }
        let id = body.capture_id;
        if (body.vault_id !== undefined) {
            if (!opaque(body.vault_id) || id !== undefined)
                return fail('invalid_reference');
            id = await deps.storage.get<string>('provider-vault:' + body.vault_id);
        }
        if (!opaque(id))
            return fail('capture_not_found', 404);
        const c = await get(id);
        if (!c)
            return fail('capture_not_found', 404);
        if (!c.vaultId && Date.now() - c.created > 30 * 86400000)
            return fail('capture_expired', 410);
        if (c.vaultId) {
            const entry = await deps.dispatch(new Request('https://credentials.internal/v1/vault-entry/' + c.vaultId, { method: 'POST' }));
            if (!entry.ok)
                return fail('vault_entry_deleted', 410);
            await entry.body?.cancel();
        }
        if (path === '/v1/provider-store') {
            if (!UUID.test(body.operation_id) || (body.name !== undefined && !text(body.name, 120)) || (body.address_vault_id !== undefined && !opaque(body.address_vault_id)))
                return fail('invalid_store');
            const operationKey = 'provider-store-operation:' + body.operation_id.toLowerCase();
            const previous = await deps.storage.get<string>(operationKey);
            if (previous && previous !== c.id)
                return fail('store_operation_conflict', 409);
            if (c.name !== undefined && ((body.name !== undefined && c.name !== body.name) || (body.address_vault_id !== undefined && c.addressId !== undefined && c.addressId !== body.address_vault_id)))
                return fail('store_operation_conflict', 409);
            if (c.vaultId)
                return reply(c, 'saved');
            c.name ??= body.name ?? (c.source.schema === 'api-key-v1' ? 'Provider API key' : 'Laso card');
            c.addressId ??= body.address_vault_id;
            await put(c);
            await deps.storage.put(operationKey, c.id);
        }
        else if (path !== '/v1/provider-card' || !['status', 'balance', 'refresh'].includes(body.operation) || c.source.schema !== 'laso-us-card-v1')
            return fail('invalid_operation');
        if (c.source.schema === 'api-key-v1')
            c.entry ??= { name: c.name, api_key: c.payload.api_key };
        else if (!(path === '/v1/provider-store' && c.entry && !c.vaultId)) {
            if (body.operation === 'refresh' && !UUID.test(body.operation_id))
                return fail('invalid_operation_id');
            if (c.payload.expires_at <= Date.now() + 30000) {
                if (c.authPending)
                    return fail('provider_auth_reconciliation_required', 409);
                c.authPending = true;
                await put(c);
                const auth = await boundedJSON(await fetch('https://laso.finance/auth', { method: 'POST', redirect: 'manual', signal: AbortSignal.timeout(15000), headers: { 'content-type': 'application/json' }, body: JSON.stringify({ grant_type: 'refresh_token', refresh_token: c.payload.auth.refresh_token }) }));
                if (auth.user_id !== c.payload.user_id)
                    return fail('provider_auth_invalid', 502);
                if (!validAuth(auth))
                    return fail('provider_auth_invalid', 502);
                c.payload.auth = { id_token: auth.id_token, refresh_token: auth.refresh_token, expires_in: auth.expires_in };
                c.payload.expires_at = Date.now() + Number(auth.expires_in) * 1000;
                c.authPending = false;
                await put(c);
            }
            const headers = { authorization: 'Bearer ' + c.payload.auth.id_token };
            if (body.operation === 'refresh') {
                if (!UUID.test(body.operation_id))
                    return fail('invalid_operation_id');
                const key = 'provider-refresh:' + body.operation_id.toLowerCase();
                const prior = await deps.storage.get<{
                    id: string;
                    state: string;
                }>(key);
                if (prior)
                    return prior.id !== c.id ? fail('refresh_operation_conflict', 409) : prior.state === 'accepted' ? reply(c, 'balance_pending') : fail('provider_refresh_reconciliation_required', 409);
                c.refreshes = c.refreshes.filter(t => t > Date.now() - 86400000);
                if (c.refreshes.length >= 12 || c.refreshes.some(t => t > Date.now() - 300000))
                    return fail('refresh_rate_limited', 429);
                c.refreshes.push(Date.now());
                const envelope = await deps.vault.seal(c);
                await deps.storage.transaction(async (txn) => { await txn.put('provider-capture:' + c.id, envelope); await txn.put(key, { id: c.id, state: 'pending' }); });
                const response = await fetch('https://laso.finance/refresh-card-data', { method: 'POST', redirect: 'manual', signal: AbortSignal.timeout(15000), headers: { ...headers, 'content-type': 'application/json', 'Idempotency-Key': body.operation_id }, body: JSON.stringify({ card_id: c.payload.card.card_id, card_type: 'Non-Reloadable U.S.' }) });
                const result = await boundedJSON(response);
                if (result.success !== true && !(result.success === undefined && text(result.message,8192)))
                    return fail('provider_refresh_unavailable', 502);
                await deps.storage.put(key, { id: c.id, state: 'accepted' });
                return reply(c, 'balance_pending');
            }
            const data = await boundedJSON(await fetch('https://laso.finance/get-card-data?card_id=' + encodeURIComponent(c.payload.card.card_id), { redirect: 'manual', signal: AbortSignal.timeout(15000), headers }));
            if (data.card_id !== c.payload.card.card_id)
                return fail('provider_card_mismatch', 502);
            if (data.details_approval)
                return reply(c, 'awaiting_issuer_approval');
            if (data.status !== 'ready' || !record(data.card_details))
                return reply(c, 'awaiting_card');
            const d = data.card_details;
            delete c.balance;
            delete c.observed;
            delete c.updated;
            if (typeof d.available_balance === 'number' && Number.isFinite(d.available_balance) && d.available_balance >= 0) {
                c.balance = d.available_balance;
                c.observed = Date.now();
                c.updated = typeof data.last_updated_timestamp === 'number' && Number.isFinite(data.last_updated_timestamp) && data.last_updated_timestamp > 0 ? data.last_updated_timestamp * 1000 : undefined;
            }
            if (path === '/v1/provider-card' && (c.vaultId || c.name === undefined)) {
                await put(c);
                return reply(c, c.vaultId ? 'saved' : 'ready');
            }
            let zip = record(d.billing_address) ? d.billing_address.zip : undefined;
            if (!text(zip, 32) && c.addressId) {
                const response = await deps.dispatch(new Request('https://credentials.internal/v1/vault-entry/' + c.addressId, { method: 'POST' }));
                if (!response.ok)
                    return fail('address_not_found', 404);
                const address: RecordValue = await response.json();
                if (address.kind !== 'address')
                    return fail('address_required');
                zip = address.zip;
            }
            if (!text(zip, 32)) {
                await put(c);
                return reply(c, 'awaiting_billing_address');
            }
            if (!c.entry) {
                const entry = { name: c.name ?? 'Laso card', card_number: d.card_number, expiry_month: String(d.exp_month).padStart(2, '0'), expiry_year: String(d.exp_year), ...(d.cvv === undefined ? {} : { cvv: d.cvv }), billing_zip: zip };
                if (!deps.validEntry(entry, 'card'))
                    return fail('provider_card_invalid', 502);
                c.entry = entry;
            }
            await put(c);
        }
        if (!c.vaultId && (path === '/v1/provider-store' || c.name !== undefined)) {
            // The capture's immutable save UUID makes a crash after the existing Vault
            // transaction recoverable without creating a duplicate or contacting issuance.
            const saved = await deps.dispatch(new Request('https://credentials.internal/v1/vault/' + (c.source.schema === 'api-key-v1' ? 'api_key' : 'card'), { method: 'POST', headers: { 'content-type': 'application/json', 'x-nanocodex-operation-id': c.saveOperation }, body: JSON.stringify(c.entry) }));
            if (!saved.ok)
                return fail('vault_save_failed', 503);
            const receipt: RecordValue = await saved.json();
            if (!opaque(receipt.id))
                return fail('vault_save_failed', 503);
            c.vaultId = receipt.id;
            if (typeof receipt.last4 === 'string' && /^[0-9]{4}$/.test(receipt.last4))
                c.last4 = receipt.last4;
            const envelope = await deps.vault.seal(c);
            await deps.storage.transaction(async (txn) => { await txn.put('provider-capture:' + c.id, envelope); await txn.put('provider-vault:' + c.vaultId, c.id); });
        }
        return reply(c, c.vaultId ? 'saved' : 'ready');
    }
    catch {
        return fail('provider_operation_failed', 503);
    }
}
