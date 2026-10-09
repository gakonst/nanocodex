// Worker entry for managed-curl-hand-recovery.test.mjs. The shipped managed
// worker handles every request; only unavoidable fixtures are added: first
// identity enrollment (an API key for a synthetic user), the external model
// provider, which replays a directive found in the user's own turn input, and
// a managed->account network fault seam selected by a marker in that input.
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools as ShippedAccountHostedTools } from '../../src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from '../../src/account-auth.ts';
export { DurableAgentSession, UserAccount, Organization, ApiKeyRecord, NonceStorage };
export { UserDataScope } from '../../src/user-data-scope.ts';
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);

const DIRECTIVE = /HAND_STEP (\{.*\})/;

// Network fault injection only. The shipped account object handles every
// request; for an /invoke whose command carries a marker, the managed caller
// loses the response in transit (connection lost while the command runs) or
// receives a truncated body after it completed.
export class AccountHostedTools extends ShippedAccountHostedTools {
  async fetch(request) {
    if (new URL(request.url).pathname !== '/invoke') return super.fetch(request);
    const body = await request.clone().text();
    const lose = body.includes('__LOSE_ACCOUNT_RESPONSE__'), truncate = body.includes('__TRUNCATE_ACCOUNT_RESPONSE__');
    if (!lose && !truncate) return super.fetch(request);
    const answered = super.fetch(request);
    console.info({ type: 'fixture.network_fault', fault: lose ? 'lose_response' : 'truncate_response' });
    if (truncate) {
      const response = await answered;
      await response.body?.cancel();
      return new Response('{', { headers: { 'content-type': 'application/json' } });
    }
    answered.catch(() => {});
    await new Promise(resolve => setTimeout(resolve, 500));
    throw new Error('Network connection lost.');
  }
}
const text = item => typeof item?.content === 'string' ? item.content
  : (item?.content ?? []).map(part => part.text ?? '').join('');

// Scripted provider. A user turn carrying `HAND_STEP {"cmd","workdir","yield"}`
// gets one Code Mode cell that calls the Hand's exec_command; the cell's output
// is then echoed verbatim as the final assistant message.
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({ tools: [], machines: [], connections: [] });
    const [client, server] = Object.values(new WebSocketPair()); server.accept();
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', event => {
      const body = JSON.parse(event.data), input = body.input ?? [];
      const last = input.at(-1);
      const id = 'resp_' + crypto.randomUUID();
      const usage = { input_tokens: 1, output_tokens: 1, total_tokens: 2 };
      console.info({ type: 'fixture.model', last_type: last?.type ?? last?.role, items: input.length });
      if (last?.type === 'custom_tool_call_output' || last?.type === 'function_call_output') {
        const output = typeof last.output === 'string' ? last.output : JSON.stringify(last.output);
        server.send(JSON.stringify({ type: 'response.completed', response: { id, status: 'completed', end_turn: true, usage,
          output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'HAND_RESULT ' + output }] }] } }));
        return;
      }
      const user = input.filter(item => item.role === 'user').map(text).at(-1) ?? '';
      const step = DIRECTIVE.exec(user)?.[1];
      if (!step) {
        server.send(JSON.stringify({ type: 'response.completed', response: { id, status: 'completed', end_turn: true, usage,
          output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'NO_HAND_STEP' }] }] } }));
        return;
      }
      const { cmd, workdir, yield: yieldMs, stdin } = JSON.parse(step);
      // An optional `stdin` writes once to the started process session.
      const cell = `let value;
try { value = await tools.exec_command(${JSON.stringify({ cmd, workdir, shell: '/bin/sh', login: false, yield_time_ms: yieldMs ?? 30000 })}); }
catch (error) { value = { thrown: String(error?.message ?? error) }; }
${stdin === undefined ? '' : `if (value?.session_id !== undefined) {
  try { value = { exec: value, stdin: await tools.write_stdin({ session_id: value.session_id, chars: ${JSON.stringify(stdin)}, yield_time_ms: 2000 }) }; }
  catch (error) { value = { exec: value, stdin_thrown: String(error?.message ?? error) }; }
}`}
text(JSON.stringify(value));`;
      server.send(JSON.stringify({ type: 'response.completed', response: { id, status: 'completed', end_turn: false, usage,
        output: [{ type: 'custom_tool_call', name: 'exec', call_id: 'call_' + crypto.randomUUID().replaceAll('-', ''), input: cell }] } }));
    });
    return new Response(null, { status: 101, webSocket: client });
  }
}

export default { async fetch(request, env, ctx) {
  // Identity bootstrap only: reachable through the in-process Miniflare
  // dispatcher, never through the public account ingress used by curl.
  if (new URL(request.url).pathname === '/__fixture') {
    const body = await request.json(); await ensureAccount(env, body.user, true);
    const auth = await (await env.NANOCODEX_USERS.getByName(body.user).fetch('https://user.internal/authorization')).json();
    const key = await createApiKey(env, { kind: 'api_key', userId: body.user, ...auth.grant, subjectId: 'api_key:' + body.user,
      credentialId: 'fixture', capabilities: ['agents:read', 'agents:write', 'tools:use'] }, 'Synthetic curl Hand recovery');
    return Response.json(key);
  }
  return worker.fetch(request, env, ctx);
} };
