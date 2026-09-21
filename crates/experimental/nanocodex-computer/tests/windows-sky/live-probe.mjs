// Opt-in diagnostic: inventory and Calculator launch with every approval denied.
// Never returns acceptance to an elicitation and never changes provider config.
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
const receipt = JSON.parse(fs.readFileSync(path.join(process.env.USERPROFILE, '.nanocodex/runtimes/openai-cua/provider.json'), 'utf8'));
// The installed receipt already includes the native host wrapper.
const child = spawn(receipt.executable, receipt.args, { env: { ...process.env, ...receipt.environment }, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
let nextId = 0; const pending = new Map();
const lines = createInterface({ input: child.stdout });
let stderr = '';
child.stderr.on('data', data => { stderr = (stderr + data).slice(-8000); });
lines.on('line', line => {
  let message; try { message = JSON.parse(line); } catch { console.log(JSON.stringify({ malformedProviderLine: line.slice(0, 500) })); return; }
  if (message.method && message.id !== undefined) {
    console.log(JSON.stringify({ elicitation: message.method, params: message.params }));
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, result: { action: 'decline' } }) + '\n');
  } else if (pending.has(message.id)) { const resolve = pending.get(message.id); pending.delete(message.id); resolve(message); }
});
function rpc(method, params) {
  return new Promise(resolve => { const id = ++nextId; pending.set(id, resolve); child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n'); });
}
const timer = setTimeout(() => { console.log(JSON.stringify({ timeout: true, stderr })); child.kill(); process.exit(2); }, 45000);
try {
  console.log(JSON.stringify({ initialize: await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: { elicitation: { form: {} } }, clientInfo: { name: 'Nanocodex Windows Sky verification', version: '1' } }) }));
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
  const catalog = await rpc('tools/list', {}); console.log(JSON.stringify({ tools: catalog.result.tools.map(t => ({ name: t.name, inputSchema: t.name === 'turn_ended' ? t.inputSchema : undefined })) }));
  const inventory = await rpc('tools/call', { name: 'js', arguments: { title: 'Read official Windows Sky inventory', code: 'await cua.getState()' }, _meta: { 'x-codex-turn-metadata': { session_id: 'nanocodex-agent8-live-proof', turn_id: 'inventory-1', call_id: 'inventory', model: 'diagnostic' } } });
  console.log(JSON.stringify({ inventory: { ...inventory, result: { ...inventory.result, content: inventory.result?.content?.filter(c => c.type !== 'text' || !c.text.startsWith('## Computer Use')) } } }));
  console.log(JSON.stringify({ deniedLaunch: await rpc('tools/call', { name: 'js', arguments: { title: 'Verify Windows consent denial', code: 'await cua.computer.launch_app({ app: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App" })' }, _meta: { 'x-codex-turn-metadata': { session_id: 'nanocodex-agent8-live-proof', turn_id: 'inventory-1', call_id: 'denial' } } }) }));
  console.log(JSON.stringify({ turnEnded: await rpc('tools/call', { name: 'turn_ended', arguments: { hook_event_name: 'turn_ended', session_id: 'nanocodex-agent8-live-proof', turn_id: 'inventory-1' } }) }));
} finally {
  clearTimeout(timer); child.stdin.end();
  await new Promise(resolve => { child.once('exit', resolve); setTimeout(() => { child.kill(); resolve(); }, 3000).unref(); });
  console.log(JSON.stringify({ stderr }));
}
