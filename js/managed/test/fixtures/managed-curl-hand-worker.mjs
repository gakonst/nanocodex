// Worker entry for managed-curl-hand-recovery.test.mjs. The shipped managed
// worker handles every request; only unavoidable fixtures are added: first
// identity enrollment (an API key for each synthetic user), the external model
// provider, which replays a directive found in the user's own turn input, and
// a per-hop account network fault seam selected by that input or armed once.
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools as ShippedAccountHostedTools } from '../../src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from '../../src/account-auth.ts';
export { DurableAgentSession, UserAccount, Organization, ApiKeyRecord, NonceStorage };
export { UserDataScope } from '../../src/user-data-scope.ts';
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);

const DIRECTIVE = /HAND_STEP (\{.*\})/;

// Network fault injection only. The shipped account object handles every
// request; a selected response is lost in transit (connection lost while the
// call runs) or truncated after it completed. Faults are scoped to one network
// hop so a shared call is never faulted twice:
//   managed: managed session -> its account /invoke (the outer hop),
//   shared:  recipient account -> owner account /shared-invoke (the inner hop).
// A reset fault is a real workerd object reset (ctx.abort), the same failure a
// deploy or eviction produces: every caller stub connected to the old instance
// is broken and the Hand WebSocket is disconnected. It applies to the managed
// /invoke hop (after the call is dispatched) or, armed once, to a selected-Hand
// /snapshot lookup (before it answers).
// SYNTHETIC overload fault: workerd cannot overload an object on demand, so the
// managed /invoke hop throws an error carrying Cloudflare's overloaded=true
// property after the call was dispatched. Receipt reads and cancellations that
// reach this object are counted (fixture.account_request), never altered.
// The owner's in-object forward of a shared call (session "shared:<hash>") is
// never faulted. Markers in the call input select a fault; inputs without a
// free-text field (an empty write_stdin poll) use a one-shot armed fault.
export class AccountHostedTools extends ShippedAccountHostedTools {
  #armed = [];
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/__fault') { this.#armed.push(await request.json()); return Response.json({ armed: this.#armed.length }); }
    if (path === '/snapshot') {
      const index = this.#armed.findIndex(arm => arm.hop === 'snapshot');
      const body = index >= 0 ? await request.clone().json().catch(() => ({})) : undefined;
      if (index >= 0 && typeof body?.machine_id === 'string') {
        this.#armed.splice(index, 1);
        console.info({ type: 'fixture.network_fault', fault: 'reset', hop: 'snapshot', name: 'snapshot' });
        this.ctx.abort('synthetic account object reset');
      }
    }
    if (path === '/invoke-receipt' || path === '/cancel-invocation') console.info({ type: 'fixture.account_request', path });
    if (path !== '/invoke' && path !== '/shared-invoke') return super.fetch(request);
    const body = await request.clone().text();
    let parsed; try { parsed = JSON.parse(body); } catch { return super.fetch(request); }
    const invocation = path === '/shared-invoke' ? parsed?.invocation : parsed;
    const hop = path === '/shared-invoke' ? 'shared' : String(invocation?.session_id ?? '').startsWith('shared:') ? 'owner' : 'managed';
    let fault;
    if (hop === 'managed' && body.includes('__LOSE_ACCOUNT_RESPONSE__')) fault = 'lose_response';
    else if (hop === 'managed' && body.includes('__RESET_ACCOUNT_OBJECT__')) fault = 'reset';
    else if (hop === 'managed' && body.includes('__OVERLOAD_ACCOUNT_OBJECT__')) fault = 'synthetic_overload';
    else if (hop === 'managed' && body.includes('__TRUNCATE_ACCOUNT_RESPONSE__')) fault = 'truncate_response';
    else if (hop === 'shared' && body.includes('__LOSE_SHARED_HOP_RESPONSE__')) fault = 'lose_response';
    else {
      const index = this.#armed.findIndex(arm => arm.hop === hop && arm.name === invocation?.name
        && (arm.chars === undefined || invocation?.input?.chars === arm.chars));
      if (index >= 0) fault = this.#armed.splice(index, 1)[0].fault;
    }
    if (!fault) return super.fetch(request);
    const answered = super.fetch(request);
    console.info({ type: 'fixture.network_fault', fault, hop, name: invocation?.name });
    if (fault === 'synthetic_overload') {
      answered.then(response => response.body?.cancel(), () => {});
      await new Promise(resolve => setTimeout(resolve, 700));
      throw Object.assign(new Error('Durable Object is overloaded (synthetic fixture fault).'), { overloaded: true, retryable: false });
    }
    if (fault === 'reset') {
      // Reset only after the call is durably dispatched and running on the Hand.
      answered.catch(() => {});
      await new Promise(resolve => setTimeout(resolve, 700));
      this.ctx.abort('synthetic account object reset');
      return await answered;
    }
    if (fault === 'truncate_response') {
      const response = await answered;
      await response.body?.cancel();
      return new Response('{', { headers: { 'content-type': 'application/json' } });
    }
    answered.then(async response => { console.info({ type: 'fixture.network_fault.discarded', hop, status: response.status }); await response.body?.cancel(); },
      error => console.info({ type: 'fixture.network_fault.discarded', hop, error: String(error?.message ?? error) }));
    await new Promise(resolve => setTimeout(resolve, 500));
    throw new Error('Network connection lost.');
  }
}
const text = item => typeof item?.content === 'string' ? item.content
  : (item?.content ?? []).map(part => part.text ?? '').join('');

// Scripted provider. A user turn carrying `HAND_STEP {"cmd","workdir","yield"}`
// gets one Code Mode cell that calls the Hand's exec_command; `HAND_STEP {"code"}`
// replays that exact Code Mode cell (the scripted model's own program). The
// cell's output is then echoed verbatim as the final assistant message.
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
        // A screen published after this turn's routes were captured is
        // discoverable only from a new cell: resend the same cell (bounded).
        const cells = input.filter(item => item.type === 'custom_tool_call');
        if (output.includes('discover in a new Code Mode cell') && cells.length < 4 && typeof cells.at(-1)?.input === 'string') {
          server.send(JSON.stringify({ type: 'response.completed', response: { id, status: 'completed', end_turn: false, usage,
            output: [{ type: 'custom_tool_call', name: 'exec', call_id: 'call_' + crypto.randomUUID().replaceAll('-', ''), input: cells.at(-1).input }] } }));
          return;
        }
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
      const { cmd, workdir, yield: yieldMs, stdin, writes, screen, code } = JSON.parse(step);
      // An optional `stdin` writes once to the started process session;
      // `writes` is an ordered list of write_stdin calls (empty chars poll).
      // `screen` is one native screen action through the Hand's CUA tool.
      const cell = code !== undefined ? code : screen !== undefined ? `let value;
try { await tools.mcp__cua_repl__js({ workdir: ${JSON.stringify(workdir)} });
  value = await tools.mcp__cua_repl__js(${JSON.stringify({ workdir, ...screen })}); }
catch (error) { value = { thrown: String(error?.message ?? error) }; }
text(JSON.stringify(value));` : `let value;
try { value = await tools.exec_command(${JSON.stringify({ cmd, workdir, shell: '/bin/sh', login: false, yield_time_ms: yieldMs ?? 30000 })}); }
catch (error) { value = { thrown: String(error?.message ?? error) }; }
${stdin === undefined ? '' : `if (value?.session_id !== undefined) {
  try { value = { exec: value, stdin: await tools.write_stdin({ session_id: value.session_id, chars: ${JSON.stringify(stdin)}, yield_time_ms: 2000 }) }; }
  catch (error) { value = { exec: value, stdin_thrown: String(error?.message ?? error) }; }
}`}
${writes === undefined ? '' : `if (value?.session_id !== undefined) {
  const exec = value, results = [];
  for (const write of ${JSON.stringify(writes)}) {
    try { results.push(await tools.write_stdin({ session_id: exec.session_id, ...write })); }
    catch (error) { results.push({ thrown: String(error?.message ?? error) }); break; }
  }
  value = { exec, writes: results };
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
  if (new URL(request.url).pathname === '/__fixture/fault') {
    const { user, ...arm } = await request.json();
    return env.NANOCODEX_ACCOUNT_TOOLS.getByName(user).fetch('https://account-tools.internal/__fault', { method: 'POST', body: JSON.stringify(arm) });
  }
  return worker.fetch(request, env, ctx);
} };
