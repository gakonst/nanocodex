import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';
import { CUA_DESCRIPTION, CUA_PARAMETERS, CUA_RESET_DESCRIPTION, CUA_RESET_PARAMETERS } from '../contract.mjs';
const runtime = new URL('../../../crates/experimental/nanocodex-computer/runtime/', import.meta.url);
const read = path => readFile(new URL(path, runtime), 'utf8');

test('CUA declarations preserve the captured installed provider contract', async () => {
  const { tools } = JSON.parse(await read('src/cua_provider_tools.json'));
  const js = tools.find(tool => tool.name === 'js');
  const reset = tools.find(tool => tool.name === 'js_reset');
  assert.equal(CUA_DESCRIPTION, js.description);
  assert.deepEqual(CUA_PARAMETERS, js.inputSchema);
  assert.equal(CUA_RESET_DESCRIPTION, reset.description);
  assert.deepEqual(CUA_RESET_PARAMETERS, reset.inputSchema);
  assert.equal(await read('src/cua_tool_description.md'), js.description);
  assert.equal(await read('../src/description.md'), js.description);
  assert.equal(await read('src/cua_reset_description.md'), reset.description);
  assert.equal(await read('../src/reset_description.md'), reset.description);
  assert.ok(CUA_DESCRIPTION.includes('await cua.rewriteDocumentation()'));
  assert.ok(!CUA_DESCRIPTION.includes('cua.getScreenshot()'));
  assert.ok(!CUA_DESCRIPTION.includes('cua.listWindows'));
});

test('provider documentation teaches target-bound screenshots', async () => {
  const context = vm.createContext({});
  vm.runInContext(await read('src/cua_docs.js'), context);
  const docs = context.__skyreDocumentation;
  assert.deepEqual(Object.keys(docs).sort(), ['confirmations', 'core-cua-repl', 'core-node-repl', 'other-browser-apis']);
  assert.match(docs['core-cua-repl'], /getScreenshot\(options\?: ObservationOptions\)/);
  assert.match(docs['core-cua-repl'], /errors\?: string\[\]/);
  assert.ok(!docs['core-cua-repl'].includes('cua.getScreenshot()'));
});
