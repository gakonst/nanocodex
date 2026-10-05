import { Agent, Backend, Transport } from 'nanocodex/node';
import { Backend as RootBackend } from 'nanocodex';
import type { Backend as Selection } from '../runtime/backend.mjs';
const codex = Backend.codex({ apiKey: 'synthetic' });
const claude = Backend.claude({ apiKey: 'synthetic' });
const checkKind: 'codex' = codex.kind;
const root: Selection = RootBackend.claude({ apiKey: 'synthetic' });
function select(kind: 'codex' | 'claude') { return kind === 'codex' ? codex : claude; }
const generic = await Agent.create({ backend: select('claude'), workspace: '.' });
const turn = generic.turn.prompt({ input: 'Read README.md' });
const result = await turn.result();
const text: string = result.finalMessage;
await generic.session.shutdown();
await Agent.create({ backend: codex, toolMode: 'direct' });
await Agent.create({ backend: claude, maxTokens: 1024 });
await Agent.create({ transport: Transport.openAi({ apiKey: 'synthetic' }), tools: {} });
// @ts-expect-error opaque backends cannot be forged
await Agent.create({ backend: { kind: 'claude' } });
// @ts-expect-error transport conflicts with backend
await Agent.create({ backend: codex, transport: Transport.openAi({ apiKey: 'synthetic' }) });
// @ts-expect-error tool arrays belong to the explicit constructor
await Agent.create({ backend: claude, tools: [] });
// @ts-expect-error discriminant cannot be changed
codex.kind = 'claude';
// @ts-expect-error Claude does not accept OpenAI endpoints
Backend.claude({ apiKey: 'synthetic', apiBaseUrl: 'https://example.invalid' });
void [checkKind, root, text];

// @ts-expect-error durability requires both store and ID
await Agent.create({ backend: claude, durabilityId: 'unsupported' });
// @ts-expect-error durability requires both store and ID
await Agent.create({ backend: codex, durabilityId: 'missing-store' });
// @ts-expect-error MCP discovery requires Code Mode
await Agent.create({ backend: codex, mcp: {}, toolMode: 'direct' });
