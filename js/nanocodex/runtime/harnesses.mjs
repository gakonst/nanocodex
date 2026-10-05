import { createXaiHost } from './xai-host.mjs';
import { toXaiConfig } from './xai.mjs';
import { registerDefinitionHost, releaseDefinitionHost, releaseHostSessions, toWasmConfig } from '../internal.mjs';
import { createClaudeHost } from './claude-host.mjs';
import { toClaudeConfig } from './claude.mjs';
import { createBrowserHost } from '../browser/host.mjs';
import { resolveResponsesTransport } from './responses-transport.mjs';
import { resolveTools } from './tool-configuration.mjs';

/** Explicit alternate-family capabilities; the Rust registry owns every child. */
export async function prepareHarnesses(harnesses, emit, {
  createCodexHost = createBrowserHost,
  subagentSessions, subagentRouting, toolProviders,
} = {}) {
  if (harnesses === undefined) return { close() {} };
  if (!harnesses || typeof harnesses !== 'object' || Array.isArray(harnesses)
    || Object.keys(harnesses).some(key => !['codex', 'claude', 'xai'].includes(key))) throw new TypeError('harnesses accepts codex, claude and xai capabilities');
  const hosts = [];
  const close = async () => {
    await Promise.all(hosts.map(async ([id, host]) => {
      releaseHostSessions(host);
      releaseDefinitionHost(id);
      await host.dispose();
    }));
  };
  const result = { close };
  try {
    if (harnesses.xai) {
      const options = harnesses.xai;
      if (options.durability !== undefined || options.durabilityId !== undefined || options.sessionId !== undefined || options.harnesses !== undefined) throw new TypeError('child harness capabilities must be ephemeral and cannot contain nested harnesses');
      const config = toXaiConfig(options);
      const host = createXaiHost({ ...options, endpoint: options.endpoint ?? "https://api.x.ai/v1/responses", onEvent: emit, subagentSessions, subagentRouting });
      const id = registerDefinitionHost(host);
      hosts.push([id, host]);
      result.xai = { ...config, hostDefinitionId: id, authHostId: id, tools: JSON.parse(host.toolDefinitions()) };
    }
    if (harnesses.claude) {
      const options = harnesses.claude;
      if (options.durability !== undefined || options.durabilityId !== undefined || options.sessionId !== undefined || options.harnesses !== undefined) throw new TypeError('child harness capabilities must be ephemeral and cannot contain nested harnesses');
      const config = toClaudeConfig(options);
      const host = createClaudeHost({ ...options, onEvent: emit, subagentSessions, subagentRouting });
      const id = registerDefinitionHost(host);
      hosts.push([id, host]);
      result.claude = { ...config, hostDefinitionId: id, authHostId: id, tools: JSON.parse(host.toolDefinitions()) };
    }
    if (harnesses.codex) {
      const options = harnesses.codex;
      if (options.durability !== undefined || options.durabilityId !== undefined || options.sessionId !== undefined || options.harnesses !== undefined || options.resume !== undefined) throw new TypeError('child harness capabilities must be ephemeral and cannot contain nested harnesses');
      const transport = resolveResponsesTransport(options.transport);
      if (transport.subscription || transport.mpp) throw new TypeError('alternate Codex harness requires an explicit API or host-managed transport');
      if (options.codeEvaluator !== undefined && typeof options.codeEvaluator !== 'function') throw new TypeError('Codex harness codeEvaluator must be a function');
      if (createCodexHost === createBrowserHost && options.toolMode === 'code' && typeof globalThis.Worker !== 'function' && options.codeEvaluator === undefined) throw new TypeError('Codex harness Code Mode requires an explicit codeEvaluator outside a browser Worker host');
      const { tools } = resolveTools(options.tools, { defaultSubagents: false });
      const host = createCodexHost({ ...transport, hostAuth: transport.hostAuth === true, tools, workspace: options.workspace, toolMode: options.toolMode ?? 'direct', codeEvaluator: options.codeEvaluator, onEvent: emit, subagentSessions, subagentRouting, toolProviders });
      const id = registerDefinitionHost(host);
      hosts.push([id, host]);
      await host.ready();
      result.codex = toWasmConfig({ ...options, ...transport, apiKey: transport.apiKey ?? 'host-managed', hostDefinitionId: id });
    }
    return result;
  } catch (error) { await close(); throw error; }
}
