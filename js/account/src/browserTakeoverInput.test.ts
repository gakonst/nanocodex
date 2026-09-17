import assert from 'node:assert/strict';
import { test } from 'node:test';
import { enqueueTakeover, imagePoint, textEdits } from './browserTakeoverInput.ts';
import { browserTakeover, type BrowserTakeoverAction, type VaultIntake } from './vaultIntake.ts';
test('busy queues preserve gesture boundaries and text, coalescing only adjacent moves', () => {
 const queue: BrowserTakeoverAction[] = [];
 const actions: BrowserTakeoverAction[] = [{action:'touch',phase:'start',x:0,y:0},{action:'touch',phase:'move',x:.1,y:.1},{action:'touch',phase:'move',x:.2,y:.2},{action:'edit',delete_backward:0,text:'密碼'},{action:'touch',phase:'move',x:.3,y:.3},{action:'touch',phase:'end',x:.4,y:.4},{action:'finish'}];
 actions.forEach(action => enqueueTakeover(queue,action)); assert.deepEqual(queue, actions.filter((_,i)=>i!==1));
});
test('edits count graphemes and split long paste without splitting surrogate pairs', () => {
 assert.deepEqual(textEdits('a👨‍👩‍👧‍👦é','a'), [{action:'edit',delete_backward:2,text:''}]);
 const edits = textEdits('', '😀'.repeat(600)); assert.equal(edits.map(a=>a.text).join(''), '😀'.repeat(600)); assert.ok(edits.every(a=>a.text!.length<=512));
 assert.deepEqual(textEdits('仮','漢字'), [{action:'edit',delete_backward:1,text:''},{action:'edit',delete_backward:0,text:'漢字'}]);
 assert.deepEqual(textEdits('x'.repeat(300),'' ).map(a=>a.delete_backward),[128,128,44]);
});
test('coordinates use actual image rectangle with letterbox offsets and clamp captured drags',()=>{
 assert.deepEqual(imagePoint({left:100,top:50,width:400,height:800},300,450),{x:.5,y:.5});
 assert.deepEqual(imagePoint({left:100,top:50,width:400,height:800},0,999),{x:0,y:1});
});
test('legacy and safe input frames parse, private fields and invalid rectangles fail closed',async()=>{
 const intake: VaultIntake={operation:'browser_takeover',kind:'login',agent_id:'agent',challenge_id:'a'.repeat(22)};
 const frame={status:'active',image:'data:image/png;base64,YQ==',width:390,height:700};
 const hint={type:'email',multiline:false}; const region={...hint,x:.1,y:.2,width:.8,height:.1};
 for(const extra of [{},{keyboard:hint,inputs:[region]}]) assert.deepEqual(await browserTakeover(intake,{action:'observe',viewport:{width:390,height:700,mobile:true}},async(_url,options)=>{assert.deepEqual(JSON.parse(options!.body as string).viewport,{width:390,height:700,mobile:true});return Response.json({...frame,...extra});}),{...frame,...extra});
 for(const extra of [{keyboard:{...hint,value:'private'}},{inputs:[{...region,x:.9}]},{inputs:Array(33).fill(region)},{inputs:[{...region,height:0}]},{keyboard:{type:'unknown',multiline:false}}]) await assert.rejects(browserTakeover(intake,{action:'observe'},async()=>Response.json({...frame,...extra})));
 let calls=0; await assert.rejects(browserTakeover(intake,{action:'edit',text:'x',delete_backward:0},async()=>{calls++;throw Error('uncertain');}));assert.equal(calls,1);
});
