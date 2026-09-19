import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileMemoriesBackend, skillsBackend, extensionTools, extensionSpecs, truncateMemoryText } from '../tools/extensions.mjs';
import { nodeMemoriesBackend } from '../tools/node-extensions.mjs';
const context = { signal: new AbortController().signal };
function backend(initial = {}) {
  const files = new Map(Object.entries(initial));
  return { files, tools: fileMemoriesBackend({ listFiles: async () => [...files.keys()], readFile: async path => {
    if (!files.has(path)) throw new Error('not found'); return files.get(path);
  }, createFile: async (path, value) => { if (files.has(path)) throw new Error('already exists'); files.set(path,value); } }) };
}
test('pinned native and JS declarations stay identical', async () => {
  const { readFile } = await import('node:fs/promises');
  const native = JSON.parse(await readFile(new URL('../../../crates/nanocodex-tools/src/extensions/specs.json', import.meta.url)));
  assert.deepEqual(extensionSpecs,native);
  assert.deepEqual(extensionSpecs.map(s => s.name).sort(), ['skills__list','skills__read','memories__list','memories__search','memories__read','memories__add_ad_hoc_note','get_goal','create_goal','update_goal'].sort());
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
test('skill paging preserves Unicode snapshots and rejects cross-package cursors', async () => {
  let content='😀'.repeat(100), reads=0;
  const tools = skillsBackend({list:async()=>({skills:[]}),read:async input=>{reads++;return {resource:'skill://a/SKILL.md',contents:content};}},150);
  const first = await tools.read({package:'a'});
  assert.ok(first.next_cursor);
  content='changed';
  let result=first.contents, cursor=first.next_cursor;
  while(cursor){const next=await tools.read({package:'a',cursor});result+=next.contents;cursor=next.next_cursor;}
  assert.equal(result,'😀'.repeat(100));
  assert.equal(reads,1);
  await assert.rejects(tools.read({package:'b',cursor:first.next_cursor}),/stale/);
});
test('node adapter persists notes across instances and rejects symlinks', async () => {
  const root=await mkdtemp(join(tmpdir(),'extension-memory-'));
  try {
    await writeFile(join(root,'existing.md'),'alpha\nbeta');
    await mkdir(join(root,'sub'));
    await symlink(join(root,'existing.md'),join(root,'sub','link.md'));
    const tools=nodeMemoriesBackend(root);
    assert.equal((await tools.list({path:'sub'})).entries.length,0);
    await assert.rejects(tools.read({path:'sub/link.md'}),/symlink/);
    const input={filename:'2026-09-19T10-30-00-test.md',note:'durable'};
    await tools.add_ad_hoc_note(input);
    const second=nodeMemoriesBackend(root);
    assert.equal((await second.read({path:'extensions/ad_hoc/notes/'+input.filename})).content,'durable');
    await assert.rejects(second.add_ad_hoc_note(input),/EEXIST/);
  } finally { await rm(root,{recursive:true,force:true}); }
});
test('middle truncation matches pinned approximate token budget',()=>{
  assert.equal(truncateMemoryText('abcdefghijklmnop',2),'abcd…2 tokens truncated…mnop');
  assert.equal(truncateMemoryText('αβγδεζηθ',2),'αβ…2 tokens truncated…ηθ');
});
