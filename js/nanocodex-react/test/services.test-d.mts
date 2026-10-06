import { createElement } from 'react';
import { createServicesClient, createHostedRequest } from 'nanocodex/services';
import { ServicesProvider, HostedServiceButton, useProvisionPhone, useVault, usePhoneMessages, usePhoneRequest, useReleasePhone } from 'nanocodex-react/services';
const client = createServicesClient({ apiKey: 'synthetic-server-key' });
createElement(ServicesProvider, { client });
createElement(HostedServiceButton, { request: createHostedRequest({ appOrigin: 'https://example.com' }), onComplete(result) { if (result.service === 'vault') { const id: string = result.vault_id; void id; } } });
function Component() {
 const vault = useVault();
 const messages = usePhoneMessages(undefined);
 const request = usePhoneRequest('operation-id');
 const provision = useProvisionPhone();
 provision.mutate({ operation_id: crypto.randomUUID(), phone_number: '+12025550101', country: 'US' });
 // @ts-expect-error A stable operation ID is mandatory
 provision.mutate({ phone_number: '+12025550101', country: 'US' });
 useReleasePhone().mutate({ id: 'number-id', operation_id: crypto.randomUUID() });
 return [vault, messages, request];
}
void Component;
createElement(HostedServiceButton, { request: createHostedRequest({ action: 'select', appOrigin: 'https://example.com' }), onComplete(result) {
  if (result.service === 'vault' && result.action === 'select') {
    const kind: string = result.kind;
    // @ts-expect-error Picker shares identity only
    result.secret;
    void kind;
  }
} });
