// Installed public facade counterexample; inert providers, no desktop permissions.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const calls = [];
const context = vm.createContext({TextEncoder});
vm.runInContext(fs.readFileSync(new URL('../../../crates/experimental/nanocodex-computer/runtime/src/facade.js', import.meta.url),'utf8'), context);
const computer = {
  target:'mac',
  async list_app_windows() { throw Error('custom discovery must not be called'); },
  async get_desktop_screenshot() { throw Error('custom capture must not be called'); },
  async get_app_state(input) { calls.push(['state',input]); return {app:'org.canonical',text:'owned'}; },
  async type_text(input) { calls.push(['type',input]); },
  async click(input) { calls.push(['click',input]); },
  async drag(input) { calls.push(['drag',input]); },
};
const cua = await context.__skyreCreateCUA({computer});
assert.equal('listWindows' in cua,false);
assert.equal('getScreenshot' in cua,false);
const app = await cua.getApp('org.owned',{
  get windowId() { throw Error('unsupported getApp options must not be read'); },
});
await app.typeText('owned text');
await app.getAXState({emit:false});
await app.click(7);
await app.drag([20,30],[80,90],{
  get mouseButton() { throw Error('unsupported drag options must not be read'); },
  get modifiers() { throw Error('unsupported drag options must not be read'); },
});
assert.deepEqual(JSON.parse(JSON.stringify(calls)),[
  ['state',{app:'org.owned',disableDiff:true}],
  ['type',{app:'org.canonical',text:'owned text'}],
  ['state',{app:'org.canonical'}],
  ['click',{app:'org.canonical',element_index:7}],
  ['drag',{app:'org.canonical',from_x:20,from_y:30,to_x:80,to_y:90}],
]);
for (const windowId of [0,-1,1.5,4294967296,'41',null]) {
  await cua.getApp('org.owned',{windowId});
  assert.deepEqual(JSON.parse(JSON.stringify(calls.at(-1))),['state',{app:'org.owned',disableDiff:true}]);
}
// The trusted approval boundary still preserves an explicit backend binding;
// this does not add a public getApp window option or advertise window discovery.
vm.runInContext(fs.readFileSync(new URL('../../../crates/experimental/nanocodex-computer/runtime/src/sky_facade.js', import.meta.url),'utf8'), context);
const requests=[];
const approved = context.__skyreComputerFacade({bytes:x=>x, setupResult:{value:{target:'mac',methods:['get_app_state']}},
  getNodeRepl:()=>({createElicitation:async()=>({action:'accept'}),withSuspendedTimeout:f=>f()}),
  rpc:async(method,args)=>{
    requests.push([method,args]);
    if(method==='sky.app_policy') return {decision:'allowed',target:{bundleIdentifier:'org.owned',displayName:'Owned',appPath:'/Applications/Owned.app',bindingIdentifier:'/Applications/Owned.app#window=41'}};
    return args;
  }});
assert.equal('list_app_windows' in approved,false);
assert.equal('get_desktop_screenshot' in approved,false);
await approved.get_app_state({app:'org.owned#window=41',screenshot:false});
assert.equal(requests[1][1].args[0].app,'/Applications/Owned.app#window=41');
console.log('canonical native facade and trusted binding boundary: passed');
