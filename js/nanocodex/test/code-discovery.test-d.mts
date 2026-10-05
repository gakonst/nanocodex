import { createCodeDiscovery, type CodeDiscovery, type CodeToolDescription } from 'nanocodex-tools/runtime/code-discovery';
import { Agent as NodeAgent, Transport as NodeTransport, type CodeEvaluator } from '../node/index.mjs';
import { Agent as HostAgent, Transport as HostTransport } from '../host/index.mjs';
import type { AgentOptions } from '../types.mjs';

const options: AgentOptions = { inlineDocsTokenBudget: 3000 };
const discovery: CodeDiscovery = createCodeDiscovery([{ type: 'function', name: 'library__lookup', parameters: {} }]);
const full: CodeToolDescription | undefined = discovery.describeTool('library__lookup');
const summaries: readonly { name: string; callableName: string; description: string }[] = discovery.searchTools('lookup', { limit: 1 });
const namespace: readonly CodeToolDescription[] = discovery.describeNamespace('library');
const evaluate: CodeEvaluator = async (_source, environment) => {
  const found = environment.searchTools('lookup')[0];
  const description = environment.describeTool(found.callableName);
  environment.text(environment.describeNamespace('library'));
  environment.text(description?.inputSchema);
  environment.text(await environment.tools[found.callableName]({ key: 'synthetic' }));
};
NodeAgent.create({ ...options, transport: NodeTransport.openAi({ apiKey: 'synthetic' }), codeEvaluator: evaluate });
HostAgent.create({ ...options, transport: HostTransport.openAi({ apiKey: 'synthetic' }), codeEvaluator: evaluate });
void full; void summaries; void namespace;
