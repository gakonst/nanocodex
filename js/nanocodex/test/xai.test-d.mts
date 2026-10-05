import { Xai as NodeXai, Agent as NodeAgent } from '../node/index.mjs';
import { Xai as BrowserXai } from '../browser/index.mjs';
import { Xai as WorkerXai } from '../worker/index.mjs';
import { Xai as HostXai, Subagents } from '../host/index.mjs';
import type { Options, ToolResult } from '../node/Xai.mjs';
const options: Options = {
  model: 'grok-4.6', auth: { headers: async () => ({ authorization: 'host-owned' }) },
  tools: [{ name: 'read_file', description: 'Explicit rooted read capability', inputSchema: { type: 'object' }, handler(_input, context) {
    const id: string = context.callId;
    const signal: AbortSignal = context.signal;
    void id; void signal;
    return { output: 'synthetic contents', success: true } satisfies ToolResult;
  } }],
  thinking: 'xhigh', autoCompactThresholdPercent: 80,
  maxRetries: 0, repetitionLimit: 1, compactionKeepTail: 0,
};
NodeXai.create(options).then(async agent => {
  const turn = agent.turn.prompt({ input: 'text' });
  await turn.steer({ input: 'correction', messageId: 'steer' });
  const withdrawn: boolean = await turn.withdrawSteer({ messageId: 'steer' });
  void withdrawn;
  const result = await turn.result();
  const output: string = result.finalMessage;
  const tokens: number = (await result.usage()).input_tokens;
  const history: readonly Record<string, unknown>[] = (await agent.session.context()).history;
  void output; void tokens; void history;
  await agent.session.cancel(); await agent.session.compact(); await agent.session.shutdown();
  // @ts-expect-error Native xAI prompt currently accepts text.
  agent.turn.prompt({ input: [{ type: 'input_text', text: 'x' }] });
});
HostXai.create(options); BrowserXai.create(options); WorkerXai.create(options);
NodeAgent.create({ ...options, harness: 'xai', subagents: {} }).then(async agent => {
  await Subagents.spawn(agent, { harness: 'xai', model: 'grok-4.6', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
  // @ts-expect-error Family mismatch.
  await Subagents.spawn(agent, { harness: 'xai', model: 'sonnet', role: 'fixture', task: 'synthetic', outputSchema: { type: 'string' } });
});
// @ts-expect-error Explicit credentials required.
NodeXai.create({ model: 'grok-4.6' });
// @ts-expect-error Authentication choices are exclusive.
NodeXai.create({ ...options, auth: { apiKey: 'x', headers: () => ({}) } });
// @ts-expect-error Durability needs store and ID.
NodeXai.create({ model: 'grok-4.6', auth: { apiKey: 'synthetic' }, durabilityId: 'state' });
// @ts-expect-error Unsupported reasoning.
NodeXai.create({ ...options, thinking: 'max' });
