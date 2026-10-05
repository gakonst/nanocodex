import { createRequire } from 'node:module';
import { realpath } from 'node:fs/promises';
import { createNodeProcessTools } from 'nanocodex-tools/node';
import { resolveBackend, ownedBackendTools, nativeClaudeDefaults } from '../runtime/backend.mjs';
import { open } from './workspace.mjs';
import { openAi } from './Transport.mjs';
import { updatePlan, viewImage } from '../tools/standard.mjs';
import { toolResult } from '../runtime/code-runtime.mjs';

const common = ['model', 'thinking', 'instructions', 'sessionId', 'workspace', 'durability', 'durabilityId', 'module'];
const familyOptions = {
  codex: ['reasoningMode', 'fastMode', 'instantToolSteering', 'rawApiEvents', 'additionalInstructions', 'beforeCompaction', 'resume', 'toolMode', 'mcp', 'codeEvaluator', 'codeEffectJournal'],
  claude: ['maxTokens', 'cache', 'adaptiveThinking', 'keepThinking', 'parallelTools', 'clientToolSearch', 'contextWindowTokens', 'autoCompactWindowTokens', 'autoCompact', 'systemBlocks', 'terminalReceiptRetention', 'subagents', 'serverTools'],
};

export async function createBackendAgent(options, create) {
  const { backend, ...configuration } = options;
  const { kind, ...provider } = resolveBackend(backend);
  for (const key of Object.keys(configuration)) {
    if (!common.includes(key) && !familyOptions[kind].includes(key)) throw new TypeError(`Agent.create backend ${kind} does not accept ${key}; use the explicit constructor for custom host tools or transport`);
  }
  if (configuration.model !== undefined) {
    if (typeof configuration.model !== 'string' || !configuration.model.trim()) throw new TypeError('backend model must be a non-empty string');
    const model = configuration.model.toLowerCase();
    const otherFamily = kind === 'codex'
      ? model.startsWith('claude-') || ['opus', 'sonnet', 'fable', 'haiku'].includes(model)
      : model.startsWith('gpt-') || /^(?:o[134](?:-|$)|codex(?:-|$))/.test(model) || ['astra', 'sol', 'luna', '@cf/zai-org/glm-5.3', 'kimi-k3', 'mimo-v2.6-pro'].includes(model);
    if (otherFamily) throw new TypeError('model family does not match backend');
  }
  if (configuration.workspace !== undefined && (typeof configuration.workspace !== 'string' || !configuration.workspace.trim())) throw new TypeError('backend workspace must be a non-empty directory path');
  const workspace = await realpath(configuration.workspace ?? process.cwd());
  if (kind === 'claude') {
    const { createClaudeTools } = await import('./claude-tools.mjs');
    const local = await createClaudeTools({ workspace, module: configuration.module });
    try {
      return await create({ ...configuration, workspace, model: configuration.model ?? 'claude-opus-5-5', harness: 'claude',
        auth: { apiKey: provider.apiKey }, ...(provider.endpoint === undefined ? {} : { endpoint: provider.endpoint }),
        subagents: configuration.subagents ?? {}, tools: local.tools, [nativeClaudeDefaults]: true, [ownedBackendTools]: local.close });
    } catch (error) { await local.close(); throw error; }
  }
  const filesystem = await open({ path: workspace, root: workspace });
  const processes = await createNodeProcessTools({ workspace });
  try {
    return await create({ ...configuration, workspace, filesystem, model: configuration.model ?? 'gpt-6-astra',
      transport: openAi(provider), tools: [...processes.tools, updatePlan(), viewImage({ workspace: filesystem }), {
        name: 'apply_patch', description: 'Apply a Rust-verified patch to the workspace.',
        parameters: { type: 'object', additionalProperties: false },
        async handler(input, context) {
          const { applyBrowserPatch } = createRequire(import.meta.url)('../pkg-node/nanocodex.js');
          return toolResult(await applyBrowserPatch(input, context.sessionId), {});
        },
      }], [ownedBackendTools]: processes.close });
  } catch (error) { await processes.close(); throw error; }
}
