// Public facade regression; no desktop permissions required.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const calls = [];
const context = vm.createContext({TextEncoder});
vm.runInContext(fs.readFileSync(new URL('../../../crates/experimental/nanocodex-computer/runtime/src/facade.js', import.meta.url),'utf8'), context);
const computer = {
  target:'mac',
  async list_app_windows(input) { calls.push(['windows',input]); return [{windowId:41},{windowId:42}]; },
  async get_app_state(input) { calls.push(['state',input]); return {app:input.app,text:'owned'}; },
  async type_text(input) { calls.push(['type',input]); },
  async click(input) { calls.push(['click',input]); },
};
const cua = await context.__skyreCreateCUA({computer});
assert.equal((await cua.listWindows('org.owned')).length,2);
const [first, second] = await Promise.all([cua.getApp('org.owned',{windowId:41}),cua.getApp('org.owned',{windowId:42})]);
await first.typeText('first');
await second.typeText('second');
await first.getAXState({emit:false});
assert.equal(calls[3][1].app,'org.owned#window=41');
assert.equal(calls[4][1].app,'org.owned#window=42');
assert.equal(calls[5][1].app,'org.owned#window=41');
await first.click(7);
await second.click(7);
assert.equal(calls[6][1].app,'org.owned#window=41');
assert.equal(calls[7][1].app,'org.owned#window=42');
assert.equal(calls[6][1].element_index,7);
assert.equal(calls[7][1].element_index,7);
for (const windowId of [0,-1,1.5,4294967296,'41',null]) {
  await assert.rejects(cua.getApp('org.owned',{windowId}),/positive u32/);
}
const plain = await context.__skyreCreateCUA({computer:{target:'mac',get_app_state:computer.get_app_state}});
assert.equal('listWindows' in plain,false);
// The approval boundary must preserve exact window identity after canonicalizing app identity.
vm.runInContext(fs.readFileSync(new URL('../../../crates/experimental/nanocodex-computer/runtime/src/sky_facade.js', import.meta.url),'utf8'), context);
const requests=[];
const approved = context.__skyreComputerFacade({bytes:x=>x, setupResult:{value:{target:'mac',methods:['get_app_state','list_app_windows']}},
  getNodeRepl:()=>({createElicitation:async()=>({action:'accept'}),withSuspendedTimeout:f=>f()}),
  rpc:async(method,args)=>{
    requests.push([method,args]);
    if(method==='sky.app_policy') return {decision:'allowed',target:{bundleIdentifier:'org.owned',displayName:'Owned',appPath:'/Applications/Owned.app',bindingIdentifier:'/Applications/Owned.app#window=41'}};
    return args;
  }});
await approved.get_app_state({app:'org.owned#window=41',screenshot:false});
assert.equal(requests[1][1].args[0].app,'/Applications/Owned.app#window=41');
console.log('native window facade bindings: passed');
