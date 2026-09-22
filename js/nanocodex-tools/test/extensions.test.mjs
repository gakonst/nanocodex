import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileMemoriesBackend, extensionTools, extensionSpecs, truncateMemoryText } from '../tools/extensions.mjs';
const context = { signal: new AbortController().signal };
function backend(initial = {}) {
  const files = new Map(Object.entries(initial));
  return { files, tools: fileMemoriesBackend({ listFiles: async () => [...files.keys()], readFile: async path => {
    if (!files.has(path)) throw new Error('not found'); return files.get(path);
  }, createFile: async (path, value) => { if (files.has(path)) throw new Error('already exists'); files.set(path,value); } }) };
}
test('native and npm packages carry identical consumed memory definitions', async () => {
  const { readFile } = await import('node:fs/promises');
  const native = JSON.parse(await readFile(new URL('../../../crates/nanocodex-tools/src/extensions/specs.json', import.meta.url)));
  assert.deepEqual(extensionSpecs, native);
});
test('only the four memory tools are declared', () => {
  assert.deepEqual(extensionSpecs.map(s => s.name).sort(), ['memories__list','memories__search','memories__read','memories__add_ad_hoc_note'].sort());
});
test('list immediate directories, file targets and pagination', async () => {
  const { tools } = backend({ 'a.md':'alpha','dir/b.md':'beta','dir/c.md':'gamma' });
  const first = await tools.list({ max_results: 1 });
  assert.deepEqual(first.entries,[{path:'a.md',entry_type:'file'}]);
  assert.equal(first.next_cursor,'1');
  assert.deepEqual((await tools.list({cursor:first.next_cursor})).entries,[{path:'dir',entry_type:'directory'}]);
  assert.equal((await tools.list({path:'dir'})).entries.length,2);
  assert.equal((await tools.list({ max_results: 0 })).entries.length, 1);
  assert.equal((await tools.list({ max_results: 2 ** 53 })).entries.length, 2);
  assert.equal((await tools.list({path:'a.md'})).entries.length,1);
  await assert.rejects(tools.list({cursor:'9'}),/cursor/);
  await assert.rejects(tools.list({path:'../secret'}),/outside/);
  await assert.rejects(tools.list({path:'.hidden'}),/hidden/);
});
test('memory read preserves newlines and uses one-indexed offsets', async () => {
  const { tools } = backend({ 'a.md':'one\r\ntwo\nthree\n' });
  assert.deepEqual(await tools.read({path:'a.md',line_offset:2,max_lines:1}),{path:'a.md', start_line_number:2,content:'two\n',truncated:true});
  assert.equal((await tools.read({path:'a.md',line_offset:4})).content,'');
  await assert.rejects(tools.read({path:'a.md',line_offset:5}),/exceeds/);
  await assert.rejects(tools.read({path:'a.md',line_offset:0}),/positive/);
});
test('search case, separators, minimal windows, context, pagination and empty-query rejection', async () => {
  const { tools } = backend({ 'a.md':'intro\nALPHA-beta\nother\ngamma\nalpha beta gamma\nend\n' });
  assert.equal((await tools.search({queries:['alphabeta']})).matches.length,0);
  assert.equal((await tools.search({queries:['alpha_beta'],normalized:true,case_sensitive:false})).matches.length,2);
  const result = await tools.search({queries:['alpha','gamma'], case_sensitive:false, match_mode:{type:'all_within_lines',line_count:3},context_lines:1});
  assert.deepEqual(result.matches.map(m=>m.match_line_number),[2,5]);
  assert.equal(result.matches[0].content_start_line_number,1);
  const same = await tools.search({queries:['alpha','gamma'],match_mode:{type:'all_on_same_line'}});
  assert.deepEqual(same.matches.map(m=>m.match_line_number),[5]);
  const page = await tools.search({queries:['alpha'],case_sensitive:false,max_results:1});
  assert.equal(page.next_cursor,'1');
  await assert.rejects(tools.search({queries:['---'],normalized:true}),/queries/);
});
test('append-only verbatim notes, filename constraints, duplicate protection', async () => {
  const { tools, files } = backend();
  const input = {filename:'2026-09-19T10-30-00-test.md',note:'# Remember\nline\n'};
  assert.deepEqual(await tools.add_ad_hoc_note(input),{});
  assert.equal(files.get('extensions/ad_hoc/notes/'+input.filename),input.note);
  await assert.rejects(tools.add_ad_hoc_note(input),/already exists/);
  await assert.rejects(tools.add_ad_hoc_note({...input,filename:'../escape.md'}),/filename/);
  await assert.rejects(tools.add_ad_hoc_note({...input,note:'  '}),/empty/);
});
test('provider gating and authorization precede reads and writes', async () => {
  assert.deepEqual(extensionTools({authorize(){}}),[]);
  const {tools:memories,files} = backend();
  let allowed = false;
  const tools = extensionTools({memories,authorize:()=>{if(!allowed)throw new Error('forbidden');}});
  assert.equal(tools.length,4);
  const note = tools.find(t=>t.name==='memories__add_ad_hoc_note');
  const input = {filename:'2026-09-19T10-30-00-test.md',note:'secret'};
  await assert.rejects(note.handler(input,context),/forbidden/);
  assert.equal(files.size,0);
  allowed = true;
  await note.handler(input,context);
  assert.equal(files.size,1);
});
test('middle truncation matches pinned approximate token budget',()=>{
  assert.equal(truncateMemoryText('abcdefghijklmnop',2),'abcd…2 tokens truncated…mnop');
  assert.equal(truncateMemoryText('αβγδεζηθ',2),'αβ…2 tokens truncated…ηθ');
});
