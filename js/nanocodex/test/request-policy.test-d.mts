import { RequestPolicy } from 'nanocodex';
import { Agent, Transport } from 'nanocodex/node';
import { Agent as HostAgent } from 'nanocodex/host';
import { Agent as CloudflareAgent } from 'nanocodex/cloudflare';
import { createMemoryDurabilityStore } from 'nanocodex/durability';
import type { RequestPolicy as Policy, Receipt } from 'nanocodex/request-policy';

const policy: Policy = await RequestPolicy.create({
  durability: createMemoryDurabilityStore('synthetic-policy'), durabilityId: 'synthetic-policy',
  selection: 'balanced', models: [{ model: 'gpt-6-luna', family: 'codex', contextTokens: 100_000, maxOutputTokens: 128 }],
  route: ({ state, context }) => ({ model: 'gpt-6-luna', state: { previous: state, family: context.family } }),
  estimateInputTokens: () => 512,
  authorize: (_receipt: Receipt) => {},
  requestContext: () => ({ requestId: 'application-operation' }),
});
await policy.configure([{ kind: 'set_section', section: { name: 'project', text: 'fixture instructions' } }]);
const snapshot = await policy.snapshot();
const status: string | undefined = snapshot.requests[0]?.status;
void status;
void Agent.create({ model: 'gpt-6-luna', requestPolicy: policy, transport: Transport.openAi({ apiKey: 'synthetic' }) });
void Agent.create({ harness: 'claude', model: 'claude-sonnet-4-6', requestPolicy: policy, auth: { apiKey: 'synthetic' } });
void HostAgent.create({ requestPolicy: policy });
void CloudflareAgent.create({}, { requestPolicy: policy });
// @ts-expect-error caller-fabricated policy handles are not valid
void Agent.create({ requestPolicy: {}, transport: Transport.openAi({ apiKey: 'synthetic' }) });
// @ts-expect-error family identity is explicit
void RequestPolicy.create({ durability: createMemoryDurabilityStore('bad'), durabilityId: 'bad', selection: 'bad', models: [{ model: 'bad', family: 'openai', contextTokens: 10, maxOutputTokens: 1 }], route: () => ({ model: 'bad', state: null }) });
// @ts-expect-error native configuration changes are named patches
void policy.configure([{ kind: 'native_patch', value: {} }]);
