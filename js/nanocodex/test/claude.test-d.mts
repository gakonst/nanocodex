import { Claude as HostClaude } from '../host/index.mjs';
import { Claude as NodeClaude } from '../node/index.mjs';
import { Claude as BrowserClaude } from '../browser/index.mjs';
import { Claude as WorkerClaude } from '../worker/index.mjs';
import type { Options, NativeToolResult, ToolResult } from '../node/Claude.mjs';
const options: Options = {
  model: 'claude-explicit', auth: { headers: async () => ({ authorization: 'host-owned' }) },
  tools: [{ name: 'Read', description: 'Explicit capability', strict: true, deferLoading: false, inputSchema: { type: 'object' }, handler(_input, context) {
    const id: string = context.callId;
    const signal: AbortSignal = context.signal;
    void id; void signal;
    return { content: [{ type: 'text', text: 'x' }], isError: true } satisfies NativeToolResult;
  } }],
  cache: '1h', thinking: 'xhigh', autoCompact: true,
};
NodeClaude.create(options).then(async agent => {
  const result = await agent.turn.prompt({ input: 'text' }).result();
  const output: string = result.finalMessage;
  const tokens: number = (await result.usage()).input_tokens;
  void output; void tokens;
  await agent.session.cancel(); await agent.session.compact(); await agent.session.shutdown();
  const document = await agent.session.document('journal');
  const value: unknown = document?.value;
  void value;
  await agent.session.compareExchangeDocuments([{key: 'journal', expectedVersion: 0, value: {count: 1}, fork: 'asOf'}]);
  await agent.session.stageDocumentWrites('turn', [{key: 'journal', expectedVersion: 1, value: {count: 2}, fork: 'asOf'}]);
  const seed = await agent.session.documentFork('turn');
  NodeClaude.create({...options, durability: {} as import('../types.mjs').DurabilityStore, durabilityId: 'child', documentFork: seed});
  // @ts-expect-error Claude does not claim the non-durable Codex session.fork API.
  agent.session.fork();
  // @ts-expect-error This SDK supports text prompts, not Codex content input.
  agent.turn.prompt({ input: [{ type: 'input_text', text: 'x' }] });
});
HostClaude.create(options); BrowserClaude.create(options); WorkerClaude.create(options);
const result: ToolResult = { output: [{ type: 'input_image', image_url: 'data:image/png;base64,aA==' }], success: false };
void result;
// @ts-expect-error Authentication cannot be inferred.
NodeClaude.create({ model: 'claude' });
// @ts-expect-error Ambiguous authentication is rejected.
NodeClaude.create({ model: 'claude', auth: { apiKey: 'x', headers: () => ({}) } });
// @ts-expect-error Durability requires the store and state ID together.
NodeClaude.create({ model: 'claude', auth: { apiKey: 'x' }, durabilityId: 'state' });
// @ts-expect-error Unknown reasoning level.
NodeClaude.create({ ...options, thinking: 'extreme' });

// @ts-expect-error Unknown Claude tool flags must not be silently discarded.
NodeClaude.create({ ...options, tools: [{ name: "Effect", description: "explicit", handler() {}, unsupported: true }] });

// Canonical task-tree API accepts an explicitly enabled native Claude owner.
import { Agent as HostAgent, Subagents, Transport } from '../host/index.mjs';
HostAgent.create({ harness: 'claude', ...options, subagents: {}, harnesses: {
  codex: { transport: Transport.openAi({ apiKey: 'synthetic-key', stateless: true }) },
} }).then(async agent => {
  await Subagents.spawn(agent, { harness: 'claude', model: 'sonnet', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
  await Subagents.spawn(agent, { model: 'sonnet', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
  await Subagents.spawn(agent, { model: 'claude-haiku-4-5', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
  await Subagents.spawn(agent, { harness: 'codex', model: 'sol', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
  // @ts-expect-error models are scoped to the selected harness family
  await Subagents.spawn(agent, { harness: 'codex', model: 'sonnet', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
  // @ts-expect-error models are scoped to the selected harness family
  await Subagents.spawn(agent, { harness: 'claude', model: 'sol', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
});
