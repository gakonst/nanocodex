import test from 'node:test';
import assert from 'node:assert/strict';
import { executeSkyRequest } from '../../src/linux_sky_text.mjs';
const window = {id:42,app:'fixture'};
const request = text => ({type:'execute',method:'type_text',args:[{window,text}]});
const node = (id,states=['focused','editable','enabled']) => ({id,states,children:[]});
const state = children => ({window,ax_tree_source:'at_spi',ax_tree:{id:'0',children}});

test('target current editable control without changing text or selection commands',async()=>{
  const calls=[]; const text='Unicode Ω 🙂\nwith\ttabs';
  const rpc=async r=>{calls.push(r); return r.method==='get_window_state'?state([node('7')]):'typed'};
  assert.equal(await executeSkyRequest(rpc,request(text)),'typed');
  assert.deepEqual(calls,[{type:'execute',method:'get_window_state',args:[{window,include_screenshot:false}]},
    {type:'execute',method:'type_text',args:[{window,text,element_id:'7'}]}]);
});
test('refresh focus between calls instead of reusing a previous click',async()=>{
  let current='3'; const targets=[];
  const rpc=async r=>r.method==='get_window_state'?state([node(current)]):targets.push(r.args[0].element_id);
  await executeSkyRequest(rpc,request('first')); current='9'; await executeSkyRequest(rpc,request('second'));
  assert.deepEqual(targets,['3','9']);
});
test('preserve native behavior when an editable target cannot be identified safely',async()=>{
  for(const snapshot of [state([]),state([node('2'),node('3')]),state([node('2',['editable','enabled'])]),
    state([node('2',['focused','editable','enabled','defunct'])]),
    {...state([node('2')]),window:{id:99}}, {...state([node('2')]),ax_tree_source:'x11'}]){
    const calls=[]; const r=request('x'); await executeSkyRequest(async q=>{calls.push(q);return snapshot},r);
    assert.strictEqual(calls.at(-1),r);
  }
});
test('explicit element and desktop requests pass through without observing another target',async()=>{
  for(const r of [{type:'execute',method:'type_text',args:[{text:'x'}]},
    {type:'execute',method:'type_text',args:[{window,text:'x',element_id:'4'}]},
    {type:'execute',method:'press_key',args:[{window,key:'Return'}]}]){
    const calls=[];await executeSkyRequest(async q=>calls.push(q),r);assert.deepEqual(calls,[r]);
  }
});
test('never retry text after an ambiguous native failure',async()=>{
  let writes=0;
  await assert.rejects(executeSkyRequest(async r=>{
    if(r.method==='get_window_state')return state([node('2')]);
    writes++;throw Error('native disconnected after dispatch');
  },request('one copy')),/native disconnected/);
  assert.equal(writes,1);
});
test('observation failure prevents dispatch rather than risking the wrong control',async()=>{
  let writes=0;
  await assert.rejects(executeSkyRequest(async r=>{
    if(r.method==='get_window_state')throw Error('window closed');
    writes++;
  },request('x')),/window closed/);
  assert.equal(writes,0);
});
