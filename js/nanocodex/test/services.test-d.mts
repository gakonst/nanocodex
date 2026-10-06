import { createServicesClient, createHostedRequest, openHostedPopup, ServiceError, serviceResource, type ServicesClient } from 'nanocodex/services';
import { Client } from 'nanocodex/connect';
const direct: ServicesClient = createServicesClient({ apiKey: 'server-only-example' });
const connect = Client.create({ appId: 'services-example', session: false });
const connection = await connect.connection.connect({ capabilities: { services: { vault: { ids: ['opaque-vault-item'], origins: ['https://example.com'], request: true }, phone: { numberIds: [], read: true, provision: true, release: false } } } });
const scoped = createServicesClient({ connect, grantId: connection.grant.id });
const entries = await scoped.vault.list();
for (const entry of entries.vault) {
  if (entry.kind === 'totp') {
    const issuer: string = entry.issuer;
    // @ts-expect-error Vault seeds never leave the broker
    entry.seed;
    void issuer;
  }
}
const receipt = await direct.vault.request({ vault_id: 'opaque-vault-item', url: 'https://example.com', method: 'POST', body_encoding: 'json', body: '{"code":"{{NANOCODEX_VAULT_TOTP}}"}' });
const status: number = receipt.status;
// @ts-expect-error Broker responses are status-only
receipt.body;
await scoped.phone.provision({ operation_id: crypto.randomUUID(), phone_number: '+12025550101', country: 'US' });
await scoped.phone.release('number-id', { operation_id: crypto.randomUUID() });
// @ts-expect-error Writes require a caller-retained operation ID
await scoped.phone.provision({ phone_number: '+12025550101', country: 'US' });
// @ts-expect-error Approval remains in the hosted account UI
scoped.phone.approve({});
// @ts-expect-error Direct and Connect credentials are exclusive
createServicesClient({ connect, grantId: 'grant', apiKey: 'key' });
const hosted = createHostedRequest({ appOrigin: 'https://example.com' });
const metadata = await openHostedPopup(hosted);
// @ts-expect-error Enrollment returns metadata only
metadata.code;
serviceResource({ phone: { numberIds: [], read: true, provision: false, release: false } });
const error = new ServiceError('unknown', { outcomeUnknown: true });
void [status, error];
const selection = createHostedRequest({ service: 'vault', action: 'select', appOrigin: 'https://example.com' });
const selected = await openHostedPopup(selection);
if (selected.service === 'vault' && selected.action === 'select') {
  const id: string = selected.vault_id;
  const kind: import('nanocodex/services').VaultMetadata['kind'] = selected.kind;
  // @ts-expect-error Selection shares identity only, not private metadata
  selected.username;
  void [id, kind];
}
// @ts-expect-error Selection applies only to Vault
createHostedRequest({ service: 'phone', operationId: crypto.randomUUID(), action: 'select', appOrigin: 'https://example.com' });

const links = await direct.links({connect: 'cloudflare', add: 'totp'});
const linkUrl: string = links.connections;
const enrollmentLink = await direct.hosted({service: 'vault', kind: 'login'});
const hostedUrl: string = enrollmentLink.url;
await direct.account.connectors.start('google', {return_to: '/connect'});
await direct.account.connectors.cloudflare({vault_id: 'selected-vault-id'});
await direct.account.connectors.mcp.create({target: 'https://mcp.example/mcp'});
await direct.account.vault.create('login', {name: 'Example', username: 'example', password: 'synthetic-private-form-value'});
await direct.account.vault.store({capture_id: 'captured-item', operation_id: crypto.randomUUID()});
await direct.account.vault.card({vault_id: 'saved-card', operation: 'refresh', operation_id: crypto.randomUUID()});
// @ts-expect-error full permission keys must use the dedicated Cloudflare path
await direct.account.connectors.start('cloudflare', {});
// @ts-expect-error no secret material goes into a captured-provider save
await direct.account.vault.store({card_number: 'synthetic', operation_id: crypto.randomUUID()});
// @ts-expect-error a refresh requires a stable operation ID
await direct.account.vault.card({vault_id: 'saved-card', operation: 'refresh'});
// @ts-expect-error login enrollment requires private password input
await direct.account.vault.create('login', {name: 'Example', username: 'example'});
void linkUrl; void hostedUrl;
